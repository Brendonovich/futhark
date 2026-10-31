use std::collections::HashMap;

use wgpu::util::DeviceExt as _;

use crate::Gpu;

/// WGSL for a full-screen triangle: a vertex entry point `vs_fullscreen` drawn with three vertices and no buffers,
/// and the `Varyings` it passes on. `uv` runs from (0, 0) at the top left to (1, 1) at the bottom right, matching
/// texture coordinates.
pub const FULLSCREEN_WGSL: &str = "struct Varyings {
    @builtin(position) position: vec4f,
    @location(0) uv: vec2f,
}

@vertex
fn vs_fullscreen(@builtin(vertex_index) index: u32) -> Varyings {
    let xy = vec2f(f32((index << 1u) & 2u), f32(index & 2u));
    var out: Varyings;
    out.position = vec4f(xy * 2.0 - 1.0, 0.0, 1.0);
    out.uv = vec2f(xy.x, 1.0 - xy.y);
    return out;
}
";

const DRAW_SCALED_WGSL: &str = "
struct Placement {
    scale: vec2f,
    offset: vec2f,
}

@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var source_sampler: sampler;
@group(0) @binding(2) var<uniform> placement: Placement;

@fragment
fn fs_main(in: Varyings) -> @location(0) vec4f {
    let uv = (in.uv - placement.offset) / placement.scale;
    if any(uv < vec2f(0.0)) || any(uv > vec2f(1.0)) {
        return vec4f(0.0);
    }
    return textureSampleLevel(source, source_sampler, uv, 0.0);
}
";

/// How [`Gpu::draw_scaled`] fits a texture into a target of another shape.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Fit {
    /// All of the source is shown at its own aspect ratio, with transparent bars filling the rest.
    #[default]
    Contain,
    /// The target is filled at the source's aspect ratio, cropping the source.
    Cover,
    /// The source is stretched to the target's shape.
    Stretch,
}

impl Fit {
    /// Where the source lands in target UV space, as a scale and an offset.
    fn placement(self, source: (u32, u32), target: (u32, u32)) -> [f32; 4] {
        let source_aspect = source.0 as f32 / source.1 as f32;
        let target_aspect = target.0 as f32 / target.1 as f32;
        // The source's size relative to the target's.
        let (w, h) = match self {
            Fit::Stretch => (1.0, 1.0),
            Fit::Contain if source_aspect > target_aspect => (1.0, target_aspect / source_aspect),
            Fit::Contain => (source_aspect / target_aspect, 1.0),
            Fit::Cover if source_aspect > target_aspect => (source_aspect / target_aspect, 1.0),
            Fit::Cover => (1.0, target_aspect / source_aspect),
        };
        [w, h, (1.0 - w) / 2.0, (1.0 - h) / 2.0]
    }
}

/// Pipelines for `draw_scaled`, by target format and whether the source can be filtered.
#[derive(Default)]
pub(crate) struct Drawer {
    pipelines: HashMap<(wgpu::TextureFormat, bool), (wgpu::RenderPipeline, wgpu::BindGroupLayout)>,
    samplers: HashMap<bool, wgpu::Sampler>,
}

impl Gpu {
    /// Records a draw of `source` into all of `target`, scaled to fit. Areas the source doesn't cover are cleared to
    /// transparent black. Sampling is linear, or nearest for formats that can't be filtered (such as `Rgba32Float`).
    ///
    /// The target needs `RENDER_ATTACHMENT` usage and the source `TEXTURE_BINDING`. Any renderable color format works
    /// as a target; viewers use `Bgra8Unorm`.
    pub fn draw_scaled(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        source: &wgpu::TextureView,
        target: &wgpu::TextureView,
        fit: Fit,
    ) {
        let (width, height) = (target.texture().width(), target.texture().height());
        self.draw_scaled_into(encoder, source, target, [0, 0, width, height], fit, true);
    }

