//! The protocol between `futharkd` and its clients: JSON-RPC 2.0, one JSON object per line, over a Unix socket.
//!
//! A client sends [`Request`]s and gets one [`Response`] for each, in order. `watch` and `shared_frames` reply once
//! and then turn the connection into a stream ([`Notification`]s, or shared-frame messages).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Bumped when an existing method changes incompatibly. New methods don't bump it.
pub const PROTOCOL: u32 = 1;

pub const JSONRPC: &str = "2.0";

/// Where the local server listens: `$FUTHARK_SOCKET`, otherwise `futharkd.sock` in the user's application support
/// directory (macOS) or runtime directory (Linux).
pub fn socket_path() -> PathBuf {
    if let Some(path) = std::env::var_os("FUTHARK_SOCKET") {
        return path.into();
    }
    state_dir().join("futharkd.sock")
}

/// The directory for the server's socket and log.
pub fn state_dir() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    if cfg!(target_os = "macos")
        && let Some(home) = home
    {
        return home.join("Library/Application Support/futhark");
    }
    if let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR") {
        return PathBuf::from(runtime).join("futhark");
    }
    let user = std::env::var("USER").unwrap_or_else(|_| "user".into());
    std::env::temp_dir().join(format!("futhark-{user}"))
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub jsonrpc: String,
    pub id: u64,
    #[serde(flatten)]
    pub method: Method,
}

