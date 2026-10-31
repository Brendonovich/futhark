//! A client of `futharkd`. Calls are synchronous and in order, one at a time per connection. `watch` takes over its
//! connection, so open another `Client` for calls while watching.

use std::{
    fs::OpenOptions,
    io::{self, BufRead, BufReader, Write},
    os::unix::{net::UnixStream, process::CommandExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use futhark_protocol::{
    CreateParams, Edit, EditParams, Edited, Event, Hello, Method, Notification, OpType, OpenParams, Opened, Request,
    Response, RpcError, Sample, SampleParams, Snapshot, Status, socket_path, state_dir,
};
use serde::de::DeserializeOwned;

/// How long `connect_or_launch` waits for a server it started.
const LAUNCH_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("can't reach futharkd at {}: {source}", path.display())]
    Connect { path: PathBuf, source: io::Error },
    #[error("{0}")]
    Io(#[from] io::Error),
    #[error("{0}")]
    Rpc(#[from] RpcError),
    #[error("unexpected reply from futharkd: {0}")]
    Protocol(String),
    #[error("couldn't start futharkd: {0}")]
    Launch(String),
}

pub type Result<T, E = ClientError> = std::result::Result<T, E>;

pub struct Client {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
    next_id: u64,
}

impl Client {
    pub fn connect(path: &Path) -> Result<Self> {
        let stream = UnixStream::connect(path).map_err(|source| ClientError::Connect {
            path: path.into(),
            source,
        })?;
        Ok(Client {
            reader: BufReader::new(stream.try_clone()?),
            writer: stream,
            next_id: 1,
        })
    }

    /// Connects to the local server at [`socket_path`].
    pub fn connect_local() -> Result<Self> {
        Self::connect(&socket_path())
    }

    /// Connects to the local server, starting it if nothing answers.
    pub fn connect_or_launch() -> Result<Self> {
        Self::connect_or_launch_at(&socket_path())
    }

    pub fn connect_or_launch_at(path: &Path) -> Result<Self> {
        if let Ok(client) = Self::connect(path) {
            return Ok(client);
        }
        launch(path)?;
        let deadline = Instant::now() + LAUNCH_TIMEOUT;
        loop {
            match Self::connect(path) {
                Ok(client) => return Ok(client),
                Err(error) if Instant::now() > deadline => return Err(error),
                Err(_) => thread::sleep(Duration::from_millis(25)),
            }
        }
    }

    /// Calls a method and decodes its result.
    pub fn call<T: DeserializeOwned>(&mut self, method: Method) -> Result<T> {
        let value = self.call_value(method)?;
        serde_json::from_value(value).map_err(|e| ClientError::Protocol(e.to_string()))
    }

    /// Calls a method and returns its result undecoded.
    pub fn call_value(&mut self, method: Method) -> Result<serde_json::Value> {
        let id = self.next_id;
        self.next_id += 1;
        let mut line = serde_json::to_string(&Request::new(id, method)).expect("requests serialize");
        line.push('\n');
        self.writer.write_all(line.as_bytes())?;

        let response: Response = self.read_line()?;
        if response.id != Some(id) {
            return Err(ClientError::Protocol(format!(
                "expected the reply to request {id}, got {:?}",
                response.id
            )));
        }
        match (response.result, response.error) {
            (_, Some(error)) => Err(error.into()),
            (Some(result), None) => Ok(result),
            (None, None) => Ok(serde_json::Value::Null),
        }
    }

    fn read_line<T: DeserializeOwned>(&mut self) -> Result<T> {
        let mut line = String::new();
        if self.reader.read_line(&mut line)? == 0 {
            return Err(ClientError::Protocol("futharkd closed the connection".into()));
        }
        serde_json::from_str(&line).map_err(|e| ClientError::Protocol(format!("{e}: {}", line.trim_end())))
    }

    pub fn hello(&mut self) -> Result<Hello> {
        self.call(Method::Hello)
    }

    pub fn status(&mut self) -> Result<Status> {
        self.call(Method::Status)
    }

    pub fn create(&mut self, path: &Path, name: Option<String>) -> Result<Opened> {
        self.call(Method::Create(CreateParams {
            path: absolute(path)?,
            name,
        }))
    }

    pub fn open(&mut self, path: &Path) -> Result<Opened> {
        self.call(Method::Open(OpenParams { path: absolute(path)? }))
    }

    pub fn save(&mut self) -> Result<()> {
        self.call(Method::Save)
    }

    pub fn close(&mut self) -> Result<()> {
        self.call(Method::Close)
    }

    pub fn types(&mut self) -> Result<Vec<OpType>> {
        self.call(Method::Types)
    }

    pub fn get(&mut self) -> Result<Snapshot> {
        self.call(Method::Get)
    }

    /// Applies edits all or nothing. With `revision`, fails unless the network is still at it.
    pub fn edit(&mut self, revision: Option<u64>, edits: Vec<Edit>) -> Result<Edited> {
        self.call(Method::Edit(EditParams { revision, edits }))
    }

    pub fn sample(&mut self, path: &str) -> Result<Sample> {
        self.call(Method::Sample(SampleParams { path: path.into() }))
    }

    pub fn shutdown(&mut self) -> Result<()> {
        self.call(Method::Shutdown)
    }

    /// Turns this connection into an event stream. Returns the status at the time it started.
    pub fn watch(mut self) -> Result<(Status, Events)> {
        let status = self.call(Method::Watch)?;
        Ok((status, Events { client: self }))
    }
}

/// Events from `watch`. Ends when the server closes the connection.
pub struct Events {
    client: Client,
}

impl Iterator for Events {
    type Item = Result<Event>;

    fn next(&mut self) -> Option<Self::Item> {
        match self.client.read_line::<Notification>() {
            Ok(notification) => Some(Ok(notification.params)),
            Err(ClientError::Protocol(message)) if message == "futharkd closed the connection" => None,
            Err(error) => Some(Err(error)),
        }
    }
}

fn absolute(path: &Path) -> Result<String> {
    Ok(std::path::absolute(path)?.to_string_lossy().into_owned())
}

/// The server binary: `$FUTHARK_DAEMON`, otherwise `futharkd` next to the running program.
fn daemon_path() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("FUTHARK_DAEMON") {
        return Ok(path.into());
    }
    let exe = std::env::current_exe()?;
    let dir = exe
        .parent()
        .ok_or_else(|| ClientError::Launch("the program has no directory".into()))?;
    // Test binaries live one level down, in target/<profile>/deps.
    [dir.join("futharkd"), dir.join("../futharkd")]
        .into_iter()
        .find(|path| path.exists())
        .ok_or_else(|| ClientError::Launch(format!("no futharkd next to {}; set FUTHARK_DAEMON", exe.display())))
}

/// Starts the server detached, in its own process group, logging to `futharkd.log` in the state directory.
fn launch(socket: &Path) -> Result<()> {
    let daemon = daemon_path()?;
    let dir = state_dir();
    std::fs::create_dir_all(&dir)?;
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("futharkd.log"))?;
    let mut child = Command::new(&daemon)
        .arg("--socket")
        .arg(socket)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .process_group(0)
        .spawn()
        .map_err(|e| ClientError::Launch(format!("{}: {e}", daemon.display())))?;
    // Reaps the server if it exits while this process is still running, so it doesn't linger as a zombie.
    thread::spawn(move || child.wait());
    Ok(())
}
