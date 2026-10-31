use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};

use crate::{Family, OpType, Registry, Value, registry::BASE_COMPONENT};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct OpId(u32);

#[derive(Debug, PartialEq, thiserror::Error)]
pub enum GraphError {
    #[error("{0:?} is not a valid name: use letters, digits and underscores, not starting with a digit")]
    InvalidName(String),
    #[error("{0} already exists")]
    DuplicateName(String),
    #[error("unknown operator type {0:?}")]
    UnknownType(String),
    #[error("{0} is not a component")]
    NotAComponent(String),
    #[error("no parameter {param:?} on {op}")]
    UnknownParam { op: String, param: String },
    #[error("{param} on {op}: {message}")]
    BadValue { op: String, param: String, message: String },
    #[error("can't wire {from} ({from_family}) into {to} ({to_family}): wires connect operators of the same family")]
    FamilyMismatch {
        from: String,
        from_family: Family,
        to: String,
        to_family: Family,
    },
    #[error("can't wire {from} into {to}: wires connect operators in the same component")]
    NotSiblings { from: String, to: String },
    #[error("{op} takes at most {max} inputs")]
    TooManyInputs { op: String, max: usize },
    #[error("wiring {from} into {to} would make a cycle")]
    Cycle { from: String, to: String },
    #[error("the root component can't be changed")]
    Root,
}

/// One operator in the network.
pub struct Node {
    name: String,
    parent: Option<OpId>,
    ty: &'static str,
    family: Family,
    params: Vec<Value>,
    inputs: Vec<Option<OpId>>,
    children: BTreeMap<String, OpId>,
    position: [f32; 2],
    /// Network revision of the last change that affects this operator's output.
    pub(crate) revision: u64,
}

impl Node {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn parent(&self) -> Option<OpId> {
        self.parent
    }

    /// The operator type's name, such as `channel/lfo`.
    pub fn ty(&self) -> &'static str {
        self.ty
    }

    pub fn family(&self) -> Family {
        self.family
    }

    /// Parameter values, in the order of the type's parameter specs.
    pub fn params(&self) -> &[Value] {
        &self.params
    }

    /// Wired inputs by index. `None` is an unconnected slot before a connected one.
    pub fn inputs(&self) -> &[Option<OpId>] {
        &self.inputs
    }

    /// Position in the network editor.
    pub fn position(&self) -> [f32; 2] {
        self.position
    }
}

/// Operators names: ASCII letters, digits and underscores, not starting with a digit. These are also valid bare KDL
/// identifiers, except for the few KDL keywords, which are rejected.
pub fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !matches!(name, "true" | "false" | "null" | "inf" | "nan")
}

/// The operators of a project, nested in components, with their parameters and wires.
pub struct Network {
    registry: Arc<Registry>,
    nodes: HashMap<OpId, Node>,
    root: OpId,
    next_id: u32,
    pub(crate) revision: u64,
}

impl Network {
    pub fn new(registry: Arc<Registry>) -> Self {
        let root = OpId(0);
        let node = Node {
            name: String::new(),
            parent: None,
            ty: BASE_COMPONENT,
            family: Family::Component,
            params: Vec::new(),
            inputs: Vec::new(),
            children: BTreeMap::new(),
            position: [0.0; 2],
            revision: 0,
        };
        Network {
            registry,
            nodes: HashMap::from([(root, node)]),
            root,
            next_id: 1,
            revision: 0,
        }
    }

    pub fn registry(&self) -> &Arc<Registry> {
        &self.registry
    }

    pub fn root(&self) -> OpId {
        self.root
    }

    pub fn get(&self, id: OpId) -> Option<&Node> {
        self.nodes.get(&id)
    }

    /// Panics if `id` was removed.
    pub fn node(&self, id: OpId) -> &Node {
        &self.nodes[&id]
    }

    pub fn op_type(&self, id: OpId) -> &OpType {
        self.registry
            .get(self.node(id).ty)
            .expect("nodes have registered types")
    }

    /// The operators inside a component, sorted by name.
    pub fn children(&self, id: OpId) -> impl Iterator<Item = (&str, OpId)> {
        self.node(id).children.iter().map(|(name, id)| (name.as_str(), *id))
    }

    pub fn child(&self, parent: OpId, name: &str) -> Option<OpId> {
        self.node(parent).children.get(name).copied()
    }

    /// The absolute path, such as `/fx/noise1`. The root is `/`.
    pub fn path(&self, id: OpId) -> String {
        let mut names = Vec::new();
        let mut at = id;
        while let Some(parent) = self.node(at).parent {
            names.push(self.node(at).name.as_str());
            at = parent;
        }
        names.reverse();
        format!("/{}", names.join("/"))
    }

    /// Finds an operator by absolute path.
    pub fn find(&self, path: &str) -> Option<OpId> {
        let rest = path.strip_prefix('/')?;
        rest.split('/')
            .filter(|name| !name.is_empty())
            .try_fold(self.root, |at, name| self.node(at).children.get(name).copied())
    }

