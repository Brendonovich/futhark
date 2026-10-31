//! Viewers (`shared_frames`) and previews (`previews`): streams that draw operators into shared surfaces on the
//! engine thread's frame tick.

use std::sync::mpsc;

use futhark_gpu::{Fit, Gpu, Texture};
use futhark_graph::{Engine, Output};
use futhark_protocol::{previews, shared};

use crate::rings::{Completions, Ensure, Rings, fitted};

/// The atlas is this wide; its height grows with the previews in it.
const ATLAS_WIDTH: u32 = 2048;
const ATLAS_MAX_HEIGHT: u32 = 4096;
/// A preview is at most this big on either side.
const PREVIEW_MAX: u32 = 512;
const PREVIEW_SLOTS: usize = 3;

/// What the engine thread gives a stream to draw with.
pub struct Draw<'a> {
    pub engine: &'a mut Engine,
    pub gpu: Option<&'a Gpu>,
    pub completions: Option<&'a Completions>,
    pub frame: i64,
    pub rate: f64,
}

/// Frames between draws for `fps` at the project's `rate`.
fn interval(fps: u32, rate: f64) -> i64 {
    ((rate / f64::from(fps.max(1))).round() as i64).max(1)
}

pub struct Viewer {
    path: String,
    max: (u32, u32),
    fps: u32,
    slots: usize,
    rings: Rings,
    events: mpsc::Sender<shared::Event>,
    /// The texture last drawn, and the ring it went to. The same texture again needs no new frame.
    last: Option<(Texture, u64)>,
    last_error: Option<String>,
    next_frame: i64,
}

impl Viewer {
    pub fn new(params: shared::Params, rate: f64, events: mpsc::Sender<shared::Event>) -> Self {
        Viewer {
            path: params.path,
            max: (params.max_width.max(1), params.max_height.max(1)),
            fps: params.fps.unwrap_or(rate.round() as u32),
            slots: params
                .slots
                .unwrap_or(shared::DEFAULT_SLOTS)
                .clamp(2, shared::MAX_SLOTS) as usize,
            rings: Rings::default(),
            events,
            last: None,
            last_error: None,
            next_frame: i64::MIN,
        }
    }

    pub fn control(&mut self, control: shared::Control) {
        match control {
            shared::Control::Release { generation, slot } => self.rings.release(generation, slot),
            shared::Control::Fps { fps } => self.fps = fps.max(1),
            shared::Control::Size { max_width, max_height } => {
                self.max = (max_width.max(1), max_height.max(1));
                self.last = None;
            }
        }
    }

    pub fn end(&self, reason: &str) {
        let _ = self.events.send(shared::Event::End { reason: reason.into() });
    }

    fn error(&mut self, message: String) {
        if self.last_error.as_ref() != Some(&message) {
            let _ = self.events.send(shared::Event::Error {
                message: message.clone(),
            });
            self.last_error = Some(message);
            self.last = None;
        }
    }

