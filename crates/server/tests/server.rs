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

    let Sample::Channel { channels, .. } = client.sample("/sum").unwrap() else {
        panic!("a Channel sample")
    };
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
    let Sample::Channel { frame: sampled, .. } = client.sample("/lfo1").unwrap() else {
        panic!("a Channel sample")
    };
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

#[test]
fn previews_carry_channel_values_and_errors() {
    use futhark_protocol::previews;
    let server = Server::start("previews-channels");
    let project = server.example("hello");
    let mut client = server.client();
    client.open(&project).unwrap();

    let mut stream = previews_stream(&server, vec![node("/lfo1"), node("/missing"), node("/fx")]);
    let previews::Event::Frame { slot, previews, .. } = next_frame(&mut stream) else {
        unreachable!()
    };
    assert_eq!(slot, None);
    let channels = previews[0].channels.as_ref().unwrap();
    assert_eq!(channels[0].0, "chan1");
    assert_eq!(previews[1].error.as_deref(), Some("no operator at /missing"));
    assert_eq!(previews[2].error.as_deref(), Some("/fx has no output"));
}

#[cfg(target_os = "macos")]
#[test]
fn viewers_and_previews_share_finished_surfaces() {
    use futhark_client::Update;
    use futhark_protocol::{previews, shared};

    let server = Server::start("shared");
    let project = server.example("textures");
    let mut client = server.client();
    client.open(&project).unwrap();
    assert!(
        client
            .hello()
            .unwrap()
            .capabilities
            .contains(&"shared-frames:iosurface-mach".to_string())
    );

    // A viewer: frames arrive in a ring, drawn before they're announced.
    let params = shared::Params {
        path: "/out".into(),
        max_width: 320,
        max_height: 320,
        fps: None,
        slots: None,
    };
    let mut viewer = server.client().shared_frames(params).unwrap();
    assert_eq!(viewer.transport(), "iosurface-mach");
    let mut last = -1;
    for _ in 0..5 {
        let Some(Update::Frame(frame)) = viewer.recv().unwrap() else {
            panic!("expected a frame")
        };
        assert_eq!((frame.surface.width(), frame.surface.height()), (320, 180));
        assert!(frame.frame > last);
        last = frame.frame;
        assert!(
            frame.surface.read_bgra().chunks(4).all(|p| p[3] == 255),
            "an undrawn frame"
        );
    }

    // A viewer of something that isn't a texture reports it.
    let params = shared::Params {
        path: "/nope".into(),
        max_width: 64,
        max_height: 64,
        fps: None,
        slots: None,
    };
    let mut broken = server.client().shared_frames(params).unwrap();
    let Some(Update::Error(message)) = broken.recv().unwrap() else {
        panic!("expected an error")
    };
    assert_eq!(message, "no operator at /nope");

    // Previews: two textures packed into one atlas.
    let params = previews::Params {
        nodes: vec![node("/glow"), node("/out")],
        fps: None,
    };
    let mut stream = server.client().previews(params).unwrap();
    let frame = stream.recv().unwrap().unwrap();
    assert_eq!(frame.previews[0].rect, Some([0, 0, 160, 90]));
    assert_eq!(frame.previews[1].rect, Some([160, 0, 160, 90]));
    let atlas = frame.atlas.as_ref().unwrap();
    assert_eq!((atlas.width(), atlas.height()), (2048, 256));
    let pixels = atlas.read_bgra();
    let alpha = |x: usize, y: usize| pixels[(y * 2048 + x) * 4 + 3];
    assert_eq!(
        (alpha(200, 45), alpha(400, 45)),
        (255, 0),
        "drawn inside the rects only"
    );

    // Previewing fewer nodes keeps the ring; the frame after shows only what's left.
    stream.control().set_nodes(vec![node("/glow")]);
    drop(frame);
    let frame = loop {
        let frame = stream.recv().unwrap().unwrap();
        if frame.previews.len() == 1 {
            break frame;
        }
    };
    assert_eq!(frame.previews[0].rect, Some([0, 0, 160, 90]));
}

fn node(path: &str) -> futhark_protocol::previews::Node {
    futhark_protocol::previews::Node {
        path: path.into(),
        width: 160,
        height: 160,
    }
}

/// A raw previews connection.
struct PreviewStream(std::io::BufReader<std::os::unix::net::UnixStream>);

fn previews_stream(server: &Server, nodes: Vec<futhark_protocol::previews::Node>) -> PreviewStream {
    use futhark_protocol::{Method, Request, previews};
    use std::io::Write;
    let mut stream = std::os::unix::net::UnixStream::connect(&server.socket).unwrap();
    let request = Request::new(1, Method::Previews(previews::Params { nodes, fps: None }));
    writeln!(stream, "{}", serde_json::to_string(&request).unwrap()).unwrap();
    let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
    let response: serde_json::Value = read_line(&mut reader);
    assert!(response["error"].is_null(), "{response}");
    PreviewStream(reader)
}

fn next_frame(stream: &mut PreviewStream) -> futhark_protocol::previews::Event {
    loop {
        let event: futhark_protocol::previews::Event = read_line(&mut stream.0);
        if matches!(event, futhark_protocol::previews::Event::Frame { .. }) {
            return event;
        }
    }
}

fn read_line<T: serde::de::DeserializeOwned>(reader: &mut impl std::io::BufRead) -> T {
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    serde_json::from_str(&line).unwrap_or_else(|e| panic!("{e}: {line}"))
}
