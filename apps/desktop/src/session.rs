//! The app's link to `futharkd`. One thread owns the calls connection and works through commands in order; another
//! watches for events on its own connection and asks for a fresh snapshot when the network changes. Replies come
//! back over a channel, and each one wakes the UI.

use std::{
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, Sender},
    thread,
};

use futhark_client::{Client, ClientError};
use futhark_protocol::{Edit, Event, Hello, OpType, RpcError, Snapshot};

pub type Waker = std::sync::Arc<dyn Fn() + Send + Sync>;

#[derive(Debug)]
pub enum Command {
    Open(PathBuf),
    /// Applies edits made against the snapshot at revision `base`.
    Edit {
        base: u64,
        edits: Vec<Edit>,
    },
    Save,
    Refresh,
}

#[derive(Debug)]
pub enum Reply {
    Connected {
        hello: Hello,
        types: Vec<OpType>,
    },
    Snapshot(Snapshot),
    /// The server has no project open.
    NoProject,
    /// An `Edit` command finished, applied or not. Its snapshot follows.
    EditDone,
    Saved,
    Error(String),
    /// The server went away; nothing more will come.
    Disconnected(String),
}

pub struct Session {
    commands: Sender<Command>,
    replies: Receiver<Reply>,
}

impl Session {
    /// Connects to the server at `socket` (starting it if needed) on a thread of its own, opening `open` if given.
    pub fn start(socket: &Path, open: Option<PathBuf>, wake: Waker) -> Session {
        let socket = socket.to_path_buf();
        let (commands, inbox) = mpsc::channel();
        let (outbox, replies) = mpsc::channel();
        let to_self = commands.clone();
        thread::Builder::new()
            .name("futhark-session".into())
            .spawn(move || {
                let out = Outbox { tx: outbox, wake };
                match Client::connect_or_launch_at(&socket) {
                    Ok(client) => Worker::new(client, out, inbox, to_self).run(&socket, open),
                    Err(error) => out.send(Reply::Disconnected(error.to_string())),
                }
            })
            .expect("spawn the session thread");
        Session { commands, replies }
    }

    pub fn send(&self, command: Command) {
        let _ = self.commands.send(command);
    }

    pub fn drain(&self) -> Vec<Reply> {
        self.replies.try_iter().collect()
    }
}

#[derive(Clone)]
struct Outbox {
    tx: Sender<Reply>,
    wake: Waker,
}

impl Outbox {
    fn send(&self, reply: Reply) {
        if self.tx.send(reply).is_ok() {
            (self.wake)();
        }
    }
}

struct Worker {
    client: Client,
    out: Outbox,
    inbox: Receiver<Command>,
    to_self: Sender<Command>,
    revisions: Revisions,
    backlog: VecDeque<Command>,
}

impl Worker {
    fn new(client: Client, out: Outbox, inbox: Receiver<Command>, to_self: Sender<Command>) -> Worker {
        Worker {
            client,
            out,
            inbox,
            to_self,
            revisions: Revisions::default(),
            backlog: VecDeque::new(),
        }
    }

    fn run(mut self, socket: &Path, open: Option<PathBuf>) {
        let connected = (|| -> Result<Reply, ClientError> {
            Ok(Reply::Connected {
                hello: self.client.hello()?,
                types: self.client.types()?,
            })
        })();
        match connected {
            Ok(reply) => self.out.send(reply),
            Err(error) => return self.out.send(Reply::Disconnected(error.to_string())),
        }
        watch(socket.to_path_buf(), self.to_self.clone(), self.out.clone());
        match open {
            Some(path) => self.handle(Command::Open(path)),
            None => self.refresh(),
        }
        while let Some(command) = self.backlog.pop_front().or_else(|| self.inbox.recv().ok()) {
            self.handle(command);
        }
    }