    /// Draws the operator's texture into the next free slot, if it is due and changed. Returns false once the
    /// client has gone.
    pub fn draw(&mut self, draw: &mut Draw<'_>) -> bool {
        if draw.frame < self.next_frame {
            return true;
        }
        self.next_frame = draw.frame + interval(self.fps, draw.rate);
        let (Some(gpu), Some(completions)) = (draw.gpu, draw.completions) else {
            self.error("this server has no GPU".into());
            return true;
        };
        let texture = match texture(draw.engine, &self.path) {
            Ok(texture) => texture,
            Err(message) => {
                self.error(message);
                return true;
            }
        };
        let size = fitted((texture.width(), texture.height()), self.max);
        let offer = match self.rings.ensure(gpu, size.0, size.1, self.slots) {
            Ok(Ensure::Ready) => None,
            Ok(Ensure::New(offer)) => Some(offer),
            Ok(Ensure::Wait) => return true,
            Err(message) => {
                self.error(format!("shared surfaces: {message}"));
                return true;
            }
        };
        if let Some(offer) = offer {
            let ring = shared::Event::Ring {
                generation: offer.generation,
                width: offer.width,
                height: offer.height,
                slots: offer.slots,
                service: offer.service,
                token: offer.token,
            };
            if self.events.send(ring).is_err() {
                return false;
            }
        }
        let ring = self.rings.current().expect("ensured above");
        let generation = ring.generation;
        if self
            .last
            .as_ref()
            .is_some_and(|(last, g)| *last == texture && *g == generation)
        {
            return true;
        }
        // Every slot held: the client is behind, so this frame is skipped.
        let Some((slot, view, _surface)) = ring.take_slot() else {
            return true;
        };

        let scope = gpu.error_scope();
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        gpu.draw_scaled(&mut encoder, texture.view(), view, Fit::Contain);
        gpu.queue.submit([encoder.finish()]);
        if let Err(message) = scope.finish() {
            self.rings.release(generation, slot);
            self.error(format!("drawing the viewer: {message}"));
            return true;
        }
        self.last = Some((texture, generation));
        self.last_error = None;
        let (events, frame) = (self.events.clone(), draw.frame);
        completions.after_submitted(gpu, move || {
            let _ = events.send(shared::Event::Frame {
                generation,
                slot,
                frame,
            });
        });
        true
    }
}

pub struct Previewer {
    nodes: Vec<previews::Node>,
    cells: Cells,
    fps: u32,
    rings: Rings,
    events: mpsc::Sender<previews::Event>,
    /// What the last frame showed, to skip frames where nothing changed.
    last: Option<(Vec<Option<Texture>>, Vec<previews::Preview>)>,
    next_frame: i64,
}

enum Picture {
    Texture(Texture),
    Channels(Vec<(String, f32)>),
    Error(String),
}

impl Previewer {
    pub fn new(params: previews::Params, events: mpsc::Sender<previews::Event>) -> Self {
        Previewer {
            nodes: params.nodes,
            cells: Cells::default(),
            fps: params.fps.unwrap_or(previews::DEFAULT_FPS),
            rings: Rings::default(),
            events,
            last: None,
            next_frame: i64::MIN,
        }
    }

    pub fn control(&mut self, control: previews::Control) {
        match control {
            previews::Control::Nodes { nodes } => {
                self.nodes = nodes;
                self.next_frame = i64::MIN;
            }
            previews::Control::Release { generation, slot } => self.rings.release(generation, slot),
            previews::Control::Fps { fps } => self.fps = fps.max(1),
        }
    }

    pub fn end(&self, reason: &str) {
        let _ = self.events.send(previews::Event::End { reason: reason.into() });
    }

