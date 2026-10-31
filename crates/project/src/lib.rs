//! Projects on disk. A project is a directory of text files meant to live in git:
//!
//! ```text
//! show/
//!   project.kdl        settings
//!   root/              the root component
//!     component.kdl    its operators, parameters and wires
//!     layout.kdl       node positions
//!     fx/              a child component, laid out the same way
//! ```
//!
//! Files are written in one fixed style so that saving never makes noise in a diff: operators sorted by name, one
//! fact per line, parameters in declaration order, defaults left out. Loading is forgiving: anything it can't make
//! sense of becomes a [`Diagnostic`] and the rest of the project still loads.

use std::{
    fmt::{self, Write as _},
    fs, io,
    path::{Path, PathBuf},
    sync::Arc,
};

use futhark_graph::{Family, GraphError, Network, OpId, Registry, Value};
use kdl::{KdlDocument, KdlNode, KdlValue};

const PROJECT_FILE: &str = "project.kdl";
const ROOT_DIR: &str = "root";
const COMPONENT_FILE: &str = "component.kdl";
const LAYOUT_FILE: &str = "layout.kdl";

pub struct Project {
    pub name: String,
    /// Frames per second.
    pub rate: f64,
    pub network: Network,
}

impl Project {
    pub fn new(name: impl Into<String>, registry: Arc<Registry>) -> Self {
        Project {
            name: name.into(),
            rate: 60.0,
            network: Network::new(registry),
        }
    }
}

/// Something in the project that couldn't be loaded as written.
#[derive(Clone, Debug, PartialEq)]
pub struct Diagnostic {
    /// The operator's path, or the file's path relative to the project.
    pub at: String,
    pub message: String,
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.at, self.message)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ProjectError {
    #[error("{}: {source}", path.display())]
    Io { path: PathBuf, source: io::Error },
    #[error("{}: {message}", path.display())]
    Invalid { path: PathBuf, message: String },
}

/// Loads a project. Only an unreadable or invalid `project.kdl` is an error; problems in components are returned
/// as diagnostics.
pub fn load(dir: &Path, registry: Arc<Registry>) -> Result<(Project, Vec<Diagnostic>), ProjectError> {
    let path = dir.join(PROJECT_FILE);
    let text = fs::read_to_string(&path).map_err(|source| ProjectError::Io {
        path: path.clone(),
        source,
    })?;
    let invalid = |message: String| ProjectError::Invalid {
        path: path.clone(),
        message,
    };
    let doc = KdlDocument::parse_v2(&text).map_err(|e| invalid(e.to_string()))?;

    let mut project = Project::new("", registry);
    for node in doc.nodes() {
        match (node.name().value(), single_arg(node)) {
            ("name", Some(KdlValue::String(name))) => project.name = name.clone(),
            ("rate", Some(KdlValue::Float(rate))) if *rate > 0.0 => project.rate = *rate,
            ("rate", Some(KdlValue::Integer(rate))) if *rate > 0 => project.rate = *rate as f64,
            (key, _) => return Err(invalid(format!("unexpected setting {key:?}"))),
        }
    }

    let mut loader = Loader {
        project: dir,
        network: &mut project.network,
        diagnostics: Vec::new(),
    };
    let root = loader.network.root();
    loader.component(&dir.join(ROOT_DIR), root);
    let diagnostics = loader.diagnostics;
    Ok((project, diagnostics))
}

/// Saves a new project, with a `.gitignore` for the rebuildable `.futhark/` cache.
pub fn create(dir: &Path, project: &Project) -> Result<(), ProjectError> {
    save(dir, project)?;
    write_if_changed(&dir.join(".gitignore"), ".futhark/\n")
}

/// Saves a project, writing only files whose contents changed.
pub fn save(dir: &Path, project: &Project) -> Result<(), ProjectError> {
    write(dir, &project.name, project.rate, &project.network)
}

