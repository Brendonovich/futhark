use std::{collections::HashMap, sync::Arc};

/// A 2D texture and a view of all of it. Cheap to clone: clones share the texture, and two `Texture`s are equal when
/// they are the same texture.
///
/// Hold on to the `Texture` itself rather than the wgpu handles inside it: a [`TexturePool`] reuses a texture as soon
/// as no `Texture` refers to it.
#[derive(Clone, Debug)]
pub struct Texture(Arc<Inner>);

#[derive(Debug)]
struct Inner {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
}

impl Texture {
    pub fn new(texture: wgpu::Texture) -> Self {
        let view = texture.create_view(&Default::default());
        Texture(Arc::new(Inner { texture, view }))
    }

    pub fn texture(&self) -> &wgpu::Texture {
        &self.0.texture
    }

    pub fn view(&self) -> &wgpu::TextureView {
        &self.0.view
    }

    pub fn width(&self) -> u32 {
        self.0.texture.width()
    }

    pub fn height(&self) -> u32 {
        self.0.texture.height()
    }

    pub fn format(&self) -> wgpu::TextureFormat {
        self.0.texture.format()
    }
}

impl PartialEq for Texture {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for Texture {}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TextureKey {
    pub width: u32,
    pub height: u32,
    pub format: wgpu::TextureFormat,
    pub usage: wgpu::TextureUsages,
}

/// Reuses textures by size, format and usage. A texture is free again once every [`Texture`] handed out for it has
/// been dropped.
#[derive(Default)]
pub struct TexturePool {
    textures: HashMap<TextureKey, Vec<Entry>>,
    generation: u64,
}

struct Entry {
    texture: Texture,
    /// The generation it was last handed out in.
    used: u64,
}

impl Entry {
    fn free(&self) -> bool {
        Arc::strong_count(&self.texture.0) == 1
    }
}

/// Free textures are kept for this many generations after they were last used.
const KEEP_GENERATIONS: u64 = 2;

impl TexturePool {
    /// What pooled textures can be used for: drawn into, sampled, and copied in either direction.
    pub const USAGE: wgpu::TextureUsages = wgpu::TextureUsages::RENDER_ATTACHMENT
        .union(wgpu::TextureUsages::TEXTURE_BINDING)
        .union(wgpu::TextureUsages::COPY_SRC)
        .union(wgpu::TextureUsages::COPY_DST);

    /// A free texture matching `key`, or a new one. Its contents are undefined.
    pub fn acquire(&mut self, device: &wgpu::Device, key: TextureKey) -> Texture {
        let entries = self.textures.entry(key).or_default();
        if let Some(entry) = entries.iter_mut().find(|entry| entry.free()) {
            entry.used = self.generation;
            return entry.texture.clone();
        }
        let texture = Texture::new(device.create_texture(&wgpu::TextureDescriptor {
            label: Some("pooled"),
            size: wgpu::Extent3d {
                width: key.width,
                height: key.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: key.format,
            usage: key.usage,
            view_formats: &[],
        }));
        entries.push(Entry {
            texture: texture.clone(),
            used: self.generation,
        });
        texture
    }

    /// Starts a new generation, dropping free textures that haven't been used for a while. Call it once a frame.
    pub fn trim(&mut self) {
        self.generation += 1;
        let generation = self.generation;
        for entries in self.textures.values_mut() {
            entries.retain(|entry| !entry.free() || generation - entry.used <= KEEP_GENERATIONS);
        }
        self.textures.retain(|_, entries| !entries.is_empty());
    }

    /// How many textures the pool holds, in use or free.
    pub fn len(&self) -> usize {
        self.textures.values().map(Vec::len).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
