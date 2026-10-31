//! The app against a real server: `futharkd` runs in process on a private socket, and the app connects, opens the
//! `hello` example, edits it, and sees previews and the viewer fill in.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, mpsc},
    thread,
    time::{Duration, Instant},
};

use futhark_client::Client;
use futhark_protocol::{Edit, Family};

use crate::app::{App, Connection};
use crate::previews::{Preview, StaticPreviews};
use crate::viewer::Viewer;

struct Server {
    socket: PathBuf,
    dir: PathBuf,
    thread: Option<thread::JoinHandle<()>>,
}

impl Server {
    fn start(name: &str) -> Server {
        let dir = std::env::temp_dir().join(format!("futhark-desktop-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("s.sock");
        let options = futhark_server::Options {
            socket: socket.clone(),
            project: None,
            exit_on_stdin_close: false,
        };
        let thread = thread::spawn(move || futhark_server::run(options).unwrap());
        let deadline = Instant::now() + Duration::from_secs(5);
        while Client::connect(&socket).is_err() {
            assert!(Instant::now() < deadline, "futharkd didn't start");
            thread::sleep(Duration::from_millis(10));
        }
        Server {
            socket,
            dir,
            thread: Some(thread),
        }
    }

    fn example(&self, name: &str) -> PathBuf {
        let to = self.dir.join(name);
        copy(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples").join(name),
            &to,
        );
        to
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Ok(mut client) = Client::connect(&self.socket) {
            let _ = client.shutdown();
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn copy(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let path = entry.unwrap().path();
        let target = to.join(path.file_name().unwrap());
        if path.is_dir() {
            copy(&path, &target)
        } else {
            fs::copy(&path, &target).map(drop).unwrap()
        }
    }
}

/// Polls the app as its window would, each time something wakes it, until `done` holds.
fn until(app: &mut App, wakes: &mpsc::Receiver<()>, what: &str, done: impl Fn(&App) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        app.poll();
        if done(app) {
            return;
        }
        let left = deadline.saturating_duration_since(Instant::now());
        assert!(!left.is_zero(), "timed out waiting for {what}");
        let _ = wakes.recv_timeout(left.min(Duration::from_millis(100)));
    }
}

/// An app connected to `server` with `example` open, and what wakes it.
fn connect(server: &Server, example: &str) -> (App, mpsc::Receiver<()>) {
    let project = server.example(example);
    let (tx, wakes) = mpsc::channel();
    let tx = std::sync::Mutex::new(tx);
    let waker: crate::session::Waker = Arc::new(move || {
        let _ = tx.lock().unwrap().send(());
    });
    let mut app = App::new(Box::new(StaticPreviews::default()));
    app.connect(server.socket.clone(), Some(project), waker);
    until(&mut app, &wakes, "the snapshot", |app| app.snapshot.is_some());
    (app, wakes)
}

#[test]
fn textures_show_in_previews_and_the_viewer() {
    let server = Server::start("textures");
    let (mut app, wakes) = connect(&server, "textures");
    app.set_viewport([1000.0, 700.0]);
    let previews: Vec<&str> = app.visible_previews().iter().map(|r| r.path.as_str()).collect();
    assert!(previews.contains(&"/out"), "{previews:?}");
    until(&mut app, &wakes, "the texture previews", |app| {
        !matches!(app.previews.get("/out"), None | Some(Preview::Pending))
    });
    app.select(Some("/out".into()));
    let Some(Viewer::Texture(_)) = &app.viewer else {
        panic!("a Texture operator gets the texture viewer")
    };
    if cfg!(target_os = "macos") {
        let Some(Preview::Texture(rect)) = app.previews.get("/out") else {
            panic!("expected a picture, got {:?}", app.previews.get("/out"))
        };
        let atlas = app.previews.atlas().expect("an atlas");
        assert!(rect[0] + rect[2] <= atlas.size[0] && rect[1] + rect[3] <= atlas.size[1]);
        until(&mut app, &wakes, "the first shared frame", |app| match &app.viewer {
            Some(Viewer::Texture(v)) => v.status().is_none(),
            _ => false,
        });
    } else {
        until(&mut app, &wakes, "the viewer's explanation", |app| match &app.viewer {
            Some(Viewer::Texture(v)) => v.status().is_some_and(|s| s.contains("macOS")),
            _ => false,
        });
    }
}

#[test]
fn edits_round_trip_through_the_server() {
    let server = Server::start("edit");
    let (mut app, wakes) = connect(&server, "hello");
    assert!(matches!(app.connection, Connection::Connected { .. }));
    assert!(!app.types.is_empty());
    let paths: Vec<&str> = app.nodes().iter().map(|op| op.path.as_str()).collect();
    assert_eq!(paths, ["/fx", "/lfo1", "/mix", "/noise1"]);

    // Channel previews fill in for the nodes that show.
    app.set_viewport([1000.0, 700.0]);
    until(
        &mut app,
        &wakes,
        "a preview",
        |app| matches!(app.previews.get("/lfo1"), Some(Preview::Channel(values)) if !values.is_empty()),
    );

    // The viewer samples the selected Channel operator.
    app.select(Some("/mix".into()));
    assert_eq!(app.op("/mix").unwrap().family, Family::Channel);
    until(&mut app, &wakes, "the channel viewer", |app| match &app.viewer {
        Some(Viewer::Channel(v)) => !v.view().channels.is_empty(),
        _ => false,
    });

    // A move and a connect, sent back to back against the same snapshot, both land.
    let revision = app.snapshot.as_ref().unwrap().revision;
    app.edit(vec![Edit::Move {
        path: "/lfo1".into(),
        x: 12.0,
        y: 24.0,
    }]);
    app.edit(vec![Edit::Connect {
        from: "/lfo1".into(),
        to: "/mix".into(),
        input: 2,
    }]);
    until(&mut app, &wakes, "both edits", |app| {
        let s = app.snapshot.as_ref().unwrap();
        s.revision >= revision + 2 && app.op("/mix").unwrap().inputs.len() == 3
    });
    assert_eq!(app.op("/lfo1").unwrap().position, [12.0, 24.0]);
    assert_eq!(app.notice, None);

    // Someone else's edit shows up through the watch.
    let mut other = Client::connect(&server.socket).unwrap();
    other
        .edit(
            None,
            vec![Edit::Add {
                path: "/extra".into(),
                ty: "channel/constant".into(),
            }],
        )
        .unwrap();
    until(&mut app, &wakes, "the other client's edit", |app| {
        app.op("/extra").is_some()
    });

    // A parameter set through the inspector's path reaches the server.
    app.select(Some("/extra".into()));
    app.set_param("value", futhark_protocol::Value::Float(2.5));
    until(&mut app, &wakes, "the parameter", |app| {
        app.op("/extra")
            .unwrap()
            .params
            .iter()
            .any(|p| p.name == "value" && p.value == futhark_protocol::Value::Float(2.5))
    });
}
