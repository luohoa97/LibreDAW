// SPDX-License-Identifier: GPL-3.0-or-later
//! The only module that names the `engine` crate (docs/phase2-interfaces.md,
//! "engine -> ui").
//!
//! `EngineLink` is the GTK-thread side of the engine. It is either live (an
//! open audio stream, `Engine`) or a stub with the same ring semantics that
//! keeps what it is given. The stub serves tests and the case where no audio
//! device can be opened: the document, undo, save, and the control socket
//! still work, and the app tells the user audio is off.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32};

use protocol::control::WavFormat;
use protocol::engine::{
    ControlTable, EngineCommand, EngineEvent, EngineStatus, ParamTable, PluginEvent, PluginHandle,
    PluginSlot,
};
use protocol::ids::PatternId;
use protocol::model::Project;

use crate::compiler::CompileJob;
use crate::slots::SlotAllocator;

pub use engine::render::Rendered;
pub use engine::{Compiled, EngineConfig, EngineError, Host, SampleStore, Slots};

/// Runs on the compiler thread.
pub fn compile(job: &CompileJob) -> Box<Compiled> {
    engine::compile_with(
        &job.project,
        &job.slots.inner,
        job.sample_rate,
        job.store.as_deref(),
    )
}

/// Writes every control and parameter value of `project` (4.3, 17.1).
pub fn write_controls(project: &Project, slots: &SlotAllocator, link: &EngineLink) {
    engine::write_controls(project, &slots.inner, &link.controls, &link.params);
}

/// Notes in all patterns of a compiled state (for tests and diagnostics).
pub fn compiled_note_count(c: &Compiled) -> usize {
    c.patterns
        .iter()
        .map(|p| p.notes.iter().map(Vec::len).sum::<usize>())
        .sum()
}

/// Output device names of a host.
pub fn devices(host: Host) -> Vec<String> {
    engine::Engine::devices(host)
}

/// What to render: a pinned project, a pattern (or the whole song), loops,
/// and a rate.
pub struct RenderJob {
    pub project: Arc<Project>,
    pub pattern: PatternId,
    pub loops: u32,
    pub sample_rate: u32,
    /// `Some(tail)` renders the whole playlist plus `tail` seconds instead
    /// of `pattern` (15.6).
    pub song_tail: Option<f64>,
    /// Decoded samples for samplers.
    pub store: Option<Arc<SampleStore>>,
}

/// Offline render on the calling thread (8). `plugins` are export instances
/// created on the GTK thread and attached by handle.
pub fn render(
    job: RenderJob,
    slots: &SlotAllocator,
    plugins: &[(PluginSlot, PluginHandle)],
    progress: &AtomicU32,
    cancel: &AtomicBool,
) -> Result<Rendered, EngineError> {
    if let Some(tail) = job.song_tail {
        let req = engine::SongRequest {
            project: job.project,
            tail_seconds: tail,
            store: job.store,
            sample_rate: job.sample_rate,
        };
        return engine::render_song(&req, &slots.inner, plugins, progress, cancel);
    }
    let req = engine::RenderRequest {
        project: job.project,
        pattern: job.pattern,
        loops: job.loops,
        tail_seconds: 2.0,
        store: job.store,
        sample_rate: job.sample_rate,
    };
    engine::render_offline(&req, &slots.inner, plugins, progress, cancel)
}

pub fn write_wav(
    path: &Path,
    frames: &[[f32; 2]],
    rate: u32,
    fmt: WavFormat,
) -> std::io::Result<()> {
    engine::write_wav(path, frames, rate, fmt)
}