    fn handle(&mut self, command: Command) {
        match command {
            Command::Open(path) => {
                if let Err(error) = self.client.open(&path) {
                    self.error(error);
                }
                self.refresh();
            }
            Command::Edit { base, edits } => {
                let base = self.revisions.rebase(base);
                match self.client.edit(Some(base), edits) {
                    Ok(edited) => self.revisions.record(base, edited.revision),
                    Err(ClientError::Rpc(error)) if error.code == RpcError::WRONG_STATE => self.out.send(Reply::Error(
                        "The network changed before that edit arrived, so it wasn't applied. Try again.".into(),
                    )),
                    Err(error) => self.error(error),
                }
                self.out.send(Reply::EditDone);
                self.refresh();
            }
            Command::Save => match self.client.save() {
                Ok(()) => self.out.send(Reply::Saved),
                Err(error) => self.error(error),
            },
            Command::Refresh => self.refresh(),
        }
    }

    /// Sends a fresh snapshot. Refreshes already queued behind this one are folded into it.
    fn refresh(&mut self) {
        for command in self.inbox.try_iter() {
            if !matches!(command, Command::Refresh) {
                self.backlog.push_back(command);
            }
        }
        match self.client.get() {
            Ok(snapshot) => self.out.send(Reply::Snapshot(snapshot)),
            Err(ClientError::Rpc(error)) if error.code == RpcError::WRONG_STATE => self.out.send(Reply::NoProject),
            Err(error) => self.error(error),
        }
    }

    fn error(&self, error: ClientError) {
        let reply = match error {
            ClientError::Rpc(error) => Reply::Error(error.message),
            other @ (ClientError::Io(_) | ClientError::Protocol(_)) => Reply::Disconnected(other.to_string()),
            other => Reply::Error(other.to_string()),
        };
        self.out.send(reply);
    }
}

/// Follows the server's events on a connection of its own, asking for a snapshot whenever one may have changed.
fn watch(socket: PathBuf, commands: Sender<Command>, out: Outbox) {
    thread::Builder::new()
        .name("futhark-watch".into())
        .spawn(move || {
            let events = match Client::connect(&socket).and_then(Client::watch) {
                Ok((_, events)) => events,
                Err(error) => return out.send(Reply::Error(format!("Not watching for changes: {error}"))),
            };
            for event in events {
                match event {
                    Ok(Event::NetworkChanged { .. } | Event::ProjectOpened { .. } | Event::ProjectClosed) => {
                        if commands.send(Command::Refresh).is_err() {
                            return;
                        }
                    }
                    Ok(Event::ProjectSaved { .. }) => {}
                    Err(error) => return out.send(Reply::Disconnected(error.to_string())),
                }
            }
            out.send(Reply::Disconnected("futharkd stopped".into()));
        })
        .expect("spawn the watch thread");
}

/// The revisions this app's own edits produced. The UI edits against the last snapshot it has, which may predate
/// edits of its own still on their way back; those can't conflict with it, so an edit is sent against the revision
/// they led to.
#[derive(Default)]
pub struct Revisions {
    next: HashMap<u64, u64>,
}

impl Revisions {
    const KEEP: usize = 256;

    pub fn rebase(&self, mut base: u64) -> u64 {
        while let Some(&next) = self.next.get(&base) {
            base = next;
        }
        base
    }

    pub fn record(&mut self, from: u64, to: u64) {
        if self.next.len() >= Self::KEEP {
            self.next.retain(|&k, _| k + Self::KEEP as u64 > from);
        }
        self.next.insert(from, to);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_build_on_the_apps_own_edits() {
        let mut revisions = Revisions::default();
        assert_eq!(revisions.rebase(3), 3);
        revisions.record(3, 4);
        revisions.record(4, 5);
        // Two edits made against revision 3 before either came back go out against 4, then 5.
        assert_eq!(revisions.rebase(3), 5);
        assert_eq!(revisions.rebase(4), 5);
        // Someone else's edit (5 to 6) isn't ours, so an edit against 5 stays there and is refused.
        assert_eq!(revisions.rebase(6), 6);
    }

    #[test]
    fn old_revisions_are_forgotten() {
        let mut revisions = Revisions::default();
        for r in 0..1000 {
            revisions.record(r, r + 1);
        }
        assert!(revisions.next.len() <= Revisions::KEEP);
        assert_eq!(revisions.rebase(990), 1000);
    }
}
