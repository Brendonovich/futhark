use std::{collections::BTreeMap, ops::RangeInclusive, path::Path, sync::Arc};

use crate::{
    ChannelCx, ChannelOp, CookError, Family, Output, ParamSpec, Params, TextureCx, TextureOp, TextureParams, Time,
    Value, engine::GpuState, texture,
};

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

    /// Adds a Texture operator type. Its parameters follow the common ones in [`TextureParams`].
    pub fn add_texture<T: TextureOp>(&mut self) {
        let spec = T::SPEC;
        let mut params = TextureParams::specs();
        params.extend(T::Params::specs());
        self.insert(OpType {
            name: spec.name,
            label: spec.label,
            doc: spec.doc,
            family: Family::Texture,
            inputs: spec.inputs,
            time_dependent: spec.time_dependent,
            params,
            create: Some(|| Box::new(TextureAdapter(T::default()))),
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

/// What the engine hands an operator to cook.
pub(crate) struct CookCx<'a> {
    pub time: Time,
    pub inputs: &'a [Output],
    pub values: &'a [Value],
    pub gpu: Option<&'a mut GpuState>,
    pub project_dir: Option<&'a Path>,
}

/// An operator instance with its family erased.
pub(crate) trait DynOp {
    fn cook(&mut self, cx: CookCx<'_>) -> Result<Output, CookError>;
}

struct ChannelAdapter<T>(T);

impl<T: ChannelOp> DynOp for ChannelAdapter<T> {
    fn cook(&mut self, cx: CookCx<'_>) -> Result<Output, CookError> {
        let inputs = cx
            .inputs
            .iter()
            .map(|input| {
                input
                    .channels()
                    .ok_or_else(|| CookError::new("an input is not a Channel operator"))
            })
            .collect::<Result<_, _>>()?;
        let params = T::Params::from_values(cx.values);
        let channels = self.0.cook(&ChannelCx { time: cx.time, inputs }, &params)?;
        Ok(Output::Channels(Arc::new(channels)))
    }
}

struct TextureAdapter<T>(T);

impl<T: TextureOp> DynOp for TextureAdapter<T> {
    fn cook(&mut self, cx: CookCx<'_>) -> Result<Output, CookError> {
        let Some(state) = cx.gpu else {
            return Err(CookError::new(
                "Texture operators need a GPU, and this engine has none (see Engine::with_gpu)",
            ));
        };
        let inputs: Vec<_> = cx
            .inputs
            .iter()
            .map(|input| {
                input
                    .texture()
                    .ok_or_else(|| CookError::new("an input is not a Texture operator"))
            })
            .collect::<Result<_, _>>()?;
        let (common, own) = cx.values.split_at(TextureParams::specs().len());
        let common = TextureParams::from_values(common);
        let params = T::Params::from_values(own);
        let (width, height, format) = texture::resolve(&common, inputs.first().copied());

        let gpu = &*state.gpu;
        let scope = gpu.error_scope();
        let mut encoder = gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some(T::SPEC.name),
        });
        let mut texture_cx = TextureCx {
            time: cx.time,
            gpu,
            encoder: &mut encoder,
            width,
            height,
            format,
            common,
            inputs,
            pool: &mut state.pool,
            project_dir: cx.project_dir,
        };
        let result = self.0.cook(&mut texture_cx, &params);
        let commands = result.is_ok().then(|| encoder.finish());
        let errors = scope.finish();
        let texture = result?;
        errors.map_err(|error| CookError::new(format!("GPU error: {error}")))?;
        state.pending.extend(commands);
        Ok(Output::Texture(texture))
    }
}
