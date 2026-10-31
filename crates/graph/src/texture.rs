use std::path::{Path, PathBuf};

use futhark_gpu::{Gpu, Texture, TextureKey, TexturePool};

use crate::{CookError, Menu, OpSpec, Params, Time};

/// Width and height used by generators (Texture operators without inputs) when theirs are 0.
pub const DEFAULT_SIZE: u32 = 256;

/// The pixel format of a Texture operator.
#[derive(Menu, Clone, Copy, Debug, PartialEq, Eq)]
pub enum PixelFormat {
    /// The first input's format, or 8-bit for generators.
    #[menu(label = "Use Input")]
    UseInput,
    #[menu(label = "8-bit")]
    Rgba8,
    #[menu(label = "16-bit float")]
    Rgba16f,
    #[menu(label = "32-bit float")]
    Rgba32f,
}

impl PixelFormat {
    /// The wgpu format, or `None` for `UseInput`.
    pub fn wgpu(self) -> Option<wgpu::TextureFormat> {
        match self {
            PixelFormat::UseInput => None,
            PixelFormat::Rgba8 => Some(wgpu::TextureFormat::Rgba8Unorm),
            PixelFormat::Rgba16f => Some(wgpu::TextureFormat::Rgba16Float),
            PixelFormat::Rgba32f => Some(wgpu::TextureFormat::Rgba32Float),
        }
    }
}

/// Parameters every Texture operator has, before its own. `Registry::add_texture` adds them.
#[derive(Params, Clone, Debug, PartialEq)]
pub struct TextureParams {
    /// Output width in pixels. 0 uses the first input's width, or 256 without inputs.
    #[param(min = 0, max = 16384, soft_max = 4096)]
    pub width: i32,
    /// Output height in pixels. 0 uses the first input's height, or 256 without inputs.
    #[param(min = 0, max = 16384, soft_max = 4096)]
    pub height: i32,
    /// Output pixel format.
    pub format: PixelFormat,
}

pub struct TextureCx<'a> {
    pub time: Time,
    pub gpu: &'a Gpu,
    /// Records this cook's GPU work. The engine submits it before `Engine::pull` returns, after the work of the
    /// operator's inputs.
    pub encoder: &'a mut wgpu::CommandEncoder,
    /// The output width and height, resolved from the common parameters and the first input.
    pub width: u32,
    pub height: u32,
    /// The output format, resolved from the common parameters and the first input.
    pub format: wgpu::TextureFormat,
    /// The common parameters as set, before resolving.
    pub common: TextureParams,
    pub(crate) inputs: Vec<&'a Texture>,
    pub(crate) pool: &'a mut TexturePool,
    pub(crate) project_dir: Option<&'a Path>,
}

impl TextureCx<'_> {
    /// The connected inputs, in order.
    pub fn inputs(&self) -> &[&Texture] {
        &self.inputs
    }

    /// A texture at the output size and format, to draw the result into. Its contents are undefined.
    pub fn target(&mut self) -> Texture {
        self.texture(self.width, self.height, self.format)
    }

    /// A scratch or output texture from the engine's pool. Its contents are undefined.
    pub fn texture(&mut self, width: u32, height: u32, format: wgpu::TextureFormat) -> Texture {
        self.pool.acquire(
            &self.gpu.device,
            TextureKey {
                width,
                height,
                format,
                usage: TexturePool::USAGE,
            },
        )
    }

    /// Resolves a path parameter: relative paths are relative to the project directory, when the engine has one.
    pub fn path(&self, path: &str) -> PathBuf {
        match self.project_dir {
            Some(dir) => dir.join(path),
            None => PathBuf::from(path),
        }
    }
}

/// An operator of the Texture family. Its output is one 2D texture, normally of the size and format in its
/// [`TextureCx`].
pub trait TextureOp: Default + 'static {
    type Params: Params;
    const SPEC: OpSpec;

    fn cook(&mut self, cx: &mut TextureCx<'_>, params: &Self::Params) -> Result<Texture, CookError>;
}

/// Resolves the output size and format from the common parameters and the first input.
pub(crate) fn resolve(common: &TextureParams, first: Option<&Texture>) -> (u32, u32, wgpu::TextureFormat) {
    let pick = |value: i32, inherited: Option<u32>| match value {
        0 => inherited.unwrap_or(DEFAULT_SIZE),
        value => value as u32,
    };
    let width = pick(common.width, first.map(Texture::width));
    let height = pick(common.height, first.map(Texture::height));
    let format = common
        .format
        .wgpu()
        .or(first.map(Texture::format))
        .unwrap_or(wgpu::TextureFormat::Rgba8Unorm);
    (width, height, format)
}
