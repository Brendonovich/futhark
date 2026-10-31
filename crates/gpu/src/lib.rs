//! The engine's GPU context. The engine creates the device whether it runs headless or windowed, and the editor's UI
//! (Rune) adopts it, so textures are shown without leaving the GPU.

mod draw;
mod texture;

use std::sync::Mutex;

pub use draw::{FULLSCREEN_WGSL, Fit};
pub use texture::{Texture, TextureKey, TexturePool};

/// Features the engine uses when the adapter has them.
const OPTIONAL_FEATURES: wgpu::Features = wgpu::Features::TIMESTAMP_QUERY;

/// Vulkan loaders crash when two threads create instances or devices at once (seen with parallel tests on lavapipe),
/// so contexts are created one at a time.
static INIT: Mutex<()> = Mutex::new(());

#[derive(Debug, thiserror::Error)]
pub enum GpuError {
    #[error("no GPU adapter: {0}")]
    Adapter(#[from] wgpu::RequestAdapterError),
    #[error("no GPU device: {0}")]
    Device(#[from] wgpu::RequestDeviceError),
    #[error("GPU readback failed: {0}")]
    Readback(String),
}

pub struct Gpu {
    pub instance: wgpu::Instance,
    pub adapter: wgpu::Adapter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    drawer: Mutex<draw::Drawer>,
}

impl Gpu {
    /// A context with no window. `WGPU_BACKEND`, `WGPU_POWER_PREF` and the other wgpu environment variables override
    /// the choice of adapter; by default it is the high-performance one.
    pub fn headless() -> Result<Gpu, GpuError> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let _init = INIT.lock().unwrap_or_else(|e| e.into_inner());
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::from_env().unwrap_or(wgpu::PowerPreference::HighPerformance),
            ..Default::default()
        }))?;
        let limits = adapter.limits();
        let (device, queue) = pollster::block_on(
            adapter.request_device(&wgpu::DeviceDescriptor {
                label: Some("futhark"),
                required_features: adapter.features() & OPTIONAL_FEATURES,
                required_limits: wgpu::Limits::default()
                    .using_resolution(limits.clone())
                    .using_alignment(limits),
                experimental_features: Default::default(),
                memory_hints: wgpu::MemoryHints::Performance,
                trace: Default::default(),
            }),
        )?;
        Ok(Gpu {
            instance,
            adapter,
            device,
            queue,
            drawer: Default::default(),
        })
    }

    /// Starts catching the device's validation, out-of-memory and internal errors on this thread, until
    /// [`ErrorScope::finish`]. Without a scope, wgpu panics on errors.
    pub fn error_scope(&self) -> ErrorScope {
        ErrorScope([
            self.device.push_error_scope(wgpu::ErrorFilter::Validation),
            self.device.push_error_scope(wgpu::ErrorFilter::OutOfMemory),
            self.device.push_error_scope(wgpu::ErrorFilter::Internal),
        ])
    }

    /// Copies a 2D `Rgba8Unorm` texture to the CPU, waiting for the GPU. Rows are tightly packed.
    pub fn read_rgba8(&self, texture: &wgpu::Texture) -> Result<Vec<u8>, GpuError> {
        assert_eq!(
            texture.format(),
            wgpu::TextureFormat::Rgba8Unorm,
            "read_rgba8 needs an Rgba8Unorm texture"
        );
        self.read_texture(texture)
    }

    /// Copies a 2D texture of an uncompressed color format to the CPU, waiting for the GPU. Rows are tightly packed.
    pub fn read_texture(&self, texture: &wgpu::Texture) -> Result<Vec<u8>, GpuError> {
        let texel = texture
            .format()
            .block_copy_size(None)
            .expect("read_texture needs an uncompressed color texture");
        let (width, height) = (texture.width(), texture.height());
        let row = width * texel;
        let padded_row = row.next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: u64::from(padded_row * height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self.device.create_command_encoder(&Default::default());
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_row),
                    rows_per_image: Some(height),
                },
            },
            texture.size(),
        );
        self.queue.submit([encoder.finish()]);

        let (sender, receiver) = std::sync::mpsc::channel();
        buffer.slice(..).map_async(wgpu::MapMode::Read, move |result| {
            let _ = sender.send(result);
        });
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|e| GpuError::Readback(e.to_string()))?;
        receiver
            .recv()
            .map_err(|e| GpuError::Readback(e.to_string()))?
            .map_err(|e| GpuError::Readback(e.to_string()))?;

        let mapped = buffer
            .slice(..)
            .get_mapped_range()
            .map_err(|e| GpuError::Readback(e.to_string()))?;
        let mut pixels = Vec::with_capacity((row * height) as usize);
        for line in mapped.chunks(padded_row as usize) {
            pixels.extend_from_slice(&line[..row as usize]);
        }
        Ok(pixels)
    }
}

/// Device errors caught since [`Gpu::error_scope`].
#[must_use = "an error scope must be finished to find out whether anything failed"]
pub struct ErrorScope([wgpu::ErrorScopeGuard; 3]);

impl ErrorScope {
    /// Stops catching errors and returns the first one caught, if any.
    pub fn finish(self) -> Result<(), String> {
        let mut first = None;
        // Scopes are popped innermost first.
        for guard in self.0.into_iter().rev() {
            if let Some(error) = pollster::block_on(guard.pop()) {
                first.get_or_insert(error.to_string());
            }
        }
        first.map_or(Ok(()), Err)
    }
}