    /// Like [`draw_scaled`](Self::draw_scaled), into the rectangle `[x, y, width, height]` of `target` only. With
    /// `clear` the whole target is cleared first; without it the rest of the target keeps its contents, which is how
    /// an atlas of previews is drawn one rectangle at a time.
    pub fn draw_scaled_into(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        source: &wgpu::TextureView,
        target: &wgpu::TextureView,
        [x, y, width, height]: [u32; 4],
        fit: Fit,
        clear: bool,
    ) {
        let src = source.texture();
        let dst = target.texture();
        let filterable = self.filterable(src.format());
        let (pipeline, layout, sampler) = {
            let mut drawer = self.drawer.lock().unwrap_or_else(|e| e.into_inner());
            let (pipeline, layout) = drawer
                .pipelines
                .entry((dst.format(), filterable))
                .or_insert_with(|| self.draw_scaled_pipeline(dst.format(), filterable))
                .clone();
            let sampler = drawer
                .samplers
                .entry(filterable)
                .or_insert_with(|| self.sampler(filterable, wgpu::AddressMode::ClampToEdge))
                .clone();
            (pipeline, layout, sampler)
        };

        let placement = fit.placement((src.width(), src.height()), (width.max(1), height.max(1)));
        let placement = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("draw_scaled placement"),
            contents: &placement.map(f32::to_le_bytes).concat(),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("draw_scaled"),
            layout: &layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(source),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: placement.as_entire_binding(),
                },
            ],
        });
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("draw_scaled"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: if clear {
                        wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT)
                    } else {
                        wgpu::LoadOp::Load
                    },
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        if width == 0 || height == 0 {
            return;
        }
        pass.set_viewport(x as f32, y as f32, width as f32, height as f32, 0.0, 1.0);
        pass.set_scissor_rect(x, y, width, height);
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.draw(0..3, 0..1);
    }

    /// Whether textures of `format` can be sampled with linear filtering on this device.
    pub fn filterable(&self, format: wgpu::TextureFormat) -> bool {
        matches!(
            format.sample_type(None, Some(self.device.features())),
            Some(wgpu::TextureSampleType::Float { filterable: true })
        )
    }

    /// A linear sampler, or a nearest one when `filterable` is false.
    pub fn sampler(&self, filterable: bool, address: wgpu::AddressMode) -> wgpu::Sampler {
        let filter = if filterable {
            wgpu::FilterMode::Linear
        } else {
            wgpu::FilterMode::Nearest
        };
        self.device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some(if filterable { "linear" } else { "nearest" }),
            address_mode_u: address,
            address_mode_v: address,
            address_mode_w: address,
            mag_filter: filter,
            min_filter: filter,
            ..Default::default()
        })
    }

    fn draw_scaled_pipeline(
        &self,
        format: wgpu::TextureFormat,
        filterable: bool,
    ) -> (wgpu::RenderPipeline, wgpu::BindGroupLayout) {
        let device = &self.device;
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("draw_scaled"),
            source: wgpu::ShaderSource::Wgsl(format!("{FULLSCREEN_WGSL}{DRAW_SCALED_WGSL}").into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("draw_scaled"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(if filterable {
                        wgpu::SamplerBindingType::Filtering
                    } else {
                        wgpu::SamplerBindingType::NonFiltering
                    }),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("draw_scaled"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("draw_scaled"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs_fullscreen"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(format.into())],
            }),
            multiview_mask: None,
            cache: None,
        });
        (pipeline, layout)
    }
}

#[cfg(test)]
mod tests {
    use super::Fit;

    #[test]
    fn placements() {
        assert_eq!(Fit::Contain.placement((200, 100), (100, 100)), [1.0, 0.5, 0.0, 0.25]);
        assert_eq!(Fit::Contain.placement((100, 200), (100, 100)), [0.5, 1.0, 0.25, 0.0]);
        assert_eq!(Fit::Cover.placement((200, 100), (100, 100)), [2.0, 1.0, -0.5, 0.0]);
        assert_eq!(Fit::Stretch.placement((200, 100), (100, 100)), [1.0, 1.0, 0.0, 0.0]);
    }
}
