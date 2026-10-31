//! Live previews inside the network editor's nodes. The editor works out which nodes show and how many pixels each
//! preview covers, and hands that to a [`PreviewSource`]; the source keeps the latest values or atlas rectangle for
//! each.
//!
//! [`StreamPreviews`] uses the server's `previews` stream: one ring of atlas surfaces for every visible Texture
//! node, plus each Channel node's latest values, on a connection of its own. The atlas goes into one
//! `NativeSurface`, and each node shows its rectangle of it.

use std::{
    collections::{HashMap, VecDeque},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use futhark_client::{Client, PreviewControl, PreviewFrame};
use futhark_protocol::{Family, previews};
use rune::NativeSurface;

use crate::session::Waker;

/// A node the editor shows, and the most device pixels its preview may take.
#[derive(Clone, Debug, PartialEq)]
pub struct PreviewRequest {
    pub path: String,
    pub family: Family,
    pub pixels: [u32; 2],
}

#[derive(Clone, Debug, PartialEq)]
pub enum Preview {
    /// The latest value of each channel.
    Channel(Vec<(String, f32)>),
    /// A Texture operator's picture: `[x, y, width, height]` in the atlas, in pixels.
    Texture([u32; 4]),
    /// Nothing to show yet.
    Pending,
    Error(String),
}

impl Preview {
    fn from_protocol(p: &previews::Preview) -> Preview {
        if let Some(error) = &p.error {
            return Preview::Error(error.clone());
        }
        if let Some(rect) = p.rect {
            return Preview::Texture(rect);
        }
        match &p.channels {
            Some(channels) => Preview::Channel(channels.clone()),
            None => Preview::Pending,
        }
    }
}

/// The surface holding every Texture preview, and its size in pixels.
#[derive(Clone)]
pub struct Atlas {
    pub surface: NativeSurface,
    pub size: [u32; 2],
}

pub trait PreviewSource {
    /// The nodes showing now, replacing the last list.
    fn set_visible(&mut self, requests: Vec<PreviewRequest>);
    fn get(&self, path: &str) -> Option<Preview>;
    fn atlas(&self) -> Option<Atlas>;
    /// Whether any preview changed since the last call. New atlas pixels alone don't count: the surface redraws
    /// itself.
    fn take_changed(&self) -> bool;
}

#[derive(Default)]
struct Shared {
    nodes: Vec<previews::Node>,
    previews: HashMap<String, Preview>,
    atlas_size: Option<[u32; 2]>,
    control: Option<PreviewControl>,
}

fn lock(shared: &Mutex<Shared>) -> std::sync::MutexGuard<'_, Shared> {
    shared.lock().unwrap_or_else(|e| e.into_inner())
}

/// Previews from the server's `previews` stream.
pub struct StreamPreviews {
    shared: Arc<Mutex<Shared>>,
    surface: NativeSurface,
    changed: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
}

impl StreamPreviews {
    pub fn start(socket: PathBuf, wake: Waker) -> StreamPreviews {
        let shared = Arc::new(Mutex::new(Shared::default()));
        let surface = NativeSurface::new();
        surface.set_waker(wake.clone());
        let changed = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let stream = Stream {
            socket,
            shared: shared.clone(),
            surface: surface.clone(),
            changed: changed.clone(),
            stop: stop.clone(),
            wake,
        };
        thread::Builder::new()
            .name("futhark-previews".into())
            .spawn(move || stream.run())
            .expect("spawn the previews thread");
        StreamPreviews {
            shared,
            surface,
            changed,
            stop,
        }
    }
}

impl Drop for StreamPreviews {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(control) = &lock(&self.shared).control {
            control.set_nodes(Vec::new());
        }
    }
}

impl PreviewSource for StreamPreviews {
    fn set_visible(&mut self, requests: Vec<PreviewRequest>) {
        let mut shared = lock(&self.shared);
        let nodes: Vec<previews::Node> = requests
            .iter()
            .map(|r| previews::Node {
                path: r.path.clone(),
                width: r.pixels[0],
                height: r.pixels[1],
            })
            .collect();
        shared.previews.retain(|path, _| nodes.iter().any(|n| n.path == *path));
        if let Some(control) = &shared.control {
            control.set_nodes(nodes.clone());
        }
        shared.nodes = nodes;
    }

    fn get(&self, path: &str) -> Option<Preview> {
        lock(&self.shared).previews.get(path).cloned()
    }

    fn atlas(&self) -> Option<Atlas> {
        lock(&self.shared).atlas_size.map(|size| Atlas {
            surface: self.surface.clone(),
            size,
        })
    }

    fn take_changed(&self) -> bool {
        self.changed.swap(false, Ordering::Relaxed)
    }
}

