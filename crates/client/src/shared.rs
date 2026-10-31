//! The client side of shared frames: fetching each ring's surfaces and releasing slots.

use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    sync::{Arc, Mutex},
    time::Duration,
};

use futhark_protocol::{
    previews::{Control as PreviewsControl, Event as PreviewsEvent, Node},
    shared::{Control, Event},
};
use futhark_surfaces::Surface;

use crate::{ClientError, Result};

/// How long fetching a ring's surfaces may take.
const FETCH_TIMEOUT: Duration = Duration::from_secs(2);

/// A viewer's stream of frames. Call [`recv`](Self::recv) in a loop; adjust it from elsewhere with a
/// [`FrameControl`].
pub struct SharedFrames {
    reader: BufReader<UnixStream>,
    control: FrameControl,
    rings: HashMap<u64, Vec<Surface>>,
    transport: String,
}

pub enum Update {
    Frame(SharedFrame),
    /// The operator has no picture right now.
    Error(String),
}

/// A finished picture in a shared surface. The server won't draw into it until this is dropped.
pub struct SharedFrame {
    pub surface: Surface,
    pub generation: u64,
    pub slot: u32,
    /// The engine frame it shows.
    pub frame: i64,
    _release: Release,
}

struct Release {
    control: FrameControl,
    generation: u64,
    slot: u32,
}

impl Drop for Release {
    fn drop(&mut self) {
        self.control.send(&Control::Release {
            generation: self.generation,
            slot: self.slot,
        });
    }
}

/// Changes a viewer's rate or size. Cheap to clone.
#[derive(Clone)]
pub struct FrameControl(Arc<Mutex<UnixStream>>);

impl FrameControl {
    pub fn set_fps(&self, fps: u32) {
        self.send(&Control::Fps { fps });
    }

    pub fn set_size(&self, max_width: u32, max_height: u32) {
        self.send(&Control::Size { max_width, max_height });
    }

    /// Best effort: a closed connection shows up as the end of the stream instead.
    fn send(&self, control: &Control) {
        let mut line = serde_json::to_string(control).expect("controls serialize");
        line.push('\n');
        let _ = self
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .write_all(line.as_bytes());
    }
}

impl SharedFrames {
    pub(crate) fn new(reader: BufReader<UnixStream>, writer: UnixStream, transport: String) -> Self {
        SharedFrames {
            reader,
            control: FrameControl(Arc::new(Mutex::new(writer))),
            rings: HashMap::new(),
            transport,
        }
    }

    /// How surfaces travel, such as `iosurface-mach`.
    pub fn transport(&self) -> &str {
        &self.transport
    }

    pub fn control(&self) -> FrameControl {
        self.control.clone()
    }

    /// Waits for the next frame. `None` when the stream ends.
    pub fn recv(&mut self) -> Result<Option<Update>> {
        loop {
            let mut line = String::new();
            if self.reader.read_line(&mut line)? == 0 {
                return Ok(None);
            }
            let event: Event =
                serde_json::from_str(&line).map_err(|e| ClientError::Protocol(format!("{e}: {}", line.trim_end())))?;
            match event {
                Event::Ring {
                    generation,
                    service,
                    token,
                    ..
                } => {
                    let surfaces = futhark_surfaces::fetch(&service, &token, FETCH_TIMEOUT)
                        .map_err(|e| ClientError::Protocol(format!("fetching shared surfaces: {e}")))?;
                    // Frames of older rings hold their own surfaces, so old rings can go.
                    self.rings.retain(|&g, _| g > generation);
                    self.rings.insert(generation, surfaces);
                }
                Event::Frame {
                    generation,
                    slot,
                    frame,
                } => {
                    let release = Release {
                        control: self.control.clone(),
                        generation,
                        slot,
                    };
                    let Some(surface) = self.rings.get(&generation).and_then(|ring| ring.get(slot as usize)) else {
                        // A frame from a ring this client never fetched: hand it straight back.
                        continue;
                    };
                    return Ok(Some(Update::Frame(SharedFrame {
                        surface: surface.clone(),
                        generation,
                        slot,
                        frame,
                        _release: release,
                    })));
                }
                Event::Error { message } => return Ok(Some(Update::Error(message))),
                Event::End { .. } => return Ok(None),
            }
        }
    }
}

