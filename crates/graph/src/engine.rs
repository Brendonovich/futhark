use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use futhark_gpu::{Gpu, TexturePool};

use crate::{
    CookError, Network, OpId, Output, Time,
    registry::{CookCx, DynOp},
};

/// Time-dependent operators that haven't been pulled for a while catch up on at most this much time.
const MAX_SLICE_SECONDS: f64 = 0.2;

/// The engine's frame clock.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Clock {
    /// Frames per second.
    pub rate: f64,
    pub frame: i64,
}

impl Default for Clock {
    fn default() -> Self {
        Clock { rate: 60.0, frame: 0 }
    }
}

/// Runtime state of one operator.
struct State {
    op: Box<dyn DynOp>,
    output: Option<Output>,
    error: Option<CookError>,
    /// Engine revision when this operator last cooked or failed.
    cooked: u64,
    /// Engine revision when its output or error last changed. Downstream operators cook when this passes theirs.
    changed: u64,
    /// The node's network revision at the last cook.
    cooked_edit: u64,
    last_frame: Option<i64>,
    /// The epoch in which this operator was last brought up to date.
    verified: u64,
    cooking: bool,
    cooks: u64,
}

impl State {
    fn result(&self) -> Result<u64, CookError> {
        match &self.error {
            Some(error) => Err(error.clone()),
            None => Ok(self.changed),
        }
    }
}

/// The engine's GPU, its texture pool, and work recorded during a pull that hasn't been submitted yet.
pub(crate) struct GpuState {
    pub gpu: Arc<Gpu>,
    pub pool: TexturePool,
    pub pending: Vec<wgpu::CommandBuffer>,
}

/// Cooks a network. Cooking is pull-based: nothing cooks until something asks for its output with `pull`, and then
/// only operators with a reason to cook do (an edit, an input that changed, or a new frame for time-dependent ones).
pub struct Engine {
    network: Network,
    clock: Clock,
    states: HashMap<OpId, State>,
    revision: u64,
    /// Bumped on every frame and network change. Within one epoch each operator is checked at most once.
    epoch: u64,
    seen_network: u64,
    gpu: Option<GpuState>,
    project_dir: Option<PathBuf>,
}

impl Engine {
    /// An engine without a GPU, which cooks every family but Texture.
    pub fn new(network: Network, clock: Clock) -> Self {
        Engine {
            network,
            clock,
            states: HashMap::new(),
            revision: 0,
            epoch: 1,
            seen_network: 0,
            gpu: None,
            project_dir: None,
        }
    }

    /// Cooks Texture operators on `gpu`. All of their GPU work runs inside wgpu error scopes on the thread that
    /// pulls, so errors become cook errors rather than panics.
    pub fn with_gpu(mut self, gpu: Arc<Gpu>) -> Self {
        self.gpu = Some(GpuState {
            gpu,
            pool: TexturePool::default(),
            pending: Vec::new(),
        });
        self
    }

