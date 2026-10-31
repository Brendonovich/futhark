//! The app's state and the actions that change it. Everything that matters lives on the server: the app holds the
//! last snapshot and the type list, and its own selection, camera and gestures in progress. Each change is an edit
//! sent against the snapshot's revision; the snapshot that comes back replaces the local guess.

use std::{cell::Cell, collections::HashMap, path::PathBuf};

use futhark_protocol::{Edit, Family, Hello, Op, OpType, ParamKind, ParamType, Snapshot, Value};
use rune::{Live, TextEdit};

use crate::{
    geometry::{self, Camera, Target},
    network,
    previews::{PreviewRequest, PreviewSource, StaticPreviews, StreamPreviews},
    session::{Command, Reply, Session, Waker},
    viewer::Viewer,
};

pub enum Connection {
    Connecting,
    Connected {
        hello: Hello,
    },
    /// Headless scenes and tests: no server.
    Offline,
    Lost(String),
}

/// A node being dragged: where it started and how far it has moved, in world points.
#[derive(Clone, Debug, PartialEq)]
pub struct NodeDrag {
    pub path: String,
    pub moved: [f32; 2],
}

/// A wire being dragged out of a node's output, or picked up from an input.
#[derive(Clone, Debug, PartialEq)]
pub struct WireDrag {
    /// The operator the wire comes from.
    pub from: String,
    /// The input it was picked up from, if it was.
    pub picked_up: Option<(String, usize)>,
    /// Where the loose end started and where it is, in world points.
    pub start: [f32; 2],
    pub end: [f32; 2],
}

/// A number being dragged in the inspector, shown at its new value until the server's snapshot catches up.
#[derive(Clone, Debug, PartialEq)]
pub struct ParamDrag {
    pub param: String,
    pub start: f64,
    pub value: Value,
}

pub struct App {
    pub connection: Connection,
    session: Option<Session>,
    socket: PathBuf,
    waker: Option<Waker>,
    /// Commands made with no session (headless tests and scenes), in order.
    pub outbox: Vec<Command>,
    pub types: Vec<OpType>,
    pub snapshot: Option<Snapshot>,
    /// Edits sent and not yet answered. Snapshots that arrive meanwhile wait, so the screen doesn't jump back.
    in_flight: usize,
    held_snapshot: Option<Snapshot>,
    /// The component the editor shows.
    pub component: String,
    pub selected: Option<String>,
    /// An operator just added, selected once a snapshot has it.
    pending_select: Option<String>,
    pub camera: Camera,
    pub live_pan: Live<[f32; 2]>,
    pub live_zoom: Live<f32>,
    /// The canvas's size in points, once laid out.
    pub viewport: [f32; 2],
    /// Device pixels per point, as the view last saw.
    pub scale: Cell<f32>,
    /// The camera should frame the component once it can.
    frame_pending: bool,
    pub node_drag: Option<NodeDrag>,
    pub wire_drag: Option<WireDrag>,
    pan_at_press: [f32; 2],
    pub previews: Box<dyn PreviewSource>,
    visible: Vec<PreviewRequest>,
    pub viewer: Option<Viewer>,
    /// The add menu, open at a window point and placing at a world point.
    pub add_menu: Option<AddMenu>,
    /// The path typed into the open-project field.
    pub open_path: TextEdit,
    /// Inspector text fields by parameter name, while editing.
    pub fields: HashMap<String, TextEdit>,
    pub param_drag: Option<ParamDrag>,
    /// A `Set` waiting for the edit in flight to land, so dragging a number doesn't flood the server.
    queued_set: Option<(String, String, Value)>,
    /// The menu parameter whose list is open.
    pub open_select: Option<String>,
    pub notice: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AddMenu {
    pub at: Option<[f32; 2]>,
    pub world: [f32; 2],
}

impl App {
    pub fn new(previews: Box<dyn PreviewSource>) -> App {
        let camera = Camera::default();
        App {
            connection: Connection::Connecting,
            session: None,
            socket: futhark_protocol::socket_path(),
            waker: None,
            outbox: Vec::new(),
            types: Vec::new(),
            snapshot: None,
            in_flight: 0,
            held_snapshot: None,
            component: "/".into(),
            selected: None,
            pending_select: None,
            camera,
            live_pan: Live::new(camera.pan),
            live_zoom: Live::new(camera.zoom),
            viewport: [0.0, 0.0],
            scale: Cell::new(2.0),
            frame_pending: true,
            node_drag: None,
            wire_drag: None,
            pan_at_press: [0.0, 0.0],
            previews,
            visible: Vec::new(),
            viewer: None,
            add_menu: None,
            open_path: TextEdit::new(""),
            fields: HashMap::new(),
            param_drag: None,
            queued_set: None,
            open_select: None,
            notice: None,
        }
    }