/// The GTK-thread side of the engine: the shared tables and the producer
/// ends of the rings.
pub struct EngineLink {
    pub controls: Arc<ControlTable>,
    pub params: Arc<ParamTable>,
    pub status: Arc<EngineStatus>,
    sample_rate: f64,
    live: Option<engine::Engine>,
    /// Stub only: capacity of the rings; `None` means unlimited. Tests set it
    /// to exercise the "ring full, retry" paths.
    pub ring_capacity: Option<usize>,
    /// Stub only: what was given to the rings, newest last (bounded).
    pub submitted: Vec<Box<Compiled>>,
    pub commands: Vec<EngineCommand>,
    pub plugin_events: Vec<PluginEvent>,
    /// Stub only: events the test wants the "audio thread" to have sent.
    pub events: VecDeque<EngineEvent>,
}

/// How many submitted states the stub keeps.
const STUB_KEEP: usize = 8;

impl EngineLink {
    /// A link that is not connected to an audio stream.
    pub fn stub(sample_rate: f64) -> EngineLink {
        EngineLink {
            controls: Arc::new(ControlTable::new()),
            params: Arc::new(ParamTable::new()),
            status: Arc::new(EngineStatus::new()),
            sample_rate,
            live: None,
            ring_capacity: None,
            submitted: Vec::new(),
            commands: Vec::new(),
            plugin_events: Vec::new(),
            events: VecDeque::new(),
        }
    }

    /// Opens the audio stream with `first` installed.
    pub fn start(cfg: &EngineConfig, first: Box<Compiled>) -> Result<EngineLink, EngineError> {
        let e = engine::Engine::start(cfg, first)?;
        Ok(EngineLink {
            controls: e.controls.clone(),
            params: e.params.clone(),
            status: e.status.clone(),
            sample_rate: e.sample_rate(),
            live: Some(e),
            ring_capacity: None,
            submitted: Vec::new(),
            commands: Vec::new(),
            plugin_events: Vec::new(),
            events: VecDeque::new(),
        })
    }

    pub fn is_live(&self) -> bool {
        self.live.is_some()
    }

    /// True if the backend lost the device and the stream must be rebuilt.
    pub fn needs_restart(&self) -> bool {
        self.live.as_ref().is_some_and(|e| e.needs_restart())
    }

    /// Stops the stream (4.7 step 1) and waits for it to be dropped.
    pub fn stop(&mut self) {
        if let Some(e) = self.live.take() {
            e.stop();
        }
    }

    pub fn sample_rate(&self) -> f64 {
        self.sample_rate
    }

    /// State ring. `Err` means full: retry on the next tick.
    pub fn submit(&mut self, c: Box<Compiled>) -> Result<(), Box<Compiled>> {
        if let Some(e) = &mut self.live {
            return e.submit(c);
        }
        if self
            .ring_capacity
            .is_some_and(|n| self.submitted.len() >= n)
        {
            return Err(c);
        }
        self.submitted.push(c);
        if self.ring_capacity.is_none() && self.submitted.len() > STUB_KEEP {
            self.submitted.remove(0);
        }
        Ok(())
    }

    /// Command ring. `Err` means full.
    pub fn command(&mut self, c: EngineCommand) -> Result<(), EngineCommand> {
        if let Some(e) = &mut self.live {
            return e.command(c);
        }
        if self.ring_capacity.is_some_and(|n| self.commands.len() >= n) {
            return Err(c);
        }
        self.commands.push(c);
        Ok(())
    }

    /// Plugin event ring. `Err` means full.
    pub fn plugin_event(&mut self, e: PluginEvent) -> Result<(), PluginEvent> {
        if let Some(l) = &mut self.live {
            return l.plugin_event(e);
        }
        if self
            .ring_capacity
            .is_some_and(|n| self.plugin_events.len() >= n)
        {
            return Err(e);
        }
        self.plugin_events.push(e);
        Ok(())
    }

    pub fn drain_events(&mut self, mut f: impl FnMut(EngineEvent)) {
        if let Some(e) = &mut self.live {
            e.drain_events(f);
            return;
        }
        while let Some(e) = self.events.pop_front() {
            f(e);
        }
    }
}