    pub fn add(&mut self, parent: OpId, name: &str, ty: &str) -> Result<OpId, GraphError> {
        if !valid_name(name) {
            return Err(GraphError::InvalidName(name.into()));
        }
        let op_type = self
            .registry
            .get(ty)
            .ok_or_else(|| GraphError::UnknownType(ty.into()))?;
        let parent_node = self.node(parent);
        if parent_node.family != Family::Component {
            return Err(GraphError::NotAComponent(self.path(parent)));
        }
        if parent_node.children.contains_key(name) {
            return Err(GraphError::DuplicateName(self.join(parent, name)));
        }
        let id = OpId(self.next_id);
        self.next_id += 1;
        self.revision += 1;
        let node = Node {
            name: name.into(),
            parent: Some(parent),
            ty: op_type.name,
            family: op_type.family,
            params: op_type.params.iter().map(|spec| spec.default.clone()).collect(),
            inputs: Vec::new(),
            children: BTreeMap::new(),
            position: [0.0; 2],
            revision: self.revision,
        };
        self.nodes.insert(id, node);
        self.nodes
            .get_mut(&parent)
            .expect("checked above")
            .children
            .insert(name.into(), id);
        Ok(id)
    }

    /// Removes an operator, and everything inside it if it is a component. Wires from it are disconnected.
    pub fn remove(&mut self, id: OpId) -> Result<(), GraphError> {
        let parent = self.node(id).parent.ok_or(GraphError::Root)?;
        let mut doomed = vec![id];
        let mut index = 0;
        while let Some(&at) = doomed.get(index) {
            doomed.extend(self.node(at).children.values().copied());
            index += 1;
        }
        let name = self.node(id).name.clone();
        self.nodes
            .get_mut(&parent)
            .expect("parents exist")
            .children
            .remove(&name);
        for at in &doomed {
            self.nodes.remove(at);
        }
        self.revision += 1;
        let revision = self.revision;
        for node in self.nodes.values_mut() {
            let mut changed = false;
            for input in node
                .inputs
                .iter_mut()
                .filter(|input| input.is_some_and(|i| doomed.contains(&i)))
            {
                *input = None;
                changed = true;
            }
            if changed {
                trim(&mut node.inputs);
                node.revision = revision;
            }
        }
        Ok(())
    }

    pub fn rename(&mut self, id: OpId, name: &str) -> Result<(), GraphError> {
        let parent = self.node(id).parent.ok_or(GraphError::Root)?;
        if !valid_name(name) {
            return Err(GraphError::InvalidName(name.into()));
        }
        if self.node(parent).children.contains_key(name) {
            return Err(GraphError::DuplicateName(self.join(parent, name)));
        }
        let old = std::mem::replace(&mut self.nodes.get_mut(&id).expect("exists").name, name.into());
        let children = &mut self.nodes.get_mut(&parent).expect("parents exist").children;
        children.remove(&old);
        children.insert(name.into(), id);
        Ok(())
    }

    pub fn param(&self, id: OpId, name: &str) -> Option<&Value> {
        let (index, _) = self.op_type(id).param(name)?;
        Some(&self.node(id).params[index])
    }

    /// Sets a parameter, checking its type and clamping it to its limits.
    pub fn set_param(&mut self, id: OpId, name: &str, value: Value) -> Result<(), GraphError> {
        let op = || self.path(id);
        let (index, spec) = self.op_type(id).param(name).ok_or_else(|| GraphError::UnknownParam {
            op: op(),
            param: name.into(),
        })?;
        let value = spec.coerce(value).map_err(|message| GraphError::BadValue {
            op: op(),
            param: name.into(),
            message,
        })?;
        self.revision += 1;
        let node = self.nodes.get_mut(&id).expect("exists");
        if node.params[index] != value {
            node.params[index] = value;
            node.revision = self.revision;
        }
        Ok(())
    }

    /// Wires `from`'s output into input `index` of `to`.
    pub fn connect(&mut self, from: OpId, to: OpId, index: usize) -> Result<(), GraphError> {
        let (from_node, to_node) = (self.node(from), self.node(to));
        if from_node.family != to_node.family || to_node.family == Family::Component {
            return Err(GraphError::FamilyMismatch {
                from: self.path(from),
                from_family: from_node.family,
                to: self.path(to),
                to_family: to_node.family,
            });
        }
        if from_node.parent != to_node.parent {
            return Err(GraphError::NotSiblings {
                from: self.path(from),
                to: self.path(to),
            });
        }
        let max = *self.op_type(to).inputs.end();
        if index >= max {
            return Err(GraphError::TooManyInputs { op: self.path(to), max });
        }
        if self.upstream(from, to) {
            return Err(GraphError::Cycle {
                from: self.path(from),
                to: self.path(to),
            });
        }
        self.revision += 1;
        let node = self.nodes.get_mut(&to).expect("exists");
        if node.inputs.len() <= index {
            node.inputs.resize(index + 1, None);
        }
        node.inputs[index] = Some(from);
        node.revision = self.revision;
        Ok(())
    }

    pub fn disconnect(&mut self, to: OpId, index: usize) {
        self.revision += 1;
        let node = self.nodes.get_mut(&to).expect("exists");
        if let Some(input) = node.inputs.get_mut(index) {
            *input = None;
            trim(&mut node.inputs);
            node.revision = self.revision;
        }
    }

    pub fn set_position(&mut self, id: OpId, position: [f32; 2]) {
        self.nodes.get_mut(&id).expect("exists").position = position;
    }

    /// Whether `target` is `at` or feeds into it.
    fn upstream(&self, at: OpId, target: OpId) -> bool {
        at == target
            || self
                .node(at)
                .inputs
                .iter()
                .flatten()
                .any(|&input| self.upstream(input, target))
    }

    fn join(&self, parent: OpId, name: &str) -> String {
        let path = self.path(parent);
        if path == "/" {
            format!("/{name}")
        } else {
            format!("{path}/{name}")
        }
    }
}

fn trim(inputs: &mut Vec<Option<OpId>>) {
    while inputs.last() == Some(&None) {
        inputs.pop();
    }
}
