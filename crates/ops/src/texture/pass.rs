//! A full-screen fragment pass over a Texture operator's inputs, shared by the operators that draw with a shader.
//!
//! The WGSL an operator supplies is appended to a generated prelude that declares:
//!
//! - `Varyings` (with `position` in pixels and `uv` from (0, 0) at the top left) and the vertex stage;
//! - `input0`, `input1`, ...: `texture_2d<f32>` at `@group(1) @binding(2 * i)`;
//! - `sampler0`, `sampler1`, ...: a `sampler` per input at `@group(1) @binding(2 * i + 1)`, linear for formats that
//!   can be filtered and nearest otherwise (32-bit float);
//! - `INPUTS`: how many inputs are connected, and `sample_input(index, uv) -> vec4f`, which samples any input by
//!   index (transparent black past the last).
//!
//! The operator's uniforms are at `@group(0) @binding(0)`.

use std::fmt::Write as _;

use futhark_gpu::FULLSCREEN_WGSL;
use futhark_graph::{CookError, Texture, TextureCx};

/// Uniform data, built one 4-byte scalar at a time. WGSL structs made only of `f32`, `u32` and `i32` fields match it
/// field for field.
#[derive(Default)]
pub(crate) struct Uniforms(Vec<u8>);

impl Uniforms {
    pub fn f32(mut self, value: impl Into<f64>) -> Self {
        self.0.extend_from_slice(&(value.into() as f32).to_le_bytes());
        self
    }

    pub fn u32(mut self, value: u32) -> Self {
        self.0.extend_from_slice(&value.to_le_bytes());
        self
    }

    pub fn i32(mut self, value: i32) -> Self {
        self.0.extend_from_slice(&value.to_le_bytes());
        self
    }

    /// The bytes, padded to a multiple of 16 and at least 16 long.
    fn bytes(&self) -> Vec<u8> {
        let mut bytes = self.0.clone();
        bytes.resize(bytes.len().max(16).next_multiple_of(16), 0);
        bytes
    }
}

#[derive(Clone, PartialEq, Eq)]
struct Key {
    format: wgpu::TextureFormat,
    /// Whether each input can be filtered.
    filterable: Vec<bool>,
}

struct Compiled {
    key: Key,
    pipeline: wgpu::RenderPipeline,
    uniforms: wgpu::BindGroupLayout,
    inputs: wgpu::BindGroupLayout,
}

pub(crate) struct Pass {
    label: &'static str,
    source: String,
    entry_point: &'static str,
    compiled: Option<Compiled>,
    /// The last compile error, kept so a broken shader isn't recompiled on every cook.
    failed: Option<(Key, CookError)>,
    uniforms: Option<wgpu::Buffer>,
    samplers: Vec<(wgpu::AddressMode, bool, wgpu::Sampler)>,
}

