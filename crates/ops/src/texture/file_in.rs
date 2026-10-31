use std::{fs, io::Cursor, path::PathBuf, time::SystemTime};

use futhark_gpu::Fit;
use futhark_graph::{CookError, OpSpec, Params, Texture, TextureCx, TextureOp};
use zune_jpeg::{
    JpegDecoder,
    zune_core::{bytestream::ZCursor, colorspace::ColorSpace, options::DecoderOptions},
};

#[derive(Params)]
pub struct FileInParams {
    /// A PNG or JPEG image. Relative paths are relative to the project.
    pub path: String,
}

/// The last image loaded, kept until its path or modification time changes.
struct Loaded {
    path: PathBuf,
    modified: Option<SystemTime>,
    texture: Texture,
}

#[derive(Default)]
pub struct FileIn {
    loaded: Option<Loaded>,
}

impl TextureOp for FileIn {
    type Params = FileInParams;
    const SPEC: OpSpec = OpSpec {
        name: "texture/file_in",
        label: "File In",
        doc: "Loads a PNG or JPEG image. Its size and format (8-bit) are used unless width, height or format are \
              set, in which case it is stretched to fit. Pixel values are used as stored, without color conversion.",
        inputs: 0..=0,
        time_dependent: false,
    };

    fn cook(&mut self, cx: &mut TextureCx<'_>, p: &FileInParams) -> Result<Texture, CookError> {
        if p.path.is_empty() {
            return Err(CookError::new("no file: set the path parameter"));
        }
        let path = cx.path(&p.path);
        let fail = |message: String| CookError::new(format!("{}: {message}", path.display()));
        let modified = fs::metadata(&path).map_err(|e| fail(e.to_string()))?.modified().ok();
        let cached = self
            .loaded
            .as_ref()
            .filter(|loaded| loaded.path == path && loaded.modified == modified && modified.is_some());
        let image = match cached {
            Some(loaded) => loaded.texture.clone(),
            None => {
                let bytes = fs::read(&path).map_err(|e| fail(e.to_string()))?;
                let (width, height, pixels) = decode(&bytes).map_err(fail)?;
                let texture = upload(cx, width, height, &pixels).map_err(fail)?;
                self.loaded = Some(Loaded {
                    path: path.clone(),
                    modified,
                    texture: texture.clone(),
                });
                texture
            }
        };

        let pick = |value: i32, own: u32| if value == 0 { own } else { value as u32 };
        let width = pick(cx.common.width, image.width());
        let height = pick(cx.common.height, image.height());
        let format = cx.common.format.wgpu().unwrap_or(image.format());
        if (width, height, format) == (image.width(), image.height(), image.format()) {
            return Ok(image);
        }
        let target = cx.texture(width, height, format);
        cx.gpu
            .draw_scaled(cx.encoder, image.view(), target.view(), Fit::Stretch);
        Ok(target)
    }
}

fn upload(cx: &TextureCx<'_>, width: u32, height: u32, pixels: &[u8]) -> Result<Texture, String> {
    let max = cx.gpu.device.limits().max_texture_dimension_2d;
    if width > max || height > max {
        return Err(format!(
            "{width}×{height} is larger than this GPU's limit of {max}×{max}"
        ));
    }
    let size = wgpu::Extent3d {
        width,
        height,
        depth_or_array_layers: 1,
    };
    let texture = cx.gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("texture/file_in"),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    cx.gpu.queue.write_texture(
        texture.as_image_copy(),
        pixels,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(width * 4),
            rows_per_image: None,
        },
        size,
    );
    Ok(Texture::new(texture))
}

/// Decodes a PNG or JPEG into 8-bit RGBA.
fn decode(bytes: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        decode_png(bytes).map_err(|e| format!("bad PNG: {e}"))
    } else if bytes.starts_with(&[0xff, 0xd8]) {
        let options = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGBA);
        let mut decoder = JpegDecoder::new_with_options(ZCursor::new(bytes), options);
        let pixels = decoder.decode().map_err(|e| format!("bad JPEG: {e:?}"))?;
        let info = decoder.info().ok_or("bad JPEG: no header")?;
        Ok((info.width.into(), info.height.into(), pixels))
    } else {
        Err("not a PNG or JPEG image".into())
    }
}

fn decode_png(bytes: &[u8]) -> Result<(u32, u32, Vec<u8>), png::DecodingError> {
    let mut decoder = png::Decoder::new(Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info()?;
    let mut buffer = vec![0; reader.output_buffer_size().unwrap_or(0)];
    let info = reader.next_frame(&mut buffer)?;
    let data = &buffer[..info.buffer_size()];
    let pixels = match info.color_type {
        png::ColorType::Rgba => data.to_vec(),
        png::ColorType::Rgb => data.chunks(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect(),
        png::ColorType::GrayscaleAlpha => data.chunks(2).flat_map(|p| [p[0], p[0], p[0], p[1]]).collect(),
        png::ColorType::Grayscale | png::ColorType::Indexed => data.iter().flat_map(|&v| [v, v, v, 255]).collect(),
    };
    Ok((info.width, info.height, pixels))
}
