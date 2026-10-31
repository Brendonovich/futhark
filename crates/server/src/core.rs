//! The engine thread. It owns everything stateful (the open project and its engine) and runs the frame clock;
//! connection threads send it jobs through a [`Handle`]. The engine never crosses threads.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, mpsc},
    thread,
    time::{Duration, Instant},
};

use std::collections::HashMap;

use futhark_gpu::Gpu;
use futhark_graph::{Clock, Engine, Network, OpId, Output, Registry};
use futhark_protocol::{
    ChannelSamples, CreateParams, Edit, EditParams, Edited, Event, Hello, Method, OpenParams, Opened, PROTOCOL,
    ProjectInfo, RpcError, Sample, SampleParams, Snapshot, Status, previews, shared,
};

use crate::{
    convert,
    rings::Completions,
    streams::{Draw, Previewer, Viewer},
    to_value,
};

type Job = Box<dyn FnOnce(&mut Core) + Send>;

/// Sends jobs to the engine thread.
#[derive(Clone)]
pub struct Handle {
    jobs: mpsc::Sender<Job>,
}

impl Handle {
    /// Runs `job` on the engine thread and waits for its result. Fails once the server is shutting down.
    pub fn call<R: Send + 'static>(&self, job: impl FnOnce(&mut Core) -> R + Send + 'static) -> std::io::Result<R> {
        let stopped = || std::io::Error::other("futharkd is shutting down");
        let (reply, result) = mpsc::channel();
        let job: Job = Box::new(move |core| {
            let _ = reply.send(job(core));
        });
        self.jobs.send(job).map_err(|_| stopped())?;
        result.recv().map_err(|_| stopped())
    }

    /// Asks the engine thread to stop: it ends every stream, then drops the GPU once its completion thread has
    /// finished, and exits. Join the handle `spawn` returned to wait for it.
    pub fn stop(&self) {
        let _ = self.jobs.send(Box::new(|core: &mut Core| core.quit = true));
    }
}

