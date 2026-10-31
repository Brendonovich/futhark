//! Texture operators. Every one has the common `width`, `height` and `format` parameters before its own (see
//! [`futhark_graph::TextureParams`]).

mod file_in;
mod pass;
mod shader;

pub use file_in::{FileIn, FileInParams};
pub use shader::{DEFAULT_SHADER, Shader, ShaderParams};

use futhark_graph::{CookError, Menu, OpSpec, Params, Texture, TextureCx, TextureOp};
use pass::{Pass, Uniforms};

#[derive(Params)]
pub struct ConstantParams {
    #[param(default = 1.0, soft_min = 0.0, soft_max = 1.0, label = "Red")]
    pub r: f32,
    #[param(default = 1.0, soft_min = 0.0, soft_max = 1.0, label = "Green")]
    pub g: f32,
    #[param(default = 1.0, soft_min = 0.0, soft_max = 1.0, label = "Blue")]
    pub b: f32,
    #[param(default = 1.0, soft_min = 0.0, soft_max = 1.0, label = "Alpha")]
    pub a: f32,
}

#[derive(Default)]
pub struct Constant;

impl TextureOp for Constant {
    type Params = ConstantParams;
    const SPEC: OpSpec = OpSpec {
        name: "texture/constant",
        label: "Constant",
        doc: "Every pixel one color.",
        inputs: 0..=0,
        time_dependent: false,
    };

    fn cook(&mut self, cx: &mut TextureCx<'_>, p: &ConstantParams) -> Result<Texture, CookError> {
        let target = cx.target();
        cx.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("texture/constant"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target.view(),
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: p.r.into(),
                        g: p.g.into(),
                        b: p.b.into(),
                        a: p.a.into(),
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        Ok(target)
    }
}

#[derive(Menu, Clone, Copy, Debug, PartialEq)]
pub enum RampKind {
    /// Left to right.
    Horizontal,
    /// Bottom to top.
    Vertical,
    /// From the center out to the nearest edge.
    Radial,
}

#[derive(Params)]
pub struct RampParams {
    pub kind: RampKind,
    #[param(soft_min = 0.0, soft_max = 1.0, label = "Start Red")]
    pub start_r: f32,
    #[param(soft_min = 0.0, soft_max = 1.0, label = "Start Green")]
    pub start_g: f32,
    #[param(soft_min = 0.0, soft_max = 1.0, label = "Start Blue")]
    pub start_b: f32,
    #[param(default = 1.0, soft_min = 0.0, soft_max = 1.0, label = "Start Alpha")]
    pub start_a: f32,
    #[param(default = 1.0, soft_min = 0.0, soft_max = 1.0, label = "End Red")]
    pub end_r: f32,
    #[param(default = 1.0, soft_min = 0.0, soft_max = 1.0, label = "End Green")]
    pub end_g: f32,
    #[param(default = 1.0, soft_min = 0.0, soft_max = 1.0, label = "End Blue")]
    pub end_b: f32,
    #[param(default = 1.0, soft_min = 0.0, soft_max = 1.0, label = "End Alpha")]
    pub end_a: f32,
}

const RAMP_WGSL: &str = "
struct Params {
    kind: u32,
    aspect: f32,
    start_r: f32, start_g: f32, start_b: f32, start_a: f32,
    end_r: f32, end_g: f32, end_b: f32, end_a: f32,
}

@group(0) @binding(0) var<uniform> params: Params;

@fragment
fn fs_main(in: Varyings) -> @location(0) vec4f {
    var t: f32;
    switch params.kind {
        case 0u: { t = in.uv.x; }
        case 1u: { t = 1.0 - in.uv.y; }
        default: {
            let d = (in.uv - 0.5) * vec2f(params.aspect, 1.0);
            t = length(d) * 2.0 / min(params.aspect, 1.0);
        }
    }
    let start = vec4f(params.start_r, params.start_g, params.start_b, params.start_a);
    let end = vec4f(params.end_r, params.end_g, params.end_b, params.end_a);
    return mix(start, end, clamp(t, 0.0, 1.0));
}
";

pub struct Ramp(Pass);

impl Default for Ramp {
    fn default() -> Self {
        Ramp(Pass::new("texture/ramp", RAMP_WGSL, "fs_main"))
    }
}

impl TextureOp for Ramp {
    type Params = RampParams;
    const SPEC: OpSpec = OpSpec {
        name: "texture/ramp",
        label: "Ramp",
        doc: "A gradient between two colors.",
        inputs: 0..=0,
        time_dependent: false,
    };

