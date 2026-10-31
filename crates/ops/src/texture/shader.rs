use futhark_graph::{CookError, OpSpec, Params, Texture, TextureCx, TextureOp};

use super::pass::{Pass, Uniforms};

/// The shader a new Shader operator starts with.
pub const DEFAULT_SHADER: &str = "@fragment
fn main(in: Varyings) -> @location(0) vec4f {
    let color = 0.5 + 0.5 * cos(futhark.time + in.uv.xyx * 4.0 + vec3f(0.0, 2.0, 4.0));
    if INPUTS > 0u {
        return sample_input(0u, in.uv) * vec4f(color, 1.0);
    }
    return vec4f(color, 1.0);
}
";

/// Declared ahead of the operator's code, after the inputs.
const UNIFORMS_WGSL: &str = "struct Futhark {
    resolution: vec2f,
    time: f32,
    frame: i32,
}

@group(0) @binding(0) var<uniform> futhark: Futhark;

";

#[derive(Params)]
pub struct ShaderParams {
    /// WGSL source with a fragment entry point named `main`. See the operator's docs for what it can use.
    #[param(default = String::from(DEFAULT_SHADER))]
    pub code: String,
}

#[derive(Default)]
pub struct Shader {
    pass: Option<Pass>,
}

impl TextureOp for Shader {
    type Params = ShaderParams;
    const SPEC: OpSpec = OpSpec {
        name: "texture/shader",
        label: "Shader",
        doc: "Runs a WGSL fragment shader over every pixel. The code defines `@fragment fn main(in: Varyings) -> \
              @location(0) vec4f`, and can use:\n\
              - `in.uv`: texture coordinates, from (0, 0) at the top left to (1, 1) at the bottom right, and \
              `in.position`: the pixel position (pixel centers are at .5);\n\
              - `futhark.resolution` (vec2f, the output size in pixels), `futhark.time` (f32, seconds) and \
              `futhark.frame` (i32), a uniform at @group(0) @binding(0);\n\
              - `input0` to `input7`: the connected inputs as `texture_2d<f32>` at @group(1) @binding(2 * i), each \
              with `sampler0` to `sampler7` at @group(1) @binding(2 * i + 1) (linear, or nearest for 32-bit float \
              inputs, which can't be filtered);\n\
              - `INPUTS`: how many inputs are connected, and `sample_input(index, uv)`, which samples an input by \
              index and returns transparent black past the last one.\n\
              Errors are reported with line numbers in the code.",
        inputs: 0..=8,
        time_dependent: true,
    };

    fn cook(&mut self, cx: &mut TextureCx<'_>, p: &ShaderParams) -> Result<Texture, CookError> {
        let source = format!("{UNIFORMS_WGSL}{}", p.code);
        if self.pass.as_ref().is_none_or(|pass| pass.source() != source) {
            self.pass = Some(Pass::new("texture/shader", source, "main"));
        }
        let pass = self.pass.as_mut().expect("made above");
        let uniforms = Uniforms::default()
            .f32(cx.width as f32)
            .f32(cx.height as f32)
            .f32(cx.time.seconds())
            .i32(cx.time.frame as i32);
        let inputs: Vec<Texture> = cx.inputs().iter().map(|input| (*input).clone()).collect();
        let inputs: Vec<&Texture> = inputs.iter().collect();
        let target = cx.target();
        pass.run(cx, &inputs, &uniforms, wgpu::AddressMode::ClampToEdge, &target)
            .map_err(|error| shift_lines(error, UNIFORMS_WGSL.lines().count()))?;
        Ok(target)
    }
}

/// Moves `line N:` prefixes up by `by` lines, so they count from the start of the operator's code.
fn shift_lines(error: CookError, by: usize) -> CookError {
    let lines: Vec<String> = error
        .0
        .lines()
        .map(|line| {
            let shifted = line.strip_prefix("line ").and_then(|rest| {
                let (number, rest) = rest.split_once(':')?;
                let number = number.parse::<usize>().ok()?.checked_sub(by)?;
                Some(format!("line {number}:{rest}"))
            });
            shifted.unwrap_or_else(|| line.to_string())
        })
        .collect();
    CookError::new(lines.join("\n"))
}