    /// An app with no server, showing `snapshot`.
    pub fn offline(types: Vec<OpType>, snapshot: Snapshot) -> App {
        let mut app = App::new(Box::new(StaticPreviews::default()));
        app.connection = Connection::Offline;
        app.types = types;
        app.apply_snapshot(snapshot);
        app
    }

    /// Connects to the server at `socket`, starting it if needed, and opens `open` if given. Previews and viewers
    /// use connections of their own to the same server.
    pub fn connect(&mut self, socket: PathBuf, open: Option<PathBuf>, waker: Waker) {
        self.session = Some(Session::start(&socket, open, waker.clone()));
        self.previews = Box::new(StreamPreviews::start(socket.clone(), waker.clone()));
        self.visible.clear();
        self.socket = socket;
        self.waker = Some(waker);
    }

    pub fn send(&mut self, command: Command) {
        match &self.session {
            Some(session) => session.send(command),
            None => self.outbox.push(command),
        }
    }

    /// Applies replies from the server and anything the background threads changed. True when the view should
    /// re-run.
    pub fn poll(&mut self) -> bool {
        let replies = self.session.as_ref().map(Session::drain).unwrap_or_default();
        let mut changed = !replies.is_empty();
        for reply in replies {
            self.on_reply(reply);
        }
        changed |= self.previews.take_changed();
        changed |= self.viewer.as_ref().is_some_and(Viewer::take_changed);
        changed
    }

    fn on_reply(&mut self, reply: Reply) {
        match reply {
            Reply::Connected { hello, types } => {
                self.connection = Connection::Connected { hello };
                self.types = types;
            }
            Reply::Snapshot(snapshot) => {
                if self.in_flight == 0 {
                    self.apply_snapshot(snapshot);
                } else {
                    self.held_snapshot = Some(snapshot);
                }
            }
            Reply::NoProject => {
                self.snapshot = None;
                self.select(None);
            }
            Reply::EditDone => {
                self.in_flight = self.in_flight.saturating_sub(1);
                if self.in_flight == 0 {
                    if let Some((path, param, value)) = self.queued_set.take() {
                        self.edit(vec![Edit::Set { path, param, value }]);
                    } else if let Some(snapshot) = self.held_snapshot.take() {
                        self.apply_snapshot(snapshot);
                    }
                }
            }
            Reply::Saved => self.notice = Some("Saved.".into()),
            Reply::Error(message) => self.notice = Some(message),
            Reply::Disconnected(message) => {
                self.connection = Connection::Lost(message);
                self.in_flight = 0;
            }
        }
    }

    pub fn apply_snapshot(&mut self, snapshot: Snapshot) {
        let project_changed = self
            .snapshot
            .as_ref()
            .is_none_or(|s| s.project.path != snapshot.project.path);
        if project_changed {
            self.component = "/".into();
            self.frame_pending = true;
        }
        if self.component != "/" && network::find(&snapshot, &self.component).is_none() {
            self.component = "/".into();
            self.frame_pending = true;
        }
        let selected_gone = self
            .selected
            .as_ref()
            .is_some_and(|path| network::find(&snapshot, path).is_none());
        self.snapshot = Some(snapshot);
        if selected_gone {
            self.select(None);
        }
        if let Some(path) = self.pending_select.take() {
            if self.op(&path).is_some() {
                self.select(Some(path));
            } else {
                self.pending_select = Some(path);
            }
        }
        if self.param_drag.is_none() {
            self.fields.retain(|_, field| field.focused);
        }
        self.frame_if_pending();
        self.update_previews();
    }

    /// Sends edits against the snapshot's revision.
    pub fn edit(&mut self, edits: Vec<Edit>) {
        let Some(base) = self.snapshot.as_ref().map(|s| s.revision) else {
            return;
        };
        if self.session.is_some() {
            self.in_flight += 1;
        }
        self.notice = None;
        self.send(Command::Edit { base, edits });
    }

    pub fn op(&self, path: &str) -> Option<&Op> {
        network::find(self.snapshot.as_ref()?, path)
    }

    pub fn op_type(&self, op: &Op) -> Option<&OpType> {
        network::op_type(&self.types, &op.ty)
    }

