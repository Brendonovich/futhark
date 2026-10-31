use std::{fmt, ops::Range, ops::RangeInclusive, sync::Arc};

use futhark_gpu::Texture;

use crate::{Channels, Params};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Family {
    Texture,
    Channel,
    Geometry,
    Data,
    Shader,
    Component,
}

impl fmt::Display for Family {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Family::Texture => "Texture",
            Family::Channel => "Channel",
            Family::Geometry => "Geometry",
            Family::Data => "Data",
            Family::Shader => "Shader",
            Family::Component => "Component",
        })
    }
}

/// What an operator type declares about itself.
pub struct OpSpec {
    /// The type's name in project files, `family/name`, such as `channel/lfo`.
    pub name: &'static str,
    pub label: &'static str,
    pub doc: &'static str,
    /// How many inputs may be connected.
    pub inputs: RangeInclusive<usize>,
    /// Whether the output changes with time alone, so the operator cooks on every frame it is pulled.
    pub time_dependent: bool,
}

/// The engine's clock as an operator sees it while cooking.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Time {
    pub frame: i64,
    /// Frames per second.
    pub rate: f64,
    /// The first frame this cook covers. For a time-dependent operator it is the frame after its previous cook, so
    /// consecutive cooks cover every frame once even when frames are skipped (up to 200 ms). Otherwise it is `frame`.
    pub slice_start: i64,
}

impl Time {
    pub fn seconds(&self) -> f64 {
        self.seconds_at(self.frame)
    }

    pub fn seconds_at(&self, frame: i64) -> f64 {
        frame as f64 / self.rate
    }

    /// The frames this cook covers.
    pub fn slice(&self) -> Range<i64> {
        self.slice_start..self.frame + 1
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CookError(pub String);

impl CookError {
    pub fn new(message: impl Into<String>) -> Self {
        CookError(message.into())
    }
}

impl fmt::Display for CookError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for CookError {}

/// The result of cooking an operator. Cheap to clone.
#[derive(Clone, Debug, PartialEq)]
pub enum Output {
    Channels(Arc<Channels>),
    /// A texture on the engine's GPU. Its work has been submitted to the queue by the time `Engine::pull` returns.
    Texture(Texture),
}

impl Output {
    pub fn channels(&self) -> Option<&Channels> {
        match self {
            Output::Channels(channels) => Some(channels),
            _ => None,
        }
    }

    pub fn texture(&self) -> Option<&Texture> {
        match self {
            Output::Texture(texture) => Some(texture),
            _ => None,
        }
    }
}

pub struct ChannelCx<'a> {
    pub time: Time,
    pub(crate) inputs: Vec<&'a Channels>,
}

impl ChannelCx<'_> {
    /// The connected inputs, in order.
    pub fn inputs(&self) -> &[&Channels] {
        &self.inputs
    }
}

/// An operator of the Channel family.
pub trait ChannelOp: Default + 'static {
    type Params: Params;
    const SPEC: OpSpec;

    fn cook(&mut self, cx: &ChannelCx<'_>, params: &Self::Params) -> Result<Channels, CookError>;
}