    /// Pulls every previewed operator and sends a frame if anything changed. Returns false once the client has
    /// gone.
    pub fn draw(&mut self, draw: &mut Draw<'_>) -> bool {
        if draw.frame < self.next_frame {
            return true;
        }
        self.next_frame = draw.frame + interval(self.fps, draw.rate);

        let pictures: Vec<Picture> = self.nodes.iter().map(|node| picture(draw.engine, &node.path)).collect();
        let boxes: Vec<Option<Placing<'_>>> = self
            .nodes
            .iter()
            .zip(&pictures)
            .map(|(node, picture)| match picture {
                Picture::Texture(texture) => {
                    let limit = (node.width.clamp(1, PREVIEW_MAX), node.height.clamp(1, PREVIEW_MAX));
                    Some((
                        node.path.as_str(),
                        limit,
                        fitted((texture.width(), texture.height()), limit),
                    ))
                }
                _ => None,
            })
            .collect();
        let (rects, atlas_height) = self.cells.place(&boxes);

        let mut previews = Vec::with_capacity(self.nodes.len());
        let mut textures = Vec::with_capacity(self.nodes.len());
        for ((node, picture), rect) in self.nodes.iter().zip(pictures).zip(&rects) {
            let mut preview = previews::Preview {
                path: node.path.clone(),
                rect: None,
                channels: None,
                error: None,
            };
            let mut texture = None;
            match picture {
                Picture::Texture(_) if draw.gpu.is_none() => preview.error = Some("this server has no GPU".into()),
                Picture::Texture(_) if futhark_surfaces::TRANSPORT == "unsupported" => {
                    preview.error = Some("texture previews need shared surfaces, which this system lacks".into());
                }
                Picture::Texture(t) => match rect {
                    Some(rect) => {
                        preview.rect = Some(*rect);
                        texture = Some(t);
                    }
                    None => preview.error = Some("too many previews to fit".into()),
                },
                Picture::Channels(values) => preview.channels = Some(values),
                Picture::Error(message) => preview.error = Some(message),
            }
            previews.push(preview);
            textures.push(texture);
        }

        if self
            .last
            .as_ref()
            .is_some_and(|(t, p)| *t == textures && *p == previews)
        {
            return true;
        }

        let drawn: Vec<(&Texture, [u32; 4])> = textures
            .iter()
            .zip(&previews)
            .filter_map(|(texture, preview)| Some((texture.as_ref()?, preview.rect?)))
            .collect();
        if drawn.is_empty() {
            self.last = Some((textures.clone(), previews.clone()));
            let frame = previews::Event::Frame {
                generation: 0,
                slot: None,
                frame: draw.frame,
                previews,
            };
            return self.events.send(frame).is_ok();
        }
        let (gpu, completions) = (
            draw.gpu.expect("checked above"),
            draw.completions.expect("there is a GPU"),
        );

        // The atlas only grows, rounded up, so panning doesn't make a new ring every frame.
        let height = self
            .rings
            .current()
            .map_or(0, |ring| ring.height)
            .max(atlas_height.next_multiple_of(256));
        let offer = match self
            .rings
            .ensure(gpu, ATLAS_WIDTH, height.min(ATLAS_MAX_HEIGHT), PREVIEW_SLOTS)
        {
            Ok(Ensure::Ready) => None,
            Ok(Ensure::New(offer)) => Some(offer),
            Ok(Ensure::Wait) => return true,
            Err(message) => {
                eprintln!("futharkd: previews: {message}");
                return true;
            }
        };
        if let Some(offer) = offer {
            let ring = previews::Event::Ring {
                generation: offer.generation,
                width: offer.width,
                height: offer.height,
                slots: offer.slots,
                service: offer.service,
                token: offer.token,
            };
            if self.events.send(ring).is_err() {
                return false;
            }
        }
        let ring = self.rings.current().expect("ensured above");
        let generation = ring.generation;
        let Some((slot, view, _surface)) = ring.take_slot() else {
            return true;
        };

        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        for (index, (texture, rect)) in drawn.iter().enumerate() {
            gpu.draw_scaled_into(&mut encoder, texture.view(), view, *rect, Fit::Contain, index == 0);
        }
        gpu.queue.submit([encoder.finish()]);
        self.last = Some((textures, previews.clone()));
        let (events, frame) = (self.events.clone(), draw.frame);
        completions.after_submitted(gpu, move || {
            let _ = events.send(previews::Event::Frame {
                generation,
                slot: Some(slot),
                frame,
                previews,
            });
        });
        true
    }
}

fn texture(engine: &mut Engine, path: &str) -> Result<Texture, String> {
    match picture(engine, path) {
        Picture::Texture(texture) => Ok(texture),
        Picture::Channels(_) => Err(format!("{path} is not a Texture operator")),
        Picture::Error(message) => Err(message),
    }
}

fn picture(engine: &mut Engine, path: &str) -> Picture {
    let Some(id) = engine.network().find(path) else {
        return Picture::Error(format!("no operator at {path}"));
    };
    match engine.pull(id) {
        Ok(Output::Texture(texture)) => Picture::Texture(texture),
        Ok(Output::Channels(channels)) => Picture::Channels(
            channels
                .channels()
                .iter()
                .map(|c| (c.name.clone(), c.samples.last().copied().unwrap_or(0.0)))
                .collect(),
        ),
        Err(error) => Picture::Error(error.to_string()),
    }
}