    /// The operators the editor shows.
    pub fn nodes(&self) -> Vec<&Op> {
        match &self.snapshot {
            Some(s) => network::children(s, &self.component).collect(),
            None => Vec::new(),
        }
    }

    /// Where a node shows, counting a drag in progress.
    pub fn position(&self, op: &Op) -> [f32; 2] {
        match &self.node_drag {
            Some(d) if d.path == op.path => [op.position[0] + d.moved[0], op.position[1] + d.moved[1]],
            _ => op.position,
        }
    }

    /// A node's input slots: whether each is wired.
    pub fn input_slots(&self, op: &Op) -> Vec<bool> {
        let (min, max) = self
            .op_type(op)
            .map_or((0, op.inputs.len()), |t| (t.min_inputs, t.max_inputs));
        let wired = op.inputs.len();
        (0..geometry::input_slots(wired, min, max))
            .map(|i| op.inputs.get(i).is_some_and(Option::is_some))
            .collect()
    }

    pub fn has_output(&self, op: &Op) -> bool {
        op.family != Family::Component
    }

    // Selection and navigation.

    pub fn select(&mut self, path: Option<String>) {
        if self.selected == path {
            return;
        }
        self.selected = path;
        self.fields.clear();
        self.param_drag = None;
        self.open_select = None;
        self.viewer = None;
        let started = self.selected.as_ref().and_then(|path| {
            let family = self.op(path)?.family;
            let waker = self.waker.clone()?;
            Some(Viewer::start(&self.socket, path, family, waker))
        });
        self.viewer = started;
    }

    pub fn enter(&mut self, component: String) {
        self.component = component;
        self.select(None);
        self.frame_pending = true;
        self.frame_if_pending();
        self.update_previews();
    }

    // The camera.

    pub fn set_viewport(&mut self, size: [f32; 2]) {
        if self.viewport != size {
            self.viewport = size;
            self.frame_if_pending();
            self.update_previews();
        }
    }

    fn frame_if_pending(&mut self) {
        if !self.frame_pending || self.viewport[0] <= 0.0 || self.snapshot.is_none() {
            return;
        }
        self.frame_pending = false;
        let bounds = geometry::nodes_bounds(self.nodes().iter().map(|op| op.position));
        let camera = match bounds {
            Some(b) => Camera::framing(b, self.viewport),
            None => Camera::default(),
        };
        self.set_camera(camera);
    }

    /// Moves the camera. True when previews turned on or off, which the view has to show; otherwise the move is
    /// the live values' alone.
    pub fn set_camera(&mut self, camera: Camera) -> bool {
        let previews_changed = camera.shows_previews() != self.camera.shows_previews();
        self.camera = camera;
        self.live_pan.set(camera.pan);
        self.live_zoom.set(camera.zoom);
        self.update_previews();
        previews_changed
    }

    pub fn pan_start(&mut self) {
        self.pan_at_press = self.camera.pan;
    }

    pub fn pan_to(&mut self, travel: [f32; 2]) -> bool {
        let camera = Camera {
            pan: [self.pan_at_press[0] + travel[0], self.pan_at_press[1] + travel[1]],
            zoom: self.camera.zoom,
        };
        self.set_camera(camera)
    }

    /// The nodes showing and their preview sizes, handed to the preview source when they change.
    pub fn preview_requests(&self) -> Vec<PreviewRequest> {
        if !self.camera.shows_previews() || self.viewport[0] <= 0.0 {
            return Vec::new();
        }
        let pixels = self.camera.preview_pixels(self.scale.get());
        self.nodes()
            .into_iter()
            .filter(|op| op.family != Family::Component)
            .filter(|op| self.camera.sees(geometry::node_rect(self.position(op)), self.viewport))
            .map(|op| PreviewRequest {
                path: op.path.clone(),
                family: op.family,
                pixels,
            })
            .collect()
    }

    pub fn update_previews(&mut self) {
        let requests = self.preview_requests();
        if requests != self.visible {
            self.visible = requests.clone();
            self.previews.set_visible(requests);
        }
    }

    #[cfg(test)]
    pub fn visible_previews(&self) -> &[PreviewRequest] {
        &self.visible
    }

    // Editing the network.

    pub fn drag_node(&mut self, path: &str, travel: [f32; 2]) {
        let zoom = self.camera.zoom;
        self.node_drag = Some(NodeDrag {
            path: path.into(),
            moved: [travel[0] / zoom, travel[1] / zoom],
        });
    }

