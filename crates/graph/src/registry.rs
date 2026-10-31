use std::{collections::BTreeMap, ops::RangeInclusive, sync::Arc};

use crate::{ChannelCx, ChannelOp, CookError, Family, Output, ParamSpec, Params, Time, Value};

/// The type name of the plain container component.
pub(crate) const BASE_COMPONENT: &str = "component/base";

/// A registered operator type.
pub struct OpType {
    pub name: &'static str,
    pub label: &'static str,
    pub doc: &'static str,
    pub family: Family,
    pub inputs: RangeInclusive<usize>,
    pub time_dependent: bool,
    pub params: Vec<ParamSpec>,
    pub(crate) create: Option<fn() -> Box<dyn DynOp>>,
}

impl OpType {
    pub fn param(&self, name: &str) -> Option<(usize, &ParamSpec)> {
        self.params.iter().enumerate().find(|(_, spec)| spec.name == name)
    }
}

/// Every operator type the engine knows. Built once and shared.
pub struct Registry {
    types: BTreeMap<&'static str, OpType>,
}

impl Default for Registry {
    fn default() -> Self {
        Self::new()
    }
}

impl Registry {
    /// A registry holding only the built-in base component.
    pub fn new() -> Self {
        let mut registry = Registry { types: BTreeMap::new() };
        registry.insert(OpType {
            name: BASE_COMPONENT,
            label: "Base",
            doc: "A container for a network.",
            family: Family::Component,
            inputs: 0..=0,
            time_dependent: false,
            params: Vec::new(),
            create: None,
        });
        registry
    }

    pub fn add_channel<T: ChannelOp>(&mut self) {
        let spec = T::SPEC;
        self.insert(OpType {
            name: spec.name,
            label: spec.label,
            doc: spec.doc,
            family: Family::Channel,
            inputs: spec.inputs,
            time_dependent: spec.time_dependent,
            params: T::Params::specs(),
            create: Some(|| Box::new(ChannelAdapter(T::default()))),
        });
    }

    fn insert(&mut self, op_type: OpType) {
        let name = op_type.name;
        assert!(
            self.types.insert(name, op_type).is_none(),
            "operator type {name} is registered twice"
        );
    }

    pub fn get(&self, name: &str) -> Option<&OpType> {
        self.types.get(name)
    }

    pub fn iter(&self) -> impl Iterator<Item = &OpType> {
        self.types.values()
    }

    pub fn into_shared(self) -> Arc<Registry> {
        Arc::new(self)
    }
}

/// An operator instance with its family erased.
pub(crate) trait DynOp {
    fn cook(&mut self, time: Time, inputs: &[Output], values: &[Value]) -> Result<Output, CookError>;
}

struct ChannelAdapter<T>(T);

impl<T: ChannelOp> DynOp for ChannelAdapter<T> {
    fn cook(&mut self, time: Time, inputs: &[Output], values: &[Value]) -> Result<Output, CookError> {
        let inputs = inputs
            .iter()
            .map(|input| {
                input
                    .channels()
                    .ok_or_else(|| CookError::new("an input is not a Channel operator"))
            })
            .collect::<Result<_, _>>()?;
        let params = T::Params::from_values(values);
        let channels = self.0.cook(&ChannelCx { time, inputs }, &params)?;
        Ok(Output::Channels(Arc::new(channels)))
    }
}
