//! The viewer of the selected operator. A Texture operator streams through shared frames into a `NativeSurface`;
//! a Channel operator is sampled about 30 times a second. Each runs on a thread with a connection of its own.

use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use futhark_client::{Client, ClientError, FrameControl};
use futhark_protocol::{Family, Sample};
use rune::NativeSurface;

use crate::session::Waker;

pub enum Viewer {
    Texture(TextureViewer),
    Channel(ChannelViewer),
    /// Families without a viewer yet.
    None,
}

impl Viewer {
    pub fn start(socket: &Path, path: &str, family: Family, wake: Waker) -> Viewer {
        let socket = socket.to_path_buf();
        match family {
            Family::Texture => Viewer::Texture(TextureViewer::start(socket, path.into(), wake)),
            Family::Channel => Viewer::Channel(ChannelViewer::start(socket, path.into(), wake)),
            _ => Viewer::None,
        }
    }

    pub fn take_changed(&self) -> bool {
        match self {
            Viewer::Texture(v) => v.changed.swap(false, Ordering::Relaxed),
            Viewer::Channel(v) => v.changed.swap(false, Ordering::Relaxed),
            Viewer::None => false,
        }
    }
}

/// What the viewer thread last said, as the view shows it.
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

pub struct TextureViewer {
    pub surface: NativeSurface,
    status: Arc<Mutex<Option<String>>>,
    changed: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    control: Arc<Mutex<Option<FrameControl>>>,
}

/// The largest picture the viewer asks for, in pixels.
const MAX_PIXELS: u32 = 1024;

impl TextureViewer {
    fn start(socket: PathBuf, path: String, wake: Waker) -> TextureViewer {
        let surface = NativeSurface::new();
        let wake_ui = wake.clone();
        surface.set_waker(wake);
        let status = Arc::new(Mutex::new(Some("Waiting for frames…".to_string())));
        let changed = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let control = Arc::new(Mutex::new(None));
        let viewer = TextureViewer {
            surface: surface.clone(),
            status: status.clone(),
            changed: changed.clone(),
            stop: stop.clone(),
            control: control.clone(),
        };
        let say = move |message: Option<String>| {
            *lock(&status) = message;
            changed.store(true, Ordering::Relaxed);
            wake_ui();
        };
        thread::Builder::new()
            .name("futhark-texture-viewer".into())
            .spawn(move || stream(socket, path, surface, stop, control, say))
            .expect("spawn the texture viewer thread");
        viewer
    }

    /// Why there's no picture, if there isn't one.
    pub fn status(&self) -> Option<String> {
        lock(&self.status).clone()
    }
}

impl Drop for TextureViewer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // The thread may be waiting on a still picture; closing the stream wakes it.
        if let Some(control) = lock(&self.control).as_ref() {
            control.close();
        }
    }
}

#[cfg(target_os = "macos")]
fn stream(
    socket: PathBuf,
    path: String,
    surface: NativeSurface,
    stop: Arc<AtomicBool>,
    control: Arc<Mutex<Option<FrameControl>>>,
    say: impl Fn(Option<String>),
) {
    use futhark_client::Update;
    use futhark_protocol::{RpcError, shared::Params};

    let params = Params {
        path,
        max_width: MAX_PIXELS,
        max_height: MAX_PIXELS,
        fps: None,
        slots: None,
    };
    let mut frames = match Client::connect(&socket).and_then(|c| c.shared_frames(params)) {
        Ok(frames) => frames,
        Err(ClientError::Rpc(error))
            if error.code == RpcError::UNSUPPORTED || error.code == RpcError::METHOD_NOT_FOUND =>
        {
            return say(Some(format!("This server can't show textures yet: {}", error.message)));
        }
        Err(error) => return say(Some(error.to_string())),
    };
    *lock(&control) = Some(frames.control());
    if stop.load(Ordering::Relaxed) {
        return;
    }
    // The frame on screen and the one before it, which the GPU may still be drawing from. Dropping a frame hands its
    // surface back to the server, so neither may go before a newer one replaces it.
    let mut held = VecDeque::with_capacity(3);
    let mut shown_any = false;
    while !stop.load(Ordering::Relaxed) {
        match frames.recv() {
            Ok(Some(Update::Frame(frame))) => match crate::frames::push(&surface, &frame.surface, frame.frame) {
                Ok(()) => {
                    held.push_back(frame);
                    if held.len() > 2 {
                        held.pop_front();
                    }
                    if !shown_any {
                        shown_any = true;
                        say(None);
                    }
                }
                Err(error) => say(Some(error)),
            },
            Ok(Some(Update::Error(message))) => {
                shown_any = false;
                say(Some(message));
            }
            Ok(None) => return say(Some("The server ended this viewer.".into())),
            Err(error) => return say(Some(error.to_string())),
        }
    }
    surface.clear();
    drop(held);
}

#[cfg(not(target_os = "macos"))]
fn stream(
    _: PathBuf,
    _: String,
    _: NativeSurface,
    _: Arc<AtomicBool>,
    _: Arc<Mutex<Option<FrameControl>>>,
    say: impl Fn(Option<String>),
) {
    let _ = MAX_PIXELS;
    say(Some(
        "Texture viewers need shared GPU surfaces, which are macOS-only so far.".into(),
    ));
}