    fn cook(&mut self, cx: &mut TextureCx<'_>, p: &RampParams) -> Result<Texture, CookError> {
        let uniforms = Uniforms::default()
            .u32(p.kind as u32)
            .f32(cx.width as f32 / cx.height as f32)
            .f32(p.start_r)
            .f32(p.start_g)
            .f32(p.start_b)
            .f32(p.start_a)
            .f32(p.end_r)
            .f32(p.end_g)
            .f32(p.end_b)
            .f32(p.end_a);
        let target = cx.target();
        self.0
            .run(cx, &[], &uniforms, wgpu::AddressMode::ClampToEdge, &target)?;
        Ok(target)
    }
}

#[derive(Params)]
pub struct NoiseParams {
    pub seed: i32,
    /// Distance between noise features, in image heights.
    #[param(default = 0.25, min = 0.001, soft_max = 2.0)]
    pub period: f32,
    /// Layers of finer noise added on top.
    #[param(default = 2, min = 1, max = 8)]
    pub octaves: i32,
    #[param(default = 0.5, soft_min = 0.0, soft_max = 1.0)]
    pub amplitude: f32,
    #[param(default = 0.5, soft_min = -1.0, soft_max = 1.0)]
    pub offset: f32,
    /// How fast the noise changes, in noise features per second.
    #[param(default = 0.5, soft_min = 0.0, soft_max = 4.0)]
    pub speed: f32,
    /// One noise in every color channel, rather than a different one in each.
    #[param(default = true)]
    pub monochrome: bool,
}

const NOISE_WGSL: &str = "
struct Params {
    seed: u32,
    octaves: u32,
    monochrome: u32,
    period: f32,
    amplitude: f32,
    offset: f32,
    time: f32,
    aspect: f32,
}

@group(0) @binding(0) var<uniform> params: Params;

fn hash(x: u32) -> u32 {
    var h = x * 747796405u + 2891336453u;
    h = ((h >> ((h >> 28u) + 4u)) ^ h) * 277803737u;
    return (h >> 22u) ^ h;
}

// A random value from -1 to 1 for each lattice point.
fn lattice(seed: u32, cell: vec3i) -> f32 {
    let h = hash(seed ^ hash(bitcast<u32>(cell.x) ^ hash(bitcast<u32>(cell.y) ^ hash(bitcast<u32>(cell.z)))));
    return f32(h >> 8u) / 8388607.5 - 1.0;
}

fn value_noise(seed: u32, p: vec3f) -> f32 {
    let base = floor(p);
    let t = p - base;
    let s = t * t * (3.0 - 2.0 * t);
    let c = vec3i(base);
    let x00 = mix(lattice(seed, c), lattice(seed, c + vec3i(1, 0, 0)), s.x);
    let x10 = mix(lattice(seed, c + vec3i(0, 1, 0)), lattice(seed, c + vec3i(1, 1, 0)), s.x);
    let x01 = mix(lattice(seed, c + vec3i(0, 0, 1)), lattice(seed, c + vec3i(1, 0, 1)), s.x);
    let x11 = mix(lattice(seed, c + vec3i(0, 1, 1)), lattice(seed, c + vec3i(1, 1, 1)), s.x);
    return mix(mix(x00, x10, s.y), mix(x01, x11, s.y), s.z);
}

fn fractal(seed: u32, p: vec3f) -> f32 {
    var sum = 0.0;
    var weight = 1.0;
    var total = 0.0;
    var scale = 1.0;
    for (var octave = 0u; octave < params.octaves; octave++) {
        sum += weight * value_noise(hash(seed + octave), p * scale);
        total += weight;
        weight *= 0.5;
        scale *= 2.0;
    }
    return sum / total;
}

@fragment
fn fs_main(in: Varyings) -> @location(0) vec4f {
    let p = vec3f(vec2f(in.uv.x * params.aspect, in.uv.y) / params.period, params.time);
    var value: vec3f;
    if params.monochrome != 0u {
        value = vec3f(fractal(params.seed, p));
    } else {
        value = vec3f(
            fractal(params.seed, p),
            fractal(params.seed ^ 0x9e3779b9u, p),
            fractal(params.seed ^ 0x3c6ef372u, p),
        );
    }
    return vec4f(value * params.amplitude + params.offset, 1.0);
}
";

pub struct Noise(Pass);

impl Default for Noise {
    fn default() -> Self {
        Noise(Pass::new("texture/noise", NOISE_WGSL, "fs_main"))
    }
}

impl TextureOp for Noise {
    type Params = NoiseParams;
    const SPEC: OpSpec = OpSpec {
        name: "texture/noise",
        label: "Noise",
        doc: "Smooth random values that change over time, from -1 to 1 before amplitude and offset. Alpha is 1.",
        inputs: 0..=0,
        time_dependent: true,
    };