impl Request {
    pub fn new(id: u64, method: Method) -> Self {
        Request {
            jsonrpc: JSONRPC.into(),
            id,
            method,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "method", content = "params", rename_all = "snake_case")]
pub enum Method {
    /// Protocol version and capabilities. Returns [`Hello`].
    Hello,
    /// Returns [`Status`].
    Status,
    /// Creates an empty project on disk and opens it. Returns [`Opened`].
    Create(CreateParams),
    /// Opens a project, closing the current one. Returns [`Opened`].
    Open(OpenParams),
    /// Saves the open project. Returns `null`.
    Save,
    /// Closes the open project without saving. Returns `null`.
    Close,
    /// Every operator type. Returns `Vec<OpType>`.
    Types,
    /// The open project's network. Returns [`Snapshot`].
    Get,
    /// Applies a batch of edits, all or nothing. Returns [`Edited`].
    Edit(EditParams),
    /// An operator's current output. Returns [`Sample`].
    Sample(SampleParams),
    /// Replies with [`Status`], then streams [`Event`]s as [`Notification`]s.
    Watch,
    /// Shows a Texture operator through shared GPU surfaces. Replies with [`shared::Started`], then the connection
    /// carries [`shared::Event`]s from the server and [`shared::Control`]s from the client, one JSON object per line.
    /// Local clients only, on systems with a shared-surface transport (see `hello`'s capabilities).
    SharedFrames(shared::Params),
    /// Stops the server. Returns `null`.
    Shutdown,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Response {
    pub jsonrpc: String,
    pub id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

impl Response {
    pub fn ok(id: u64, result: serde_json::Value) -> Self {
        Response {
            jsonrpc: JSONRPC.into(),
            id: Some(id),
            result: Some(result),
            error: None,
        }
    }

    pub fn error(id: Option<u64>, error: RpcError) -> Self {
        Response {
            jsonrpc: JSONRPC.into(),
            id,
            result: None,
            error: Some(error),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
}

impl RpcError {
    pub const PARSE_ERROR: i64 = -32700;
    pub const INVALID_REQUEST: i64 = -32600;
    pub const METHOD_NOT_FOUND: i64 = -32601;
    pub const INVALID_PARAMS: i64 = -32602;
    /// The request was valid but failed.
    pub const FAILED: i64 = -32000;
    /// The request doesn't fit the server's state: no project is open, or an edit's revision is stale.
    pub const WRONG_STATE: i64 = -32001;
    /// This server can't do that, such as shared frames on a system without a shared-surface transport.
    pub const UNSUPPORTED: i64 = -32003;

    pub fn new(code: i64, message: impl Into<String>) -> Self {
        RpcError {
            code,
            message: message.into(),
        }
    }

    pub fn failed(message: impl Into<String>) -> Self {
        Self::new(Self::FAILED, message)
    }

    pub fn wrong_state(message: impl Into<String>) -> Self {
        Self::new(Self::WRONG_STATE, message)
    }
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.message, self.code)
    }
}

impl std::error::Error for RpcError {}

/// A message the server sends without a request, on a `watch` connection.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Notification {
    pub jsonrpc: String,
    pub method: String,
    pub params: Event,
}

impl Notification {
    pub fn event(event: Event) -> Self {
        Notification {
            jsonrpc: JSONRPC.into(),
            method: "event".into(),
            params: event,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// The network changed. `get` returns the new state.
    NetworkChanged {
        revision: u64,
    },
    ProjectOpened {
        project: ProjectInfo,
    },
    ProjectClosed,
    ProjectSaved {
        project: ProjectInfo,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Hello {
    pub protocol: u32,
    /// The server's version.
    pub version: String,
    /// Optional features, such as `shared-frames:iosurface-mach`.
    pub capabilities: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Status {
    pub project: Option<ProjectInfo>,
    pub frame: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProjectInfo {
    /// The project directory, absolute.
    pub path: String,
    pub name: String,
    /// Frames per second.
    pub rate: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CreateParams {
    pub path: String,
    /// Defaults to the directory's name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OpenParams {
    pub path: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Opened {
    pub project: ProjectInfo,
    pub revision: u64,
    /// Anything in the project's files that didn't load as written.
    pub diagnostics: Vec<String>,
}

/// A parameter value. Numbers with a fraction or exponent are floats; menu items are strings.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Value {
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
}

impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Value::Bool(v) => write!(f, "{v}"),
            Value::Int(v) => write!(f, "{v}"),
            Value::Float(v) => write!(f, "{v:?}"),
            Value::String(v) => write!(f, "{v}"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub revision: u64,
    pub project: ProjectInfo,
    /// Every operator, parents before children, siblings by name.
    pub ops: Vec<Op>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Op {
    pub path: String,
    #[serde(rename = "type")]
    pub ty: String,
    pub family: Family,
    /// Wired inputs by index, as sibling names.
    pub inputs: Vec<Option<String>>,
    /// Every parameter, in the type's order.
    pub params: Vec<Param>,
    pub position: [f32; 2],
    /// The last cook error, if it failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Param {
    pub name: String,
    pub value: Value,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Family {
    Texture,
    Channel,
    Geometry,
    Data,
    Shader,
    Component,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OpType {
    pub name: String,
    pub label: String,
    pub doc: String,
    pub family: Family,
    pub min_inputs: usize,
    pub max_inputs: usize,
    pub time_dependent: bool,
    pub params: Vec<ParamType>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ParamType {
    pub name: String,
    pub label: String,
    pub doc: String,
    pub kind: ParamKind,
    pub default: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub soft_min: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub soft_max: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ParamKind {
    Float,
    Int,
    Toggle,
    String,
    Menu { items: Vec<MenuItem> },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MenuItem {
    pub name: String,
    pub label: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EditParams {
    /// Fail with `WRONG_STATE` unless the network is at this revision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<u64>,
    pub edits: Vec<Edit>,
}

/// One change to the network. Operators are named by absolute path, such as `/fx/noise1`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Edit {
    Add {
        path: String,
        #[serde(rename = "type")]
        ty: String,
    },
    Remove {
        path: String,
    },
    Rename {
        path: String,
        name: String,
    },
    Set {
        path: String,
        param: String,
        value: Value,
    },
    Connect {
        from: String,
        to: String,
        #[serde(default)]
        input: usize,
    },
    Disconnect {
        to: String,
        #[serde(default)]
        input: usize,
    },
    Move {
        path: String,
        x: f32,
        y: f32,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Edited {
    pub revision: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SampleParams {
    pub path: String,
}

/// An operator's output at the server's current frame.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "family", rename_all = "snake_case")]
pub enum Sample {
    Channel {
        frame: i64,
        rate: f64,
        start: i64,
        channels: Vec<ChannelSamples>,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChannelSamples {
    pub name: String,
    pub samples: Vec<f32>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn requests_look_as_documented() {
        let request = Request::new(
            7,
            Method::Edit(EditParams {
                revision: Some(3),
                edits: vec![
                    Edit::Add {
                        path: "/lfo1".into(),
                        ty: "channel/lfo".into(),
                    },
                    Edit::Set {
                        path: "/lfo1".into(),
                        param: "frequency".into(),
                        value: Value::Float(2.0),
                    },
                    Edit::Connect {
                        from: "/lfo1".into(),
                        to: "/math1".into(),
                        input: 0,
                    },
                ],
            }),
        );
        assert_eq!(
            serde_json::to_value(&request).unwrap(),
            json!({
                "jsonrpc": "2.0",
                "id": 7,
                "method": "edit",
                "params": {
                    "revision": 3,
                    "edits": [
                        { "op": "add", "path": "/lfo1", "type": "channel/lfo" },
                        { "op": "set", "path": "/lfo1", "param": "frequency", "value": 2.0 },
                        { "op": "connect", "from": "/lfo1", "to": "/math1", "input": 0 },
                    ],
                },
            })
        );
        let get: Request = serde_json::from_str(r#"{"jsonrpc":"2.0","id":1,"method":"get"}"#).unwrap();
        assert_eq!(get.method, Method::Get);
    }

    #[test]
    fn values_keep_their_type() {
        let parse = |s: &str| serde_json::from_str::<Value>(s).unwrap();
        assert_eq!(parse("1"), Value::Int(1));
        assert_eq!(parse("1.0"), Value::Float(1.0));
        assert_eq!(parse("1e3"), Value::Float(1000.0));
        assert_eq!(parse("true"), Value::Bool(true));
        assert_eq!(parse("\"sine\""), Value::String("sine".into()));
        assert_eq!(serde_json::to_string(&Value::Float(2.0)).unwrap(), "2.0");
    }

    #[test]
    fn events_look_as_documented() {
        let line = serde_json::to_value(Notification::event(Event::NetworkChanged { revision: 4 })).unwrap();
        assert_eq!(
            line,
            json!({ "jsonrpc": "2.0", "method": "event", "params": { "type": "network_changed", "revision": 4 } })
        );
    }
}

/// Shared frames: a viewer of a Texture operator drawn by the server's GPU into a ring of surfaces the client shows
/// without copying.
///
/// The server sends a [`Ring`](shared::Event::Ring) naming a surface service and a one-time token, and the client
/// fetches the ring's surfaces with them (`futhark_surfaces::fetch`). Each [`Frame`](shared::Event::Frame) then names
/// the slot holding a finished picture; the slot stays the client's until it sends a
/// [`Release`](shared::Control::Release). A new ring (a new generation) replaces the old one when the size changes.
pub mod shared {
    use serde::{Deserialize, Serialize};

    pub const DEFAULT_SLOTS: u32 = 4;
    pub const MAX_SLOTS: u32 = 8;

    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
    pub struct Params {
        /// The Texture operator to show.
        pub path: String,
        /// The largest picture the client wants. The ring fits the texture into it, keeping its shape.
        pub max_width: u32,
        pub max_height: u32,
        /// Frames per second, at most the project's rate.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub fps: Option<u32>,
        /// Surfaces in the ring, 2 to 8.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub slots: Option<u32>,
    }

    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
    pub struct Started {
        /// How surfaces travel, such as `iosurface-mach`.
        pub transport: String,
    }

    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
    #[serde(tag = "type", rename_all = "snake_case")]
    pub enum Event {
        /// A new ring of surfaces. Fetch them from `service` with `token`.
        Ring {
            generation: u64,
            width: u32,
            height: u32,
            slots: u32,
            service: String,
            token: String,
        },
        /// `slot` of ring `generation` holds the picture of engine frame `frame`, finished on the GPU.
        Frame { generation: u64, slot: u32, frame: i64 },
        /// The operator has no picture right now (for example, a cook error). Frames resume when it does.
        Error { message: String },
        /// The stream is over.
        End { reason: String },
    }

    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
    #[serde(tag = "type", rename_all = "snake_case")]
    pub enum Control {
        /// The client no longer shows this slot; the server may draw into it again.
        Release {
            generation: u64,
            slot: u32,
        },
        Fps {
            fps: u32,
        },
        Size {
            max_width: u32,
            max_height: u32,
        },
    }
}