pub fn spawn() -> (Handle, thread::JoinHandle<()>) {
    let (jobs, queue) = mpsc::channel::<Job>();
    let thread = thread::Builder::new()
        .name("engine".into())
        .spawn(move || {
            let gpu = match Gpu::headless() {
                Ok(gpu) => Some(Arc::new(gpu)),
                Err(error) => {
                    eprintln!("futharkd: no GPU, so Texture operators won't cook: {error}");
                    None
                }
            };
            let (completions, completion_thread) = gpu.clone().map(Completions::spawn).unzip();
            let mut core = Core {
                registry: Arc::new(futhark_ops::registry()),
                gpu,
                completions,
                open: None,
                subscribers: Vec::new(),
                viewers: HashMap::new(),
                previewers: HashMap::new(),
                next_stream: 1,
                quit: false,
            };
            while !core.quit {
                match queue.recv_timeout(core.until_next_frame()) {
                    Ok(job) => job(&mut core),
                    Err(mpsc::RecvTimeoutError::Timeout) => core.tick(),
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
            core.end_streams("futharkd is shutting down");
            drop(core);
            // The GPU goes when the completion thread lets go of it: wait, so nothing is inside the driver when the
            // process exits.
            if let Some(thread) = completion_thread {
                let _ = thread.join();
            }
        })
        .expect("spawning the engine thread");
    (Handle { jobs }, thread)
}

pub struct Core {
    registry: Arc<Registry>,
    gpu: Option<Arc<Gpu>>,
    completions: Option<Completions>,
    open: Option<OpenProject>,
    subscribers: Vec<mpsc::Sender<Event>>,
    viewers: HashMap<u64, Viewer>,
    previewers: HashMap<u64, Previewer>,
    next_stream: u64,
    quit: bool,
}

struct OpenProject {
    dir: PathBuf,
    name: String,
    rate: f64,
    engine: Engine,
    /// When frame 0 was, for the realtime clock.
    started: Instant,
}

impl OpenProject {
    fn info(&self) -> ProjectInfo {
        ProjectInfo {
            path: self.dir.to_string_lossy().into_owned(),
            name: self.name.clone(),
            rate: self.rate,
        }
    }

    /// The frame the realtime clock is on now.
    fn frame_now(&self) -> i64 {
        (self.started.elapsed().as_secs_f64() * self.rate).floor() as i64
    }
}

impl Core {
    fn until_next_frame(&self) -> Duration {
        match &self.open {
            Some(open) => {
                let next = (open.engine.clock().frame + 1) as f64 / open.rate;
                Duration::from_secs_f64((next - open.started.elapsed().as_secs_f64()).max(0.0))
            }
            None => Duration::from_secs(3600),
        }
    }

    /// Moves the clock to the current frame, skipping frames that were missed, and draws the streams that are due.
    fn tick(&mut self) {
        let Some(open) = &mut self.open else { return };
        let frame = open.frame_now();
        if frame != open.engine.clock().frame {
            open.engine.set_frame(frame);
        }
        let mut draw = Draw {
            engine: &mut open.engine,
            gpu: self.gpu.as_deref(),
            completions: self.completions.as_ref(),
            frame,
            rate: open.rate,
        };
        self.viewers.retain(|_, viewer| viewer.draw(&mut draw));
        self.previewers.retain(|_, previewer| previewer.draw(&mut draw));
    }

    fn shared_transport(&self) -> Option<&'static str> {
        (self.gpu.is_some() && futhark_surfaces::TRANSPORT != "unsupported").then_some(futhark_surfaces::TRANSPORT)
    }

    /// Starts a `shared_frames` viewer. Its events go to `events` until it ends.
    pub fn start_viewer(
        &mut self,
        params: shared::Params,
        events: mpsc::Sender<shared::Event>,
    ) -> Result<(u64, shared::Started), RpcError> {
        let transport = self.shared_transport().ok_or_else(|| {
            RpcError::new(
                RpcError::UNSUPPORTED,
                "shared frames need a GPU and shared surfaces, which this server lacks",
            )
        })?;
        let rate = self.open_project()?.rate;
        let id = self.next_stream;
        self.next_stream += 1;
        self.viewers.insert(id, Viewer::new(params, rate, events));
        self.tick();
        Ok((
            id,
            shared::Started {
                transport: transport.into(),
            },
        ))
    }

    pub fn viewer_control(&mut self, id: u64, control: shared::Control) {
        if let Some(viewer) = self.viewers.get_mut(&id) {
            viewer.control(control);
        }
    }

    pub fn stop_viewer(&mut self, id: u64) {
        self.viewers.remove(&id);
    }

    /// Starts a `previews` stream. Its events go to `events` until it ends.
    pub fn start_previews(
        &mut self,
        params: previews::Params,
        events: mpsc::Sender<previews::Event>,
    ) -> Result<(u64, shared::Started), RpcError> {
        self.open_project()?;
        let id = self.next_stream;
        self.next_stream += 1;
        self.previewers.insert(id, Previewer::new(params, events));
        self.tick();
        let transport = self.shared_transport().unwrap_or("unsupported");
        Ok((
            id,
            shared::Started {
                transport: transport.into(),
            },
        ))
    }

    pub fn previews_control(&mut self, id: u64, control: previews::Control) {
        if let Some(previewer) = self.previewers.get_mut(&id) {
            previewer.control(control);
        }
    }

    pub fn stop_previews(&mut self, id: u64) {
        self.previewers.remove(&id);
    }

    /// Ends every viewer and preview stream, when the project they show goes away.
    fn end_streams(&mut self, reason: &str) {
        for viewer in self.viewers.values() {
            viewer.end(reason);
        }
        for previewer in self.previewers.values() {
            previewer.end(reason);
        }
        self.viewers.clear();
        self.previewers.clear();
    }

    /// Registers a `watch` connection. Returns its events and the current status.
    pub fn subscribe(&mut self) -> (mpsc::Receiver<Event>, Status) {
        let (sender, events) = mpsc::channel();
        self.subscribers.push(sender);
        (events, self.status())
    }

    fn emit(&mut self, event: Event) {
        self.subscribers
            .retain(|subscriber| subscriber.send(event.clone()).is_ok());
    }

    fn status(&self) -> Status {
        Status {
            project: self.open.as_ref().map(OpenProject::info),
            frame: self.open.as_ref().map_or(0, |open| open.engine.clock().frame),
        }
    }

    fn open_project(&mut self) -> Result<&mut OpenProject, RpcError> {
        self.open
            .as_mut()
            .ok_or_else(|| RpcError::wrong_state("no project is open: open or create one first"))
    }

    pub fn handle(&mut self, method: Method) -> Result<serde_json::Value, RpcError> {
        self.tick();
        Ok(match method {
            Method::Hello => to_value(&Hello {
                protocol: PROTOCOL,
                version: env!("CARGO_PKG_VERSION").into(),
                capabilities: self
                    .shared_transport()
                    .map(|transport| vec![format!("shared-frames:{transport}"), format!("previews:{transport}")])
                    .unwrap_or_default(),
            }),
            Method::Status => to_value(&self.status()),
            Method::Create(params) => to_value(&self.create(params)?),
            Method::Open(params) => to_value(&self.open(params)?),
            Method::Save => {
                let open = self.open_project()?;
                futhark_project::write(&open.dir, &open.name, open.rate, open.engine.network())
                    .map_err(|e| RpcError::failed(e.to_string()))?;
                let project = open.info();
                self.emit(Event::ProjectSaved { project });
                serde_json::Value::Null
            }
            Method::Close => {
                self.end_streams("the project was closed");
                if self.open.take().is_some() {
                    self.emit(Event::ProjectClosed);
                }
                serde_json::Value::Null
            }
            Method::Types => to_value(&self.registry.iter().map(convert::op_type).collect::<Vec<_>>()),
            Method::Get => to_value(&self.snapshot()?),
            Method::Edit(params) => to_value(&self.edit(params)?),
            Method::Sample(params) => to_value(&self.sample(params)?),
            Method::Watch | Method::Shutdown | Method::SharedFrames(_) | Method::Previews(_) => {
                unreachable!("handled by the connection")
            }
        })
    }

    fn create(&mut self, params: CreateParams) -> Result<Opened, RpcError> {
        let dir = absolute(&params.path)?;
        if dir.join("project.kdl").exists() {
            return Err(RpcError::failed(format!(
                "{} already has a project; open it instead",
                dir.display()
            )));
        }
        let name = params
            .name
            .or_else(|| Some(dir.file_name()?.to_string_lossy().into_owned()))
            .unwrap_or_else(|| "untitled".into());
        futhark_project::create(&dir, &futhark_project::Project::new(name, self.registry.clone()))
            .map_err(|e| RpcError::failed(e.to_string()))?;
        self.open(OpenParams { path: params.path })
    }

    fn open(&mut self, params: OpenParams) -> Result<Opened, RpcError> {
        let dir = absolute(&params.path)?;
        let (project, diagnostics) =
            futhark_project::load(&dir, self.registry.clone()).map_err(|e| RpcError::failed(e.to_string()))?;
        let clock = Clock {
            rate: project.rate,
            frame: 0,
        };
        let mut engine = Engine::new(project.network, clock).with_project_dir(&dir);
        if let Some(gpu) = &self.gpu {
            engine = engine.with_gpu(gpu.clone());
        }
        self.end_streams("another project was opened");
        let open = OpenProject {
            dir,
            name: project.name,
            rate: project.rate,
            engine,
            started: Instant::now(),
        };
        let opened = Opened {
            project: open.info(),
            revision: open.engine.network().revision(),
            diagnostics: diagnostics.iter().map(ToString::to_string).collect(),
        };
        self.open = Some(open);
        self.emit(Event::ProjectOpened {
            project: opened.project.clone(),
        });
        Ok(opened)
    }

    fn snapshot(&mut self) -> Result<Snapshot, RpcError> {
        let open = self.open_project()?;
        let network = open.engine.network();
        let mut ops = Vec::new();
        let mut stack: Vec<OpId> = network.children(network.root()).map(|(_, id)| id).collect();
        stack.reverse();
        while let Some(id) = stack.pop() {
            ops.push(convert::op(network, id, open.engine.error(id)));
            let first_child = stack.len();
            stack.extend(network.children(id).map(|(_, child)| child));
            stack[first_child..].reverse();
        }
        Ok(Snapshot {
            revision: network.revision(),
            project: open.info(),
            ops,
        })
    }

    fn edit(&mut self, params: EditParams) -> Result<Edited, RpcError> {
        let open = self.open_project()?;
        let current = open.engine.network().revision();
        if let Some(expected) = params.revision
            && expected != current
        {
            return Err(RpcError::wrong_state(format!(
                "the network is at revision {current}, not {expected}: get it again and retry"
            )));
        }
        let mut network = open.engine.network().clone();
        for (index, edit) in params.edits.into_iter().enumerate() {
            apply(&mut network, edit).map_err(|message| RpcError::failed(format!("edits[{index}]: {message}")))?;
        }
        let revision = network.revision();
        *open.engine.network_mut() = network;
        if revision != current {
            self.emit(Event::NetworkChanged { revision });
        }
        Ok(Edited { revision })
    }

    fn sample(&mut self, params: SampleParams) -> Result<Sample, RpcError> {
        let open = self.open_project()?;
        let id = find(open.engine.network(), &params.path)?;
        let output = open
            .engine
            .pull(id)
            .map_err(|e| RpcError::failed(format!("{}: {e}", params.path)))?;
        let frame = open.engine.clock().frame;
        Ok(match output {
            Output::Channels(channels) => Sample::Channel {
                frame,
                rate: channels.rate,
                start: channels.start,
                channels: channels
                    .channels()
                    .iter()
                    .map(|c| ChannelSamples {
                        name: c.name.clone(),
                        samples: c.samples.clone(),
                    })
                    .collect(),
            },
            Output::Texture(texture) => Sample::Texture {
                frame,
                width: texture.width(),
                height: texture.height(),
                format: format!("{:?}", texture.format()),
            },
        })
    }
}

fn absolute(path: &str) -> Result<PathBuf, RpcError> {
    let path = Path::new(path);
    if !path.is_absolute() {
        return Err(RpcError::new(
            RpcError::INVALID_PARAMS,
            format!("path must be absolute, got {path:?}"),
        ));
    }
    Ok(path.into())
}

fn find(network: &Network, path: &str) -> Result<OpId, RpcError> {
    network
        .find(path)
        .ok_or_else(|| RpcError::failed(format!("no operator at {path}")))
}

fn apply(network: &mut Network, edit: Edit) -> Result<(), String> {
    let find = |network: &Network, path: &str| network.find(path).ok_or_else(|| format!("no operator at {path}"));
    match edit {
        Edit::Add { path, ty } => {
            let (parent, name) = path
                .rsplit_once('/')
                .ok_or_else(|| format!("{path:?} is not an absolute path"))?;
            let parent = find(network, if parent.is_empty() { "/" } else { parent })?;
            network.add(parent, name, &ty).map(drop).map_err(|e| e.to_string())
        }
        Edit::Remove { path } => {
            let id = find(network, &path)?;
            network.remove(id).map_err(|e| e.to_string())
        }
        Edit::Rename { path, name } => {
            let id = find(network, &path)?;
            network.rename(id, &name).map_err(|e| e.to_string())
        }
        Edit::Set { path, param, value } => {
            let id = find(network, &path)?;
            network
                .set_param(id, &param, convert::value_in(value))
                .map_err(|e| e.to_string())
        }
        Edit::Connect { from, to, input } => {
            let (from, to) = (find(network, &from)?, find(network, &to)?);
            network.connect(from, to, input).map_err(|e| e.to_string())
        }
        Edit::Disconnect { to, input } => {
            let to = find(network, &to)?;
            network.disconnect(to, input);
            Ok(())
        }
        Edit::Move { path, x, y } => {
            let id = find(network, &path)?;
            network.set_position(id, [x, y]);
            Ok(())
        }
    }
}