/// Saves a project from its parts, for callers that keep the network elsewhere (such as in a running engine).
pub fn write(dir: &Path, name: &str, rate: f64, network: &Network) -> Result<(), ProjectError> {
    fs::create_dir_all(dir).map_err(|source| ProjectError::Io {
        path: dir.into(),
        source,
    })?;
    let mut settings = String::new();
    writeln!(settings, "name {}", quote(name)).expect("writing to a string");
    writeln!(settings, "rate {}", number(rate)).expect("writing to a string");
    write_if_changed(&dir.join(PROJECT_FILE), &settings)?;
    save_component(&dir.join(ROOT_DIR), network, network.root())
}

struct Loader<'a> {
    project: &'a Path,
    network: &'a mut Network,
    diagnostics: Vec<Diagnostic>,
}

impl Loader<'_> {
    fn diagnose(&mut self, at: impl Into<String>, message: impl fmt::Display) {
        self.diagnostics.push(Diagnostic {
            at: at.into(),
            message: message.to_string(),
        });
    }

    fn file(&self, path: &Path) -> String {
        path.strip_prefix(self.project).unwrap_or(path).display().to_string()
    }

    /// Parses a KDL file, or `None` (with a diagnostic if it exists but can't be read).
    fn read(&mut self, path: &Path) -> Option<KdlDocument> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return None,
            Err(e) => {
                self.diagnose(self.file(path), e);
                return None;
            }
        };
        match KdlDocument::parse_v2(&text) {
            Ok(doc) => Some(doc),
            Err(e) => {
                self.diagnose(self.file(path), e);
                None
            }
        }
    }

    fn component(&mut self, dir: &Path, id: OpId) {
        let at = self.network.path(id);
        let Some(doc) = self.read(&dir.join(COMPONENT_FILE)) else {
            return;
        };

        let mut wires: Vec<(OpId, usize, Option<String>, String)> = Vec::new();
        for node in doc.nodes() {
            let name = node.name().value();
            let path = if at == "/" {
                format!("/{name}")
            } else {
                format!("{at}/{name}")
            };
            let Some(KdlValue::String(ty)) = single_arg(node) else {
                self.diagnose(path, "expected the operator type, such as \"channel/lfo\"");
                continue;
            };
            let op = match self.network.add(id, name, ty) {
                Ok(op) => op,
                Err(e) => {
                    self.diagnose(path, e);
                    continue;
                }
            };
            for line in node.children().map(KdlDocument::nodes).unwrap_or_default() {
                let key = line.name().value();
                let Some(value) = single_arg(line) else {
                    self.diagnose(&path, format!("{key}: expected one value"));
                    continue;
                };
                if key == "input" {
                    let source = match value {
                        KdlValue::String(source) => Some(source.clone()),
                        KdlValue::Null => None,
                        _ => {
                            self.diagnose(&path, "input: expected an operator name");
                            continue;
                        }
                    };
                    wires.push((
                        op,
                        wires.iter().filter(|(o, ..)| *o == op).count(),
                        source,
                        path.clone(),
                    ));
                    continue;
                }
                let value = match value {
                    KdlValue::String(v) => Value::String(v.clone()),
                    KdlValue::Integer(v) => match i64::try_from(*v) {
                        Ok(v) => Value::Int(v),
                        Err(_) => {
                            self.diagnose(&path, format!("{key}: {v} is out of range"));
                            continue;
                        }
                    },
                    KdlValue::Float(v) => Value::Float(*v),
                    KdlValue::Bool(v) => Value::Bool(*v),
                    KdlValue::Null => {
                        self.diagnose(&path, format!("{key}: expected a value"));
                        continue;
                    }
                };
                match self.network.set_param(op, key, value) {
                    Ok(()) => {}
                    Err(GraphError::BadValue { param, message, .. }) => {
                        self.diagnose(&path, format!("{param}: {message}"))
                    }
                    Err(GraphError::UnknownParam { param, .. }) => {
                        self.diagnose(&path, format!("no parameter {param:?}"))
                    }
                    Err(e) => self.diagnose(&path, e),
                }
            }
        }

        for (op, index, source, path) in wires {
            let Some(source) = source else { continue };
            match self.network.child(id, &source) {
                Some(from) => {
                    if let Err(e) = self.network.connect(from, op, index) {
                        self.diagnose(path, e);
                    }
                }
                None => self.diagnose(path, format!("input {index}: no operator named {source}")),
            }
        }

        if let Some(layout) = self.read(&dir.join(LAYOUT_FILE)) {
            for node in layout.nodes() {
                let position = match node.entries().iter().map(|e| e.value()).collect::<Vec<_>>()[..] {
                    [x, y] => number_value(x).zip(number_value(y)),
                    _ => None,
                };
                match (self.network.child(id, node.name().value()), position) {
                    (Some(op), Some((x, y))) => self.network.set_position(op, [x as f32, y as f32]),
                    _ => {
                        let file = self.file(&dir.join(LAYOUT_FILE));
                        self.diagnose(file, format!("ignoring the position of {}", node.name().value()));
                    }
                }
            }
        }

        let components: Vec<_> = self
            .network
            .children(id)
            .filter(|(_, child)| self.network.node(*child).family() == Family::Component)
            .map(|(name, child)| (name.to_string(), child))
            .collect();
        for (name, child) in components {
            self.component(&dir.join(name), child);
        }
    }
}