impl Pass {
    /// A pass running `source`'s fragment entry point `entry_point` over the prelude described in the module docs.
    pub fn new(label: &'static str, source: impl Into<String>, entry_point: &'static str) -> Self {
        Pass {
            label,
            source: source.into(),
            entry_point,
            compiled: None,
            failed: None,
            uniforms: None,
            samplers: Vec::new(),
        }
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    /// Records the pass, drawing every pixel of `target`. Input samplers use `address` outside 0 to 1.
    pub fn run(
        &mut self,
        cx: &mut TextureCx<'_>,
        inputs: &[&Texture],
        uniforms: &Uniforms,
        address: wgpu::AddressMode,
        target: &Texture,
    ) -> Result<(), CookError> {
        let gpu = cx.gpu;
        let key = Key {
            format: target.format(),
            filterable: inputs.iter().map(|input| gpu.filterable(input.format())).collect(),
        };
        if self.compiled.as_ref().is_none_or(|compiled| compiled.key != key) {
            self.compiled = None;
            if let Some((failed, error)) = &self.failed
                && *failed == key
            {
                return Err(error.clone());
            }
            match self.compile(&gpu.device, key.clone()) {
                Ok(compiled) => self.compiled = Some(compiled),
                Err(error) => {
                    self.failed = Some((key, error.clone()));
                    return Err(error);
                }
            }
        }
        let compiled = self.compiled.as_ref().expect("compiled above");

        let bytes = uniforms.bytes();
        if self
            .uniforms
            .as_ref()
            .is_none_or(|buffer| buffer.size() != bytes.len() as u64)
        {
            self.uniforms = Some(gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(self.label),
                size: bytes.len() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
        }
        let buffer = self.uniforms.as_ref().expect("created above");
        gpu.queue.write_buffer(buffer, 0, &bytes);
        let uniforms = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(self.label),
            layout: &compiled.uniforms,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: buffer.as_entire_binding(),
            }],
        });

        let mut samplers = Vec::new();
        for &filterable in &compiled.key.filterable {
            let found = self
                .samplers
                .iter()
                .find(|(a, f, _)| *a == address && *f == filterable)
                .map(|(.., sampler)| sampler.clone());
            samplers.push(found.unwrap_or_else(|| {
                let sampler = gpu.sampler(filterable, address);
                self.samplers.push((address, filterable, sampler.clone()));
                sampler
            }));
        }
        let mut entries = Vec::new();
        for (index, (input, sampler)) in inputs.iter().zip(&samplers).enumerate() {
            entries.push(wgpu::BindGroupEntry {
                binding: 2 * index as u32,
                resource: wgpu::BindingResource::TextureView(input.view()),
            });
            entries.push(wgpu::BindGroupEntry {
                binding: 2 * index as u32 + 1,
                resource: wgpu::BindingResource::Sampler(sampler),
            });
        }
        let inputs = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(self.label),
            layout: &compiled.inputs,
            entries: &entries,
        });

        let mut pass = cx.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some(self.label),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target.view(),
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        pass.set_pipeline(&compiled.pipeline);
        pass.set_bind_group(0, &uniforms, &[]);
        pass.set_bind_group(1, &inputs, &[]);
        pass.draw(0..3, 0..1);
        Ok(())
    }

    fn compile(&self, device: &wgpu::Device, key: Key) -> Result<Compiled, CookError> {
        let prelude = prelude(key.filterable.len());
        let prelude_lines = prelude.lines().count() as u32;
        let source = format!("{prelude}{}", self.source);

        // Errors are caught by the engine's error scope around the cook too; this inner one keeps a broken pipeline
        // out of the cache and reports shader errors at lines of the operator's own source.
        let validation = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let internal = device.push_error_scope(wgpu::ErrorFilter::Internal);
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(self.label),
            source: wgpu::ShaderSource::Wgsl(source.into()),
        });
        let info = pollster::block_on(module.get_compilation_info());
        let messages: Vec<String> = info
            .messages
            .iter()
            .filter(|message| message.message_type == wgpu::CompilationMessageType::Error)
            .map(|message| {
                // naga's report names the position as `wgsl:line:column`, which is more reliable than `location`.
                let position = snippet_position(&message.message).or(message
                    .location
                    .as_ref()
                    .map(|location| (location.line_number, location.line_position)));
                match position {
                    Some((line, column)) if line > prelude_lines => {
                        format!(
                            "line {}:{column}: {}",
                            line - prelude_lines,
                            summarize(&message.message)
                        )
                    }
                    _ => summarize(&message.message),
                }
            })
            .collect();

        let compiled = messages.is_empty().then(|| {
            let uniforms = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some(self.label),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                }],
            });
            let entries: Vec<_> = key
                .filterable
                .iter()
                .enumerate()
                .flat_map(|(index, &filterable)| {
                    [
                        wgpu::BindGroupLayoutEntry {
                            binding: 2 * index as u32,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Texture {
                                sample_type: wgpu::TextureSampleType::Float { filterable },
                                view_dimension: wgpu::TextureViewDimension::D2,
                                multisampled: false,
                            },
                            count: None,
                        },
                        wgpu::BindGroupLayoutEntry {
                            binding: 2 * index as u32 + 1,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Sampler(if filterable {
                                wgpu::SamplerBindingType::Filtering
                            } else {
                                wgpu::SamplerBindingType::NonFiltering
                            }),
                            count: None,
                        },
                    ]
                })
                .collect();
            let inputs = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some(self.label),
                entries: &entries,
            });
            let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(self.label),
                bind_group_layouts: &[Some(&uniforms), Some(&inputs)],
                immediate_size: 0,
            });
            let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(self.label),
                layout: Some(&layout),
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
                    entry_point: Some(self.entry_point),
                    compilation_options: Default::default(),
                    targets: &[Some(key.format.into())],
                }),
                multiview_mask: None,
                cache: None,
            });
            (pipeline, uniforms, inputs)
        });

        let internal = pollster::block_on(internal.pop());
        let validation = pollster::block_on(validation.pop());
        if !messages.is_empty() {
            return Err(CookError::new(messages.join("\n")));
        }
        if let Some(error) = validation.or(internal) {
            return Err(CookError::new(error.to_string()));
        }
        let (pipeline, uniforms, inputs) = compiled.expect("made when there are no messages");
        Ok(Compiled {
            key,
            pipeline,
            uniforms,
            inputs,
        })
    }
}

/// The gist of a naga error: its first line without the "Shader 'label' parsing error:" prefix, and its notes,
/// leaving out the source snippet (whose line numbers count the prelude).
fn summarize(message: &str) -> String {
    let mut lines = message.lines().map(str::trim).filter(|line| !line.is_empty());
    let first = lines.next().unwrap_or_default();
    let first = first.split_once("error: ").map_or(first, |(_, rest)| rest);
    let notes = lines.filter_map(|line| line.strip_prefix("= "));
    std::iter::once(first).chain(notes).collect::<Vec<_>>().join("; ")
}

/// The `line:column` after `wgsl:` in a naga report.
fn snippet_position(message: &str) -> Option<(u32, u32)> {
    let (_, rest) = message.split_once("wgsl:")?;
    let mut numbers = rest.split(|c: char| !c.is_ascii_digit());
    Some((numbers.next()?.parse().ok()?, numbers.next()?.parse().ok()?))
}

fn prelude(inputs: usize) -> String {
    let mut out = String::from(FULLSCREEN_WGSL);
    out.push('\n');
    for index in 0..inputs {
        let (texture, sampler) = (2 * index, 2 * index + 1);
        writeln!(out, "@group(1) @binding({texture}) var input{index}: texture_2d<f32>;").unwrap();
        writeln!(out, "@group(1) @binding({sampler}) var sampler{index}: sampler;").unwrap();
    }
    writeln!(out, "\nconst INPUTS: u32 = {inputs}u;\n").unwrap();
    out.push_str("fn sample_input(index: u32, uv: vec2f) -> vec4f {\n    switch index {\n");
    for index in 0..inputs {
        writeln!(
            out,
            "        case {index}u: {{ return textureSampleLevel(input{index}, sampler{index}, uv, 0.0); }}"
        )
        .unwrap();
    }
    out.push_str("        default: { return vec4f(0.0); }\n    }\n}\n\n");
    out
}