/// Live previews of many operators. Call [`recv`](Self::recv) in a loop; change which operators are previewed from
/// elsewhere with a [`PreviewControl`].
pub struct Previews {
    reader: BufReader<UnixStream>,
    control: PreviewControl,
    rings: HashMap<u64, Vec<Surface>>,
    transport: String,
}

/// One frame of previews. Texture previews are rectangles of `atlas`; it stays the client's until this is dropped.
pub struct PreviewFrame {
    pub atlas: Option<Surface>,
    pub frame: i64,
    pub previews: Vec<futhark_protocol::previews::Preview>,
    _release: Option<PreviewRelease>,
}

struct PreviewRelease {
    control: PreviewControl,
    generation: u64,
    slot: u32,
}

impl Drop for PreviewRelease {
    fn drop(&mut self) {
        self.control.send(&PreviewsControl::Release {
            generation: self.generation,
            slot: self.slot,
        });
    }
}

/// Changes what a preview stream shows. Cheap to clone.
#[derive(Clone)]
pub struct PreviewControl(Arc<Mutex<UnixStream>>);

impl PreviewControl {
    /// Replaces the operators being previewed.
    pub fn set_nodes(&self, nodes: Vec<Node>) {
        self.send(&PreviewsControl::Nodes { nodes });
    }

    pub fn set_fps(&self, fps: u32) {
        self.send(&PreviewsControl::Fps { fps });
    }

    fn send(&self, control: &PreviewsControl) {
        let mut line = serde_json::to_string(control).expect("controls serialize");
        line.push('\n');
        let _ = self
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .write_all(line.as_bytes());
    }
}

impl Previews {
    pub(crate) fn new(reader: BufReader<UnixStream>, writer: UnixStream, transport: String) -> Self {
        Previews {
            reader,
            control: PreviewControl(Arc::new(Mutex::new(writer))),
            rings: HashMap::new(),
            transport,
        }
    }

    /// How surfaces travel, or `unsupported`, in which case Texture previews come with an error instead.
    pub fn transport(&self) -> &str {
        &self.transport
    }

    pub fn control(&self) -> PreviewControl {
        self.control.clone()
    }

    /// Waits for the next frame. `None` when the stream ends.
    pub fn recv(&mut self) -> Result<Option<PreviewFrame>> {
        loop {
            let mut line = String::new();
            if self.reader.read_line(&mut line)? == 0 {
                return Ok(None);
            }
            let event: PreviewsEvent =
                serde_json::from_str(&line).map_err(|e| ClientError::Protocol(format!("{e}: {}", line.trim_end())))?;
            match event {
                PreviewsEvent::Ring {
                    generation,
                    service,
                    token,
                    ..
                } => {
                    let surfaces = futhark_surfaces::fetch(&service, &token, FETCH_TIMEOUT)
                        .map_err(|e| ClientError::Protocol(format!("fetching shared surfaces: {e}")))?;
                    self.rings.retain(|&g, _| g > generation);
                    self.rings.insert(generation, surfaces);
                }
                PreviewsEvent::Frame {
                    generation,
                    slot,
                    frame,
                    previews,
                } => {
                    let (atlas, release) = match slot {
                        Some(slot) => {
                            let release = PreviewRelease {
                                control: self.control.clone(),
                                generation,
                                slot,
                            };
                            match self.rings.get(&generation).and_then(|ring| ring.get(slot as usize)) {
                                Some(surface) => (Some(surface.clone()), Some(release)),
                                None => continue,
                            }
                        }
                        None => (None, None),
                    };
                    return Ok(Some(PreviewFrame {
                        atlas,
                        frame,
                        previews,
                        _release: release,
                    }));
                }
                PreviewsEvent::End { .. } => return Ok(None),
            }
        }
    }
}
