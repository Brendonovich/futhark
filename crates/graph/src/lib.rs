//! The network model, parameters, and the pull-based cook engine. Nothing here knows about the GPU or the UI.

// Lets the derives' `::futhark_graph::...` paths resolve inside this crate's own tests.
extern crate self as futhark_graph;

mod channel;
mod engine;
mod network;
mod op;
mod param;
mod registry;

pub use channel::{Channel, Channels};
pub use engine::{Clock, Engine};
pub use futhark_macros::{Menu, Params};
pub use network::{GraphError, Network, Node, OpId, valid_name};
pub use op::{ChannelCx, ChannelOp, CookError, Family, OpSpec, Output, Time};
pub use param::{MenuItem, ParamKind, ParamSpec, ParamType, Params, Value};
pub use registry::{OpType, Registry};
