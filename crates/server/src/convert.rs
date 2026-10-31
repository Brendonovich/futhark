//! Between engine types and protocol types.

use futhark_graph::{CookError, Family, Network, OpId, ParamKind, Value};
use futhark_protocol as protocol;

pub fn value_in(value: protocol::Value) -> Value {
    match value {
        protocol::Value::Bool(v) => Value::Bool(v),
        protocol::Value::Int(v) => Value::Int(v),
        protocol::Value::Float(v) => Value::Float(v),
        // The parameter's spec turns a string into a menu item where it expects one.
        protocol::Value::String(v) => Value::String(v),
    }
}

pub fn value_out(value: &Value) -> protocol::Value {
    match value {
        Value::Bool(v) => protocol::Value::Bool(*v),
        Value::Int(v) => protocol::Value::Int(*v),
        Value::Float(v) => protocol::Value::Float(*v),
        Value::Menu(v) | Value::String(v) => protocol::Value::String(v.clone()),
    }
}

pub fn family(family: Family) -> protocol::Family {
    match family {
        Family::Texture => protocol::Family::Texture,
        Family::Channel => protocol::Family::Channel,
        Family::Geometry => protocol::Family::Geometry,
        Family::Data => protocol::Family::Data,
        Family::Shader => protocol::Family::Shader,
        Family::Component => protocol::Family::Component,
    }
}

pub fn op(network: &Network, id: OpId, error: Option<&CookError>) -> protocol::Op {
    let node = network.node(id);
    let op_type = network.op_type(id);
    protocol::Op {
        path: network.path(id),
        ty: node.ty().into(),
        family: family(node.family()),
        inputs: node
            .inputs()
            .iter()
            .map(|input| input.map(|i| network.node(i).name().to_string()))
            .collect(),
        params: op_type
            .params
            .iter()
            .zip(node.params())
            .map(|(spec, value)| protocol::Param {
                name: spec.name.into(),
                value: value_out(value),
            })
            .collect(),
        position: node.position(),
        error: error.map(ToString::to_string),
    }
}

pub fn op_type(op_type: &futhark_graph::OpType) -> protocol::OpType {
    protocol::OpType {
        name: op_type.name.into(),
        label: op_type.label.into(),
        doc: op_type.doc.into(),
        family: family(op_type.family),
        min_inputs: *op_type.inputs.start(),
        max_inputs: *op_type.inputs.end(),
        time_dependent: op_type.time_dependent,
        params: op_type
            .params
            .iter()
            .map(|spec| protocol::ParamType {
                name: spec.name.into(),
                label: spec.label.into(),
                doc: spec.doc.into(),
                kind: match spec.kind {
                    ParamKind::Float => protocol::ParamKind::Float,
                    ParamKind::Int => protocol::ParamKind::Int,
                    ParamKind::Toggle => protocol::ParamKind::Toggle,
                    ParamKind::String => protocol::ParamKind::String,
                    ParamKind::Menu(items) => protocol::ParamKind::Menu {
                        items: items
                            .iter()
                            .map(|item| protocol::MenuItem {
                                name: item.name.into(),
                                label: item.label.into(),
                            })
                            .collect(),
                    },
                },
                default: value_out(&spec.default),
                min: spec.min,
                max: spec.max,
                soft_min: spec.soft_min,
                soft_max: spec.soft_max,
            })
            .collect(),
    }
}
