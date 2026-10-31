//! Runs the real `futharkd` binary on a private socket and drives it through the client library.

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use futhark_client::{Client, ClientError};
use futhark_protocol::{Edit, Event, Family, RpcError, Sample, Value};

struct Server {
    child: Child,
    socket: PathBuf,
    dir: PathBuf,
}

impl Server {
    fn start(name: &str) -> Server {
        let dir = std::env::temp_dir().join(format!("futhark-server-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("s.sock");
        let child = Command::new(env!("CARGO_BIN_EXE_futharkd"))
            .arg("--socket")
            .arg(&socket)
            .arg("--exit-on-stdin-close")
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while Client::connect(&socket).is_err() {
            assert!(Instant::now() < deadline, "futharkd didn't start");
            thread::sleep(Duration::from_millis(10));
        }
        Server { child, socket, dir }
    }

    fn client(&self) -> Client {
        Client::connect(&self.socket).unwrap()
    }

    /// A copy of an example project in this server's directory.
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
        // Closing stdin stops the server.
        drop(self.child.stdin.take());
        let _ = self.child.wait();
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

fn rpc_code(error: ClientError) -> i64 {
    match error {
        ClientError::Rpc(error) => error.code,
        other => panic!("expected an RPC error, got {other}"),
    }
}

#[test]
fn hello_and_status_without_a_project() {
    let server = Server::start("hello");
    let mut client = server.client();
    assert_eq!(client.hello().unwrap().protocol, futhark_protocol::PROTOCOL);
    assert_eq!(client.status().unwrap().project, None);
    assert_eq!(rpc_code(client.get().unwrap_err()), RpcError::WRONG_STATE);
}

#[test]
fn opens_edits_samples_and_saves() {
    let server = Server::start("edit");
    let project = server.example("hello");
    let mut client = server.client();

    let opened = client.open(&project).unwrap();
    assert_eq!(opened.project.name, "hello");
    assert_eq!(opened.diagnostics, Vec::<String>::new());

    let snapshot = client.get().unwrap();
    let paths: Vec<&str> = snapshot.ops.iter().map(|op| op.path.as_str()).collect();
    assert_eq!(paths, ["/fx", "/fx/wobble", "/lfo1", "/mix", "/noise1"]);
    let mix = &snapshot.ops[3];
    assert_eq!(mix.family, Family::Channel);
    assert_eq!(mix.inputs, [Some("lfo1".to_string()), Some("noise1".to_string())]);

    let edited = client
        .edit(
            Some(snapshot.revision),
            vec![
                Edit::Add {
                    path: "/one".into(),
                    ty: "channel/constant".into(),
                },
                Edit::Set {
                    path: "/one".into(),
                    param: "value".into(),
                    value: Value::Int(1),
                },
                Edit::Add {
                    path: "/sum".into(),
                    ty: "channel/math".into(),
                },
                Edit::Connect {
                    from: "/one".into(),
                    to: "/sum".into(),
                    input: 0,
                },
                Edit::Connect {
                    from: "/one".into(),
                    to: "/sum".into(),
                    input: 1,
                },
                Edit::Set {
                    path: "/sum".into(),
                    param: "combine".into(),
                    value: Value::String("add".into()),
                },
                Edit::Move {
                    path: "/sum".into(),
                    x: 200.0,
                    y: 0.0,
                },
            ],
        )
        .unwrap();
    assert!(edited.revision > snapshot.revision);

    let Sample::Channel { channels, .. } = client.sample("/sum").unwrap();
    assert_eq!(channels[0].samples, [2.0]);

    // A stale revision is refused, and a failing batch changes nothing.
    let stale = client.edit(Some(snapshot.revision), vec![Edit::Remove { path: "/one".into() }]);
    assert_eq!(rpc_code(stale.unwrap_err()), RpcError::WRONG_STATE);
    let failing = client.edit(
        None,
        vec![
            Edit::Remove { path: "/one".into() },
            Edit::Remove {
                path: "/missing".into(),
            },
        ],
    );
    let ClientError::Rpc(error) = failing.unwrap_err() else {
        panic!("expected an RPC error")
    };
    assert_eq!(error.message, "edits[1]: no operator at /missing");
    assert_eq!(client.get().unwrap().revision, edited.revision);

    client.save().unwrap();
    let component = fs::read_to_string(project.join("root/component.kdl")).unwrap();
    assert!(
        component.contains("one \"channel/constant\" {\n    value 1.0\n}"),
        "{component}"
    );
    assert!(
        component.contains("sum \"channel/math\" {\n    input one\n    input one\n}"),
        "{component}"
    );
}

#[test]
fn watchers_hear_about_changes() {
    let server = Server::start("watch");
    let project = server.example("hello");
    let (status, mut events) = server.client().watch().unwrap();
    assert_eq!(status.project, None);

    let mut client = server.client();
    client.open(&project).unwrap();
    let edited = client
        .edit(
            None,
            vec![Edit::Add {
                path: "/c".into(),
                ty: "channel/constant".into(),
            }],
        )
        .unwrap();
    client.close().unwrap();

    assert!(matches!(events.next().unwrap().unwrap(), Event::ProjectOpened { project } if project.name == "hello"));
    assert_eq!(
        events.next().unwrap().unwrap(),
        Event::NetworkChanged {
            revision: edited.revision
        }
    );
    assert_eq!(events.next().unwrap().unwrap(), Event::ProjectClosed);
}

#[test]
fn the_clock_runs_in_real_time() {
    let server = Server::start("clock");
    let project = server.example("hello");
    let mut client = server.client();
    client.open(&project).unwrap();
    thread::sleep(Duration::from_millis(100));
    let frame = client.status().unwrap().frame;
    assert!((4..=60).contains(&frame), "frame {frame} after 100 ms at 60 fps");
    let Sample::Channel { frame: sampled, .. } = client.sample("/lfo1").unwrap();
    assert!(sampled >= frame);
}

#[test]
fn bad_requests_get_useful_errors() {
    let server = Server::start("errors");
    let mut client = server.client();
    let response = raw(&server.socket, r#"{"jsonrpc":"2.0","id":1,"method":"teleport"}"#);
    assert_eq!(response["error"]["code"], RpcError::METHOD_NOT_FOUND);
    let response = raw(&server.socket, "not json");
    assert_eq!(response["error"]["code"], RpcError::PARSE_ERROR);
    let relative = client.open(Path::new("/nowhere/at/all"));
    assert_eq!(rpc_code(relative.unwrap_err()), RpcError::FAILED);
}

#[test]
fn creates_a_project_and_shuts_down() {
    let mut server = Server::start("create");
    let mut client = server.client();
    let path = server.dir.join("fresh");
    let opened = client.create(&path, None).unwrap();
    assert_eq!(opened.project.name, "fresh");
    assert!(path.join("project.kdl").exists());
    assert!(client.create(&path, None).is_err());

    client.shutdown().unwrap();
    let status = server.child.wait().unwrap();
    assert!(status.success());
    assert!(!server.socket.exists());
}

fn raw(socket: &Path, line: &str) -> serde_json::Value {
    use std::io::{BufRead, BufReader, Write};
    let mut stream = std::os::unix::net::UnixStream::connect(socket).unwrap();
    writeln!(stream, "{line}").unwrap();
    let mut reply = String::new();
    BufReader::new(stream).read_line(&mut reply).unwrap();
    serde_json::from_str(&reply).unwrap()
}