    fn cook(&mut self, cx: &mut TextureCx<'_>, p: &NoiseParams) -> Result<Texture, CookError> {
        let uniforms = Uniforms::default()
            .u32(p.seed as u32)
            .u32(p.octaves as u32)
            .u32(p.monochrome.into())
            .f32(p.period)
            .f32(p.amplitude)
            .f32(p.offset)
            .f32(cx.time.seconds() * f64::from(p.speed))
            .f32(cx.width as f32 / cx.height as f32);
        let target = cx.target();
        self.0
            .run(cx, &[], &uniforms, wgpu::AddressMode::ClampToEdge, &target)?;
        Ok(target)
    }
}

#[derive(Menu, Clone, Copy, Debug, PartialEq)]
pub enum Extend {
    /// The edge pixels continue outward.
    Hold,
    /// Transparent black.
    Zero,
    Repeat,
    /// Repeats, flipping every other copy.
    Mirror,
}

#[derive(Params)]
pub struct TransformParams {
    /// Horizontal move, in image widths.
    #[param(soft_min = -1.0, soft_max = 1.0)]
    pub translate_x: f32,
    /// Vertical move, in image heights. Positive is up.
    #[param(soft_min = -1.0, soft_max = 1.0)]
    pub translate_y: f32,
    #[param(default = 1.0, soft_min = 0.0, soft_max = 4.0)]
    pub scale_x: f32,
    #[param(default = 1.0, soft_min = 0.0, soft_max = 4.0)]
    pub scale_y: f32,
    /// Counterclockwise rotation in degrees, around the center.
    #[param(soft_min = -180.0, soft_max = 180.0)]
    pub rotate: f32,
    /// What fills the area outside the input.
    pub extend: Extend,
}

const TRANSFORM_WGSL: &str = "
struct Params {
    translate_x: f32,
    translate_y: f32,
    scale_x: f32,
    scale_y: f32,
    angle: f32,
    aspect: f32,
    zero: u32,
}

@group(0) @binding(0) var<uniform> params: Params;

@fragment
fn fs_main(in: Varyings) -> @location(0) vec4f {
    // Work in a y-up space centered on the image, with square units so rotation keeps its shape.
    var p = vec2f(in.uv.x - 0.5, 0.5 - in.uv.y) - vec2f(params.translate_x, params.translate_y);
    p.x *= params.aspect;
    let c = cos(params.angle);
    let s = sin(params.angle);
    p = vec2f(c * p.x + s * p.y, c * p.y - s * p.x);
    p.x /= params.aspect;
    p /= vec2f(params.scale_x, params.scale_y);
    let uv = vec2f(p.x + 0.5, 0.5 - p.y);
    if params.zero != 0u && (any(uv < vec2f(0.0)) || any(uv > vec2f(1.0))) {
        return vec4f(0.0);
    }
    return textureSampleLevel(input0, sampler0, uv, 0.0);
}
";

pub struct Transform(Pass);

impl Default for Transform {
    fn default() -> Self {
        Transform(Pass::new("texture/transform", TRANSFORM_WGSL, "fs_main"))
    }
}

impl TextureOp for Transform {
    type Params = TransformParams;
    const SPEC: OpSpec = OpSpec {
        name: "texture/transform",
        label: "Transform",
        doc: "Moves, scales and rotates its input. Scaling and rotation are around the center.",
        inputs: 1..=1,
        time_dependent: false,
    };

    fn cook(&mut self, cx: &mut TextureCx<'_>, p: &TransformParams) -> Result<Texture, CookError> {
        let nonzero = |v: f32| if v.abs() < 1e-6 { 1e-6f32.copysign(v) } else { v };
        let uniforms = Uniforms::default()
            .f32(p.translate_x)
            .f32(p.translate_y)
            .f32(nonzero(p.scale_x))
            .f32(nonzero(p.scale_y))
            .f32(p.rotate.to_radians())
            .f32(cx.width as f32 / cx.height as f32)
            .u32((p.extend == Extend::Zero).into());
        let address = match p.extend {
            Extend::Hold | Extend::Zero => wgpu::AddressMode::ClampToEdge,
            Extend::Repeat => wgpu::AddressMode::Repeat,
            Extend::Mirror => wgpu::AddressMode::MirrorRepeat,
        };
        let input = cx.inputs()[0].clone();
        let target = cx.target();
        self.0.run(cx, &[&input], &uniforms, address, &target)?;
        Ok(target)
    }
}

#[derive(Params)]
pub struct LevelParams {
    /// Multiplies the color.
    #[param(default = 1.0, min = 0.0, soft_max = 2.0)]
    pub brightness: f32,
    /// Scales the color away from mid gray.
    #[param(default = 1.0, min = 0.0, soft_max = 2.0)]
    pub contrast: f32,
    /// Above 1 brightens the mid tones, below 1 darkens them.
    #[param(default = 1.0, min = 0.01, soft_max = 4.0)]
    pub gamma: f32,
    /// Multiplies alpha.
    #[param(default = 1.0, min = 0.0, max = 1.0)]
    pub opacity: f32,
    /// Blends the color toward its inverse, before the other adjustments.
    #[param(min = 0.0, max = 1.0)]
    pub invert: f32,
}

const LEVEL_WGSL: &str = "
struct Params {
    brightness: f32,
    contrast: f32,
    gamma: f32,
    opacity: f32,
    invert: f32,
}

@group(0) @binding(0) var<uniform> params: Params;

@fragment
fn fs_main(in: Varyings) -> @location(0) vec4f {
    let color = textureSampleLevel(input0, sampler0, in.uv, 0.0);
    var rgb = mix(color.rgb, 1.0 - color.rgb, params.invert);
    rgb *= params.brightness;
    rgb = (rgb - 0.5) * params.contrast + 0.5;
    rgb = pow(max(rgb, vec3f(0.0)), vec3f(1.0 / params.gamma));
    return vec4f(rgb, color.a * params.opacity);
}
";

pub struct Level(Pass);

impl Default for Level {
    fn default() -> Self {
        Level(Pass::new("texture/level", LEVEL_WGSL, "fs_main"))
    }
}

impl TextureOp for Level {
    type Params = LevelParams;
    const SPEC: OpSpec = OpSpec {
        name: "texture/level",
        label: "Level",
        doc: "Adjusts brightness, contrast, gamma and opacity, and inverts.",
        inputs: 1..=1,
        time_dependent: false,
    };