/// Where each previewed operator's picture goes in the atlas: a grid of equal cells, one per operator, that an
/// operator keeps for as long as it stays previewed at the same size. Panning adds and drops operators at the edges
/// of the view, and with fixed cells that never moves anyone else's picture, so a client that briefly shows one
/// frame's atlas with another frame's rectangles still crops every steady operator correctly.
/// An operator to place: its path, the box it asked for, and its picture's size inside that box.
type Placing<'a> = (&'a str, (u32, u32), (u32, u32));

#[derive(Default)]
struct Cells {
    /// The size of every cell: the largest box asked for.
    size: (u32, u32),
    assigned: std::collections::HashMap<String, usize>,
}

impl Cells {
    /// Places each `(path, box, picture size)`, keeping earlier placements. Returns each one's rectangle (`None` for
    /// no picture, or when the atlas is full) and the height of the cells in use.
    fn place(&mut self, nodes: &[Option<Placing<'_>>]) -> (Vec<Option<[u32; 4]>>, u32) {
        let size = nodes
            .iter()
            .flatten()
            .fold((1, 1), |(w, h), (_, b, _)| (w.max(b.0), h.max(b.1)));
        if size != self.size {
            // Every picture changes size anyway, so lay them out afresh.
            self.size = size;
            self.assigned.clear();
        }
        self.assigned
            .retain(|path, _| nodes.iter().flatten().any(|(p, ..)| p == path));
        let columns = (ATLAS_WIDTH / size.0).max(1) as usize;
        let capacity = columns * (ATLAS_MAX_HEIGHT / size.1).max(1) as usize;
        let rects = nodes
            .iter()
            .map(|node| {
                let (path, _, picture) = (*node)?;
                let index = match self.assigned.get(path) {
                    Some(index) => *index,
                    None => {
                        let index = (0..capacity).find(|i| !self.assigned.values().any(|taken| taken == i))?;
                        self.assigned.insert(path.to_string(), index);
                        index
                    }
                };
                let (column, row) = ((index % columns) as u32, (index / columns) as u32);
                Some([column * size.0, row * size.1, picture.0, picture.1])
            })
            .collect();
        let rows = self.assigned.values().map(|i| i / columns + 1).max().unwrap_or(0) as u32;
        (rects, rows * size.1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operators_keep_their_cells() {
        let node = |path, picture| Some((path, (256, 144), picture));
        let mut cells = Cells::default();
        let (rects, height) = cells.place(&[node("/a", (256, 144)), None, node("/b", (144, 144))]);
        assert_eq!(rects, [Some([0, 0, 256, 144]), None, Some([256, 0, 144, 144])]);
        assert_eq!(height, 144);

        // /a scrolls out of view and /c in: /b stays where it was, and /c takes the free cell.
        let (rects, _) = cells.place(&[node("/b", (144, 144)), node("/c", (256, 144))]);
        assert_eq!(rects, [Some([256, 0, 144, 144]), Some([0, 0, 256, 144])]);

        // A new preview size lays everything out again.
        let (rects, _) = cells.place(&[
            Some(("/b", (512, 288), (288, 288))),
            Some(("/c", (512, 288), (512, 288))),
        ]);
        assert_eq!(rects, [Some([0, 0, 288, 288]), Some([512, 0, 512, 288])]);

        // A full atlas leaves the rest out.
        let paths: Vec<String> = (0..40).map(|i| format!("/n{i}")).collect();
        let many: Vec<_> = paths
            .iter()
            .map(|p| Some((p.as_str(), (512, 512), (512, 512))))
            .collect();
        let (rects, height) = Cells::default().place(&many);
        assert_eq!(rects.iter().filter(|r| r.is_some()).count(), 32);
        assert_eq!(height, ATLAS_MAX_HEIGHT);
    }
}
