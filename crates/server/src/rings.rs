//! Rings of shared surfaces, and the GPU completion thread that announces finished frames.

use std::{
    sync::{Arc, mpsc},
    thread,
};

use futhark_gpu::Gpu;
use futhark_surfaces::{Exporter, Surface, Token};

/// One ring: surfaces the client fetched once, drawn into in turn.
pub struct Ring {
    pub generation: u64,
    pub width: u32,
    pub height: u32,
    surfaces: Vec<Surface>,
    views: Vec<wgpu::TextureView>,
    held: Vec<bool>,
    token: Token,
}

/// What a client needs to fetch a new ring.
pub struct Offer {
    pub generation: u64,
    pub width: u32,
    pub height: u32,
    pub slots: u32,
    pub service: String,
    pub token: String,
}

impl Ring {
    fn new(gpu: &Gpu, generation: u64, width: u32, height: u32, slots: usize) -> Result<(Ring, Offer), String> {
        let exporter = Exporter::shared().map_err(|e| e.to_string())?;
        let mut surfaces = Vec::with_capacity(slots);
        let mut views = Vec::with_capacity(slots);
        for _ in 0..slots {
            let surface = Surface::bgra(width, height).map_err(|e| e.to_string())?;
            let texture = surface
                .texture(&gpu.device, wgpu::TextureUsages::RENDER_ATTACHMENT)
                .map_err(|e| e.to_string())?;
            views.push(texture.create_view(&Default::default()));
            surfaces.push(surface);
        }
        let token = exporter.offer(surfaces.clone());
        let offer = Offer {
            generation,
            width,
            height,
            slots: slots as u32,
            service: exporter.service_name().into(),
            token: token.to_string(),
        };
        Ok((
            Ring {
                generation,
                width,
                height,
                surfaces,
                views,
                held: vec![false; slots],
                token,
            },
            offer,
        ))
    }

    /// A slot the client isn't holding, marked as held.
    pub fn take_slot(&mut self) -> Option<(u32, &wgpu::TextureView, Surface)> {
        let slot = self.held.iter().position(|held| !held)?;
        self.held[slot] = true;
        Some((slot as u32, &self.views[slot], self.surfaces[slot].clone()))
    }

    fn busy(&self) -> bool {
        self.held.iter().any(|held| *held)
    }
}

impl Drop for Ring {
    fn drop(&mut self) {
        if let Ok(exporter) = Exporter::shared() {
            exporter.withdraw(&self.token);
        }
    }
}

/// The current ring of a stream, and the previous one until the client releases all of its slots.
#[derive(Default)]
pub struct Rings {
    current: Option<Ring>,
    retired: Option<Ring>,
    generation: u64,
}

pub enum Ensure {
    /// The current ring fits.
    Ready,
    /// A new ring was made; tell the client.
    New(Offer),
    /// The size changed but the client still holds slots of an older ring. Try again later.
    Wait,
}

impl Rings {
    /// Makes sure the current ring is `width`×`height`.
    pub fn ensure(&mut self, gpu: &Gpu, width: u32, height: u32, slots: usize) -> Result<Ensure, String> {
        if self
            .current
            .as_ref()
            .is_some_and(|ring| (ring.width, ring.height) == (width, height))
        {
            return Ok(Ensure::Ready);
        }
        if self.retired.as_ref().is_some_and(Ring::busy) {
            return Ok(Ensure::Wait);
        }
        self.generation += 1;
        let (ring, offer) = Ring::new(gpu, self.generation, width, height, slots)?;
        self.retired = self.current.replace(ring);
        Ok(Ensure::New(offer))
    }

    pub fn current(&mut self) -> Option<&mut Ring> {
        self.current.as_mut()
    }

    pub fn release(&mut self, generation: u64, slot: u32) {
        for ring in [&mut self.current, &mut self.retired].into_iter().flatten() {
            if ring.generation == generation
                && let Some(held) = ring.held.get_mut(slot as usize)
            {
                *held = false;
            }
        }
        if self.retired.as_ref().is_some_and(|ring| !ring.busy()) {
            self.retired = None;
        }
    }
}

/// Waits for submitted GPU work so that `on_submitted_work_done` callbacks run as soon as frames are finished,
/// without making the engine thread wait.
#[derive(Clone)]
pub struct Completions(mpsc::Sender<()>);

impl Completions {
    pub fn spawn(gpu: Arc<Gpu>) -> Self {
        let (sender, submitted) = mpsc::channel::<()>();
        thread::Builder::new()
            .name("gpu completions".into())
            .spawn(move || {
                while submitted.recv().is_ok() {
                    while submitted.try_recv().is_ok() {}
                    let _ = gpu.device.poll(wgpu::PollType::wait_indefinitely());
                }
            })
            .expect("spawning the GPU completion thread");
        Completions(sender)
    }

    /// Runs `done` once everything submitted so far has finished on the GPU.
    pub fn after_submitted(&self, gpu: &Gpu, done: impl FnOnce() + Send + 'static) {
        gpu.queue.on_submitted_work_done(done);
        let _ = self.0.send(());
    }
}

/// The largest size with `source`'s shape that fits in `limit`, never larger than `source`.
pub fn fitted((width, height): (u32, u32), (max_width, max_height): (u32, u32)) -> (u32, u32) {
    let scale = (max_width as f64 / width as f64)
        .min(max_height as f64 / height as f64)
        .min(1.0);
    let size = |v: u32| ((v as f64 * scale).round() as u32).max(1);
    (size(width), size(height))
}

#[cfg(test)]
mod tests {
    use super::fitted;

    #[test]
    fn fits_keeping_the_shape() {
        assert_eq!(fitted((1920, 1080), (640, 640)), (640, 360));
        assert_eq!(fitted((1080, 1920), (640, 640)), (360, 640));
        assert_eq!(fitted((64, 64), (640, 480)), (64, 64));
        assert_eq!(fitted((4000, 1), (100, 100)), (100, 1));
    }
}