    fn cook(&mut self, cx: &mut TextureCx<'_>, p: &LevelParams) -> Result<Texture, CookError> {
        let uniforms = Uniforms::default()
            .f32(p.brightness)
            .f32(p.contrast)
            .f32(p.gamma)
            .f32(p.opacity)
            .f32(p.invert);
        let input = cx.inputs()[0].clone();
        let target = cx.target();
        self.0
            .run(cx, &[&input], &uniforms, wgpu::AddressMode::ClampToEdge, &target)?;
        Ok(target)
    }
}

#[derive(Menu, Clone, Copy, Debug, PartialEq)]
pub enum Operation {
    /// Later inputs are laid over earlier ones by their alpha.
    Over,
    Add,
    Multiply,
    Screen,
    Difference,
    Maximum,
    Minimum,
}

#[derive(Params)]
pub struct CompositeParams {
    /// How each input is combined with the ones before it.
    pub operation: Operation,
}

const COMPOSITE_WGSL: &str = "
struct Params {
    operation: u32,
}

@group(0) @binding(0) var<uniform> params: Params;

fn combine(base: vec4f, layer: vec4f) -> vec4f {
    var rgb: vec3f;
    switch params.operation {
        case 0u: {
            let alpha = layer.a + base.a * (1.0 - layer.a);
            if alpha <= 0.0 {
                return vec4f(0.0);
            }
            return vec4f((layer.rgb * layer.a + base.rgb * base.a * (1.0 - layer.a)) / alpha, alpha);
        }
        case 1u: { rgb = base.rgb + layer.rgb; }
        case 2u: { rgb = base.rgb * layer.rgb; }
        case 3u: { rgb = 1.0 - (1.0 - base.rgb) * (1.0 - layer.rgb); }
        case 4u: { rgb = abs(base.rgb - layer.rgb); }
        case 5u: { rgb = max(base.rgb, layer.rgb); }
        default: { rgb = min(base.rgb, layer.rgb); }
    }
    return vec4f(rgb, base.a + layer.a - base.a * layer.a);
}

@fragment
fn fs_main(in: Varyings) -> @location(0) vec4f {
    var result = sample_input(0u, in.uv);
    for (var index = 1u; index < INPUTS; index++) {
        result = combine(result, sample_input(index, in.uv));
    }
    return result;
}
";

pub struct Composite(Pass);

impl Default for Composite {
    fn default() -> Self {
        Composite(Pass::new("texture/composite", COMPOSITE_WGSL, "fs_main"))
    }
}

impl TextureOp for Composite {
    type Params = CompositeParams;
    const SPEC: OpSpec = OpSpec {
        name: "texture/composite",
        label: "Composite",
        doc: "Combines its inputs in order, each with the result of the ones before it. Inputs are stretched to the \
              output size. Every operation but Over applies to color, and alpha is combined as with Over.",
        inputs: 2..=16,
        time_dependent: false,
    };

    fn cook(&mut self, cx: &mut TextureCx<'_>, p: &CompositeParams) -> Result<Texture, CookError> {
        let uniforms = Uniforms::default().u32(p.operation as u32);
        let inputs: Vec<Texture> = cx.inputs().iter().map(|input| (*input).clone()).collect();
        let inputs: Vec<&Texture> = inputs.iter().collect();
        let target = cx.target();
        self.0
            .run(cx, &inputs, &uniforms, wgpu::AddressMode::ClampToEdge, &target)?;
        Ok(target)
    }
}