/// The thread behind [`StreamPreviews`]: keeps one stream open while there is anything to preview, reconnecting if
/// it ends.
struct Stream {
    socket: PathBuf,
    shared: Arc<Mutex<Shared>>,
    surface: NativeSurface,
    changed: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    wake: Waker,
}

impl Stream {
    fn run(self) {
        while !self.stop.load(Ordering::Relaxed) {
            let nodes = lock(&self.shared).nodes.clone();
            if nodes.is_empty() {
                thread::sleep(Duration::from_millis(100));
                continue;
            }
            let params = previews::Params {
                nodes: nodes.clone(),
                fps: None,
            };
            match Client::connect(&self.socket).and_then(|c| c.previews(params)) {
                Ok(stream) => self.follow(stream, nodes),
                Err(error) => {
                    self.fail(&error.to_string());
                    thread::sleep(Duration::from_secs(1));
                }
            }
        }
    }

    fn follow(&self, mut stream: futhark_client::Previews, sent: Vec<previews::Node>) {
        {
            let mut shared = lock(&self.shared);
            // The editor may have moved on while the stream started.
            if shared.nodes != sent {
                stream.control().set_nodes(shared.nodes.clone());
            }
            shared.control = Some(stream.control());
        }
        // The frame on screen and the one before, which the GPU may still be reading: dropping a frame hands its
        // atlas back to the server.
        let mut held: VecDeque<PreviewFrame> = VecDeque::with_capacity(3);
        while !self.stop.load(Ordering::Relaxed) {
            match stream.recv() {
                Ok(Some(frame)) => {
                    self.show(&frame);
                    held.push_back(frame);
                    if held.len() > 2 {
                        held.pop_front();
                    }
                }
                Ok(None) => break,
                Err(error) => {
                    self.fail(&error.to_string());
                    break;
                }
            }
        }
        lock(&self.shared).control = None;
        self.surface.clear();
        drop(held);
    }

    fn show(&self, frame: &PreviewFrame) {
        let mut atlas_size = None;
        if let Some(atlas) = &frame.atlas {
            match crate::frames::push(&self.surface, atlas, frame.frame) {
                Ok(()) => atlas_size = Some([atlas.width(), atlas.height()]),
                Err(error) => return self.fail(&error),
            }
        }
        let mut shared = lock(&self.shared);
        let mut changed = false;
        if atlas_size.is_some() && shared.atlas_size != atlas_size {
            shared.atlas_size = atlas_size;
            changed = true;
        }
        for p in &frame.previews {
            if !shared.nodes.iter().any(|n| n.path == p.path) {
                continue;
            }
            let preview = Preview::from_protocol(p);
            if shared.previews.get(&p.path) != Some(&preview) {
                shared.previews.insert(p.path.clone(), preview);
                changed = true;
            }
        }
        drop(shared);
        if changed {
            self.changed.store(true, Ordering::Relaxed);
            (self.wake)();
        }
    }

    /// Shows `message` on every node that has nothing better.
    fn fail(&self, message: &str) {
        let mut shared = lock(&self.shared);
        let paths: Vec<String> = shared.nodes.iter().map(|n| n.path.clone()).collect();
        for path in paths {
            let entry = shared.previews.entry(path).or_insert(Preview::Pending);
            if *entry == Preview::Pending {
                *entry = Preview::Error(message.to_string());
            }
        }
        drop(shared);
        self.changed.store(true, Ordering::Relaxed);
        (self.wake)();
    }
}

/// Fixed previews, for headless tests and scenes.
#[derive(Default)]
pub struct StaticPreviews {
    pub previews: HashMap<String, Preview>,
    pub atlas: Option<Atlas>,
}

impl PreviewSource for StaticPreviews {
    fn set_visible(&mut self, _: Vec<PreviewRequest>) {}

    fn get(&self, path: &str) -> Option<Preview> {
        self.previews.get(path).cloned()
    }

    fn atlas(&self) -> Option<Atlas> {
        self.atlas.clone()
    }

    fn take_changed(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn previews_read_the_stream() {
        let p = |rect, channels, error| previews::Preview {
            path: "/a".into(),
            rect,
            channels,
            error,
        };
        assert_eq!(
            Preview::from_protocol(&p(Some([0, 0, 4, 4]), None, None)),
            Preview::Texture([0, 0, 4, 4])
        );
        assert_eq!(
            Preview::from_protocol(&p(None, Some(vec![("x".into(), 1.0)]), None)),
            Preview::Channel(vec![("x".into(), 1.0)])
        );
        assert_eq!(
            Preview::from_protocol(&p(Some([0, 0, 4, 4]), None, Some("bad".into()))),
            Preview::Error("bad".into())
        );
        assert_eq!(Preview::from_protocol(&p(None, None, None)), Preview::Pending);
    }
}