fn save_component(dir: &Path, network: &Network, id: OpId) -> Result<(), ProjectError> {
    let mut component = String::new();
    let mut layout = String::new();
    for (name, child) in network.children(id) {
        let node = network.node(child);
        let mut lines = Vec::new();
        for input in node.inputs() {
            lines.push(match input {
                Some(input) => format!("input {}", network.node(*input).name()),
                None => "input #null".to_string(),
            });
        }
        for (spec, value) in network.op_type(child).params.iter().zip(node.params()) {
            if *value != spec.default {
                lines.push(format!("{} {}", spec.name, format_value(value)));
            }
        }

        if !component.is_empty() {
            component.push('\n');
        }
        write!(component, "{name} {}", quote(node.ty())).expect("writing to a string");
        if lines.is_empty() {
            component.push('\n');
        } else {
            component.push_str(" {\n");
            for line in lines {
                writeln!(component, "    {line}").expect("writing to a string");
            }
            component.push_str("}\n");
        }

        let [x, y] = node.position();
        writeln!(layout, "{name} {} {}", number(x.into()), number(y.into())).expect("writing to a string");
    }

    fs::create_dir_all(dir).map_err(|source| ProjectError::Io {
        path: dir.into(),
        source,
    })?;
    write_if_changed(&dir.join(COMPONENT_FILE), &component)?;
    write_if_changed(&dir.join(LAYOUT_FILE), &layout)?;
    for (name, child) in network.children(id) {
        if network.node(child).family() == Family::Component {
            save_component(&dir.join(name), network, child)?;
        }
    }
    Ok(())
}

/// Writes through a temporary file and a rename, so a crash never leaves a half-written file.
fn write_if_changed(path: &Path, contents: &str) -> Result<(), ProjectError> {
    if fs::read_to_string(path).is_ok_and(|existing| existing == contents) {
        return Ok(());
    }
    let io = |source| ProjectError::Io {
        path: path.into(),
        source,
    };
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(".tmp");
    fs::write(&temporary, contents).map_err(io)?;
    fs::rename(&temporary, path).map_err(io)
}

/// The node's only argument, if it has exactly one and no properties.
fn single_arg(node: &KdlNode) -> Option<&KdlValue> {
    match node.entries() {
        [entry] if entry.name().is_none() => Some(entry.value()),
        _ => None,
    }
}

fn number_value(value: &KdlValue) -> Option<f64> {
    match value {
        KdlValue::Float(v) => Some(*v),
        KdlValue::Integer(v) => Some(*v as f64),
        _ => None,
    }
}

fn format_value(value: &Value) -> String {
    match value {
        Value::Float(v) => number(*v),
        Value::Int(v) => v.to_string(),
        Value::Bool(v) => format!("#{v}"),
        // Menu items are checked to be plain identifiers, so they are written bare.
        Value::Menu(v) => v.clone(),
        Value::String(v) => quote(v),
    }
}

fn number(v: f64) -> String {
    if v.is_nan() {
        "#nan".into()
    } else if v.is_infinite() {
        if v > 0.0 { "#inf".into() } else { "#-inf".into() }
    } else {
        format!("{v:?}")
    }
}

fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => write!(out, "\\u{{{:x}}}", c as u32).expect("writing to a string"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