    /// Resolves relative file paths in parameters (such as File In's) against `dir`.
    pub fn with_project_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.project_dir = Some(dir.into());
        self
    }

    pub fn gpu(&self) -> Option<&Arc<Gpu>> {
        self.gpu.as_ref().map(|state| &state.gpu)
    }

    pub fn project_dir(&self) -> Option<&Path> {
        self.project_dir.as_deref()
    }

    pub fn network(&self) -> &Network {
        &self.network
    }

    pub fn network_mut(&mut self) -> &mut Network {
        &mut self.network
    }

    pub fn clock(&self) -> Clock {
        self.clock
    }

    /// Moves to the next frame.
    pub fn advance(&mut self) {
        self.set_frame(self.clock.frame + 1);
    }

    pub fn set_frame(&mut self, frame: i64) {
        self.clock.frame = frame;
        self.epoch += 1;
        if let Some(gpu) = &mut self.gpu {
            gpu.pool.trim();
        }
    }

    /// Brings an operator up to date and returns its output. GPU work for a texture is submitted before this returns.
    pub fn pull(&mut self, id: OpId) -> Result<Output, CookError> {
        if self.network.revision != self.seen_network {
            self.seen_network = self.network.revision;
            self.epoch += 1;
            let network = &self.network;
            self.states.retain(|id, _| network.get(*id).is_some());
        }
        let result = self.ensure(id);
        if let Some(gpu) = &mut self.gpu
            && !gpu.pending.is_empty()
        {
            gpu.gpu.queue.submit(gpu.pending.drain(..));
        }
        result?;
        Ok(self.states[&id]
            .output
            .clone()
            .expect("an operator without an error has an output"))
    }

    /// The last output, without cooking.
    pub fn output(&self, id: OpId) -> Option<&Output> {
        self.states.get(&id)?.output.as_ref()
    }

    /// The last cook error, without cooking.
    pub fn error(&self, id: OpId) -> Option<&CookError> {
        self.states.get(&id)?.error.as_ref()
    }

    /// How many times an operator has cooked.
    pub fn cooks(&self, id: OpId) -> u64 {
        self.states.get(&id).map_or(0, |state| state.cooks)
    }

    fn ensure(&mut self, id: OpId) -> Result<u64, CookError> {
        let registry = self.network.registry().clone();
        let Some(node) = self.network.get(id) else {
            return Err(CookError::new("the operator was removed"));
        };
        let op_type = registry.get(node.ty()).expect("nodes have registered types");
        let Some(create) = op_type.create else {
            return Err(CookError::new(format!("{} has no output", self.network.path(id))));
        };
        let inputs = node.inputs().to_vec();
        let values = node.params().to_vec();
        let edit = node.revision;

        let state = self.states.entry(id).or_insert_with(|| State {
            op: create(),
            output: None,
            error: None,
            cooked: 0,
            changed: 0,
            cooked_edit: 0,
            last_frame: None,
            verified: 0,
            cooking: false,
            cooks: 0,
        });
        if state.verified == self.epoch {
            return state.result();
        }
        if state.cooking {
            return Err(CookError::new("the network has a cycle"));
        }
        state.cooking = true;
        let cooked = state.cooked;
        let frame = self.clock.frame;
        let mut reason = state.output.is_none()
            || edit > state.cooked_edit
            || (op_type.time_dependent && state.last_frame != Some(frame));

        let mut outputs = Vec::new();
        let mut failure = None;
        for input in inputs.iter().flatten() {
            match self.ensure(*input) {
                Ok(changed) => {
                    reason |= changed > cooked;
                    outputs.push(self.states[input].output.clone().expect("checked by ensure"));
                }
                Err(_) => {
                    failure.get_or_insert_with(|| {
                        CookError::new(format!("input {} has an error", self.network.node(*input).name()))
                    });
                }
            }
        }
        let min = *op_type.inputs.start();
        if failure.is_none() && outputs.len() < min {
            failure = Some(CookError::new(format!(
                "needs at least {min} input{}",
                if min == 1 { "" } else { "s" }
            )));
        }

        let state = self.states.get_mut(&id).expect("inserted above");
        if let Some(failure) = failure {
            if state.error.as_ref() != Some(&failure) {
                self.revision += 1;
                state.changed = self.revision;
            }
            state.output = None;
            state.error = Some(failure);
            state.cooked = self.revision;
        } else if reason {
            let cap = ((MAX_SLICE_SECONDS * self.clock.rate).ceil() as i64).max(1);
            let slice_start = match state.last_frame {
                Some(last) if op_type.time_dependent && last < frame => (last + 1).max(frame - cap + 1),
                _ => frame,
            };
            let time = Time {
                frame,
                rate: self.clock.rate,
                slice_start,
            };
            let result = state.op.cook(CookCx {
                time,
                inputs: &outputs,
                values: &values,
                gpu: self.gpu.as_mut(),
                project_dir: self.project_dir.as_deref(),
            });
            self.revision += 1;
            state.cooked = self.revision;
            state.changed = self.revision;
            state.cooked_edit = edit;
            state.last_frame = Some(frame);
            state.cooks += 1;
            match result {
                Ok(output) => {
                    state.output = Some(output);
                    state.error = None;
                }
                Err(error) => {
                    state.output = None;
                    state.error = Some(error);
                }
            }
        }
        state.verified = self.epoch;
        state.cooking = false;
        state.result()
    }
}