/// How many samples of history a channel graph keeps.
pub const HISTORY: usize = 180;
const SAMPLE_EVERY: Duration = Duration::from_millis(33);

/// A Channel operator's recent values.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ChannelView {
    /// Each channel's values to graph, oldest first: its history over time when each sample carries one value, or
    /// the samples themselves when it carries many.
    pub channels: Vec<(String, Vec<f32>)>,
    pub error: Option<String>,
}

pub struct ChannelViewer {
    view: Arc<Mutex<ChannelView>>,
    changed: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
}

impl ChannelViewer {
    fn start(socket: PathBuf, path: String, wake: Waker) -> ChannelViewer {
        let view = Arc::new(Mutex::new(ChannelView::default()));
        let changed = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let (v, c, s) = (view.clone(), changed.clone(), stop.clone());
        thread::Builder::new()
            .name("futhark-channel-viewer".into())
            .spawn(move || {
                let mut history = History::default();
                let mut client = None;
                while !s.load(Ordering::Relaxed) {
                    if client.is_none() {
                        client = Client::connect(&socket).ok();
                    }
                    if let Some(cl) = client.as_mut() {
                        let next = match cl.sample(&path) {
                            Ok(sample) => history.add(&sample),
                            Err(ClientError::Rpc(error)) => ChannelView {
                                error: Some(error.message),
                                ..history.view()
                            },
                            Err(error) => {
                                client = None;
                                ChannelView {
                                    error: Some(error.to_string()),
                                    ..history.view()
                                }
                            }
                        };
                        let mut shown = lock(&v);
                        if *shown != next {
                            *shown = next;
                            drop(shown);
                            c.store(true, Ordering::Relaxed);
                            wake();
                        }
                    }
                    thread::sleep(SAMPLE_EVERY);
                }
            })
            .expect("spawn the channel viewer thread");
        ChannelViewer { view, changed, stop }
    }

    pub fn view(&self) -> ChannelView {
        lock(&self.view).clone()
    }
}

impl Drop for ChannelViewer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Collects samples into graphs.
#[derive(Default)]
pub struct History {
    channels: Vec<(String, VecDeque<f32>)>,
    last_frame: Option<i64>,
}

impl History {
    pub fn add(&mut self, sample: &Sample) -> ChannelView {
        let (frame, channels) = match sample {
            Sample::Channel { frame, channels, .. } => (frame, channels),
            Sample::Texture { .. } => {
                return ChannelView {
                    error: Some("This operator outputs a texture, not channels".into()),
                    ..ChannelView::default()
                };
            }
        };
        if channels.iter().any(|c| c.samples.len() > 1) {
            self.channels.clear();
            self.last_frame = None;
            return ChannelView {
                channels: channels.iter().map(|c| (c.name.clone(), c.samples.clone())).collect(),
                error: None,
            };
        }
        let names_match = self.channels.len() == channels.len()
            && self.channels.iter().zip(channels).all(|((name, _), c)| *name == c.name);
        if !names_match {
            self.channels = channels.iter().map(|c| (c.name.clone(), VecDeque::new())).collect();
        }
        // The same engine frame sampled twice adds nothing.
        if self.last_frame != Some(*frame) {
            self.last_frame = Some(*frame);
            for ((_, values), c) in self.channels.iter_mut().zip(channels) {
                if let Some(&v) = c.samples.last() {
                    if values.len() == HISTORY {
                        values.pop_front();
                    }
                    values.push_back(v);
                }
            }
        }
        self.view()
    }

    fn view(&self) -> ChannelView {
        ChannelView {
            channels: self
                .channels
                .iter()
                .map(|(name, values)| (name.clone(), values.iter().copied().collect()))
                .collect(),
            error: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futhark_protocol::ChannelSamples;

    fn sample(frame: i64, values: &[(&str, &[f32])]) -> Sample {
        Sample::Channel {
            frame,
            rate: 60.0,
            start: frame,
            channels: values
                .iter()
                .map(|(name, samples)| ChannelSamples {
                    name: (*name).into(),
                    samples: samples.to_vec(),
                })
                .collect(),
        }
    }

    #[test]
    fn single_samples_build_a_history() {
        let mut h = History::default();
        h.add(&sample(1, &[("a", &[0.1])]));
        h.add(&sample(1, &[("a", &[0.1])]));
        let view = h.add(&sample(2, &[("a", &[0.2])]));
        assert_eq!(view.channels, [("a".to_string(), vec![0.1, 0.2])]);
        for f in 3..400 {
            h.add(&sample(f, &[("a", &[f as f32])]));
        }
        assert_eq!(h.view().channels[0].1.len(), HISTORY);
        // New channels start over.
        let view = h.add(&sample(500, &[("b", &[1.0])]));
        assert_eq!(view.channels, [("b".to_string(), vec![1.0])]);
    }

    #[test]
    fn many_samples_show_as_they_are() {
        let mut h = History::default();
        let view = h.add(&sample(1, &[("a", &[0.0, 1.0, 2.0])]));
        assert_eq!(view.channels, [("a".to_string(), vec![0.0, 1.0, 2.0])]);
    }
}