    /// Ends a node drag, moving it on the server (and here, until the snapshot says so).
    pub fn drop_node(&mut self) {
        let Some(drag) = self.node_drag.take() else { return };
        if drag.moved == [0.0, 0.0] {
            return;
        }
        let Some(op) = self
            .snapshot
            .as_mut()
            .and_then(|s| s.ops.iter_mut().find(|op| op.path == drag.path))
        else {
            return;
        };
        let to = [
            (op.position[0] + drag.moved[0]).round(),
            (op.position[1] + drag.moved[1]).round(),
        ];
        op.position = to;
        self.edit(vec![Edit::Move {
            path: drag.path,
            x: to[0],
            y: to[1],
        }]);
        self.update_previews();
    }

    /// Starts dragging a wire out of `from`'s output.
    pub fn start_wire(&mut self, from: &str) {
        let Some(op) = self.op(from) else { return };
        let start = geometry::output_point(op.position);
        self.wire_drag = Some(WireDrag {
            from: from.into(),
            picked_up: None,
            start,
            end: start,
        });
    }

    /// Picks up the wire into `to`'s input `input`, if one is there.
    pub fn pick_up_wire(&mut self, to: &str, input: usize) {
        let Some(op) = self.op(to) else { return };
        let Some(Some(source)) = op.inputs.get(input) else {
            return;
        };
        let start = geometry::input_point(op.position, input, self.input_slots(op).len());
        self.wire_drag = Some(WireDrag {
            from: network::join(network::parent(to), source),
            picked_up: Some((to.into(), input)),
            start,
            end: start,
        });
    }

    pub fn move_wire(&mut self, travel: [f32; 2]) {
        let zoom = self.camera.zoom;
        if let Some(wire) = &mut self.wire_drag {
            wire.end = [wire.start[0] + travel[0] / zoom, wire.start[1] + travel[1] / zoom];
        }
    }

    /// Drops the wire being dragged: connects it to the input under its end, or disconnects it if it was picked up
    /// and dropped on nothing.
    pub fn drop_wire(&mut self) {
        let Some(wire) = self.wire_drag.take() else { return };
        let nodes = self.nodes();
        let slots: Vec<Vec<bool>> = nodes.iter().map(|op| self.input_slots(op)).collect();
        let targets: Vec<Target<'_>> = nodes
            .iter()
            .zip(&slots)
            .map(|(op, inputs)| Target {
                position: op.position,
                inputs,
            })
            .collect();
        let target = geometry::drop_target(wire.end, &targets)
            .map(|(i, input)| (nodes[i].path.clone(), input))
            .filter(|(to, _)| *to != wire.from);
        let mut edits = Vec::new();
        if let Some((to, input)) = &wire.picked_up {
            if target.as_ref() == Some(&(to.clone(), *input)) {
                return;
            }
            edits.push(Edit::Disconnect {
                to: to.clone(),
                input: *input,
            });
        }
        if let Some((to, input)) = target {
            edits.push(Edit::Connect {
                from: wire.from,
                to,
                input,
            });
        }
        if !edits.is_empty() {
            self.edit(edits);
        }
    }

    pub fn add_op(&mut self, ty: &str, at: [f32; 2]) {
        let Some(snapshot) = &self.snapshot else { return };
        let path = network::join(&self.component, &network::fresh_name(snapshot, &self.component, ty));
        self.edit(vec![
            Edit::Add {
                path: path.clone(),
                ty: ty.into(),
            },
            Edit::Move {
                path: path.clone(),
                x: at[0].round(),
                y: at[1].round(),
            },
        ]);
        self.add_menu = None;
        self.pending_select = Some(path);
    }

    pub fn delete_selected(&mut self) {
        if let Some(path) = self.selected.clone() {
            self.edit(vec![Edit::Remove { path }]);
        }
    }

    /// Opens the add menu at window point `at` (or under the toolbar's button), placing at the world point there.
    pub fn open_add_menu(&mut self, at: Option<[f32; 2]>, canvas_point: Option<[f32; 2]>) {
        let screen = canvas_point.unwrap_or([self.viewport[0] / 2.0, self.viewport[1] / 2.0]);
        let world = self.camera.to_world(screen);
        self.add_menu = Some(AddMenu {
            at,
            world: [world[0] - geometry::NODE_W / 2.0, world[1] - geometry::NODE_H / 2.0],
        });
    }

    // Parameters.

    /// A parameter's value as shown: a drag in progress, or the snapshot's.
    pub fn param_value(&self, op: &Op, param: &str) -> Option<Value> {
        if let Some(d) = &self.param_drag
            && d.param == param
        {
            return Some(d.value.clone());
        }
        op.params.iter().find(|p| p.name == param).map(|p| p.value.clone())
    }

    pub fn set_param(&mut self, param: &str, value: Value) {
        let Some(path) = self.selected.clone() else { return };
        if let Some(op) = self
            .snapshot
            .as_mut()
            .and_then(|s| s.ops.iter_mut().find(|op| op.path == path))
            && let Some(p) = op.params.iter_mut().find(|p| p.name == param)
        {
            p.value = value.clone();
        }
        if self.in_flight > 0 {
            self.queued_set = Some((path, param.into(), value));
        } else {
            self.edit(vec![Edit::Set {
                path,
                param: param.into(),
                value,
            }]);
        }
    }

    /// Drags a number parameter by `dx` points.
    pub fn drag_param(&mut self, ty: &ParamType, current: f64, dx: f32, fine: bool) {
        let start = match &self.param_drag {
            Some(d) if d.param == ty.name => d.start,
            _ => current,
        };
        let value = dragged(ty, start, dx, fine);
        self.param_drag = Some(ParamDrag {
            param: ty.name.clone(),
            start,
            value: value.clone(),
        });
        self.set_param(&ty.name, value);
    }

    pub fn end_param_drag(&mut self) {
        self.param_drag = None;
    }

    /// Starts typing into a parameter's field.
    pub fn edit_field(&mut self, param: &str, text: String) {
        let mut field = TextEdit::new(text);
        field.focused = true;
        let len = field.text.len();
        field.set_selection(0, len);
        self.fields.insert(param.into(), field);
    }

    pub fn field(&mut self, param: &str) -> &mut TextEdit {
        self.fields.entry(param.into()).or_insert_with(|| TextEdit::new(""))
    }

    /// Sends what was typed into a parameter's field.
    pub fn submit_field(&mut self, ty: &ParamType) {
        let Some(field) = self.fields.remove(&ty.name) else {
            return;
        };
        match parse(ty, field.text.trim()) {
            Some(value) => self.set_param(&ty.name, value),
            None => self.notice = Some(format!("{} isn't a valid {}", field.text.trim(), ty.label)),
        }
    }

    pub fn open_project(&mut self, path: PathBuf) {
        self.send(Command::Open(path));
    }

    pub fn save(&mut self) {
        self.send(Command::Save);
    }
}

/// A number dragged `dx` points from `start`: about 200 points across its soft range, a tenth of that with `fine`,
/// clamped to its hard range.
pub fn dragged(ty: &ParamType, start: f64, dx: f32, fine: bool) -> Value {
    let lo = ty.soft_min.or(ty.min).unwrap_or(0.0);
    let hi = ty.soft_max.or(ty.max).unwrap_or(lo + 1.0);
    let mut per_point = ((hi - lo).abs().max(1e-6)) / 200.0;
    if fine {
        per_point /= 10.0;
    }
    let mut v = start + dx as f64 * per_point;
    if let Some(min) = ty.min {
        v = v.max(min);
    }
    if let Some(max) = ty.max {
        v = v.min(max);
    }
    match ty.kind {
        ParamKind::Int => Value::Int(v.round() as i64),
        _ => Value::Float((v * 1000.0).round() / 1000.0),
    }
}

/// A typed value for a parameter, or None if it doesn't read as one.
pub fn parse(ty: &ParamType, text: &str) -> Option<Value> {
    let clamp = |v: f64| {
        let v = ty.min.map_or(v, |min| v.max(min));
        ty.max.map_or(v, |max| v.min(max))
    };
    match &ty.kind {
        ParamKind::Float => text
            .parse::<f64>()
            .ok()
            .filter(|v| v.is_finite())
            .map(|v| Value::Float(clamp(v))),
        ParamKind::Int => text
            .parse::<i64>()
            .ok()
            .or_else(|| text.parse::<f64>().ok().map(|v| v.round() as i64))
            .map(|v| Value::Int(clamp(v as f64) as i64)),
        ParamKind::String => Some(Value::String(text.into())),
        ParamKind::Toggle => match text {
            "true" | "on" | "1" => Some(Value::Bool(true)),
            "false" | "off" | "0" => Some(Value::Bool(false)),
            _ => None,
        },
        ParamKind::Menu { items } => items
            .iter()
            .find(|i| i.name == text || i.label == text)
            .map(|i| Value::String(i.name.clone())),
    }
}

/// A number value as f64.
pub fn number(value: &Value) -> Option<f64> {
    match value {
        Value::Int(v) => Some(*v as f64),
        Value::Float(v) => Some(*v),
        _ => None,
    }
}
