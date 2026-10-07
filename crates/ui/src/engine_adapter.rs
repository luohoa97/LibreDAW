// SPDX-License-Identifier: GPL-3.0-or-later
//! The only module that talks to the `engine` crate (docs/phase2-interfaces.md,
//! "engine -> ui").
//!
//! The engine crate's `compile`, `Slots`, and `Engine` are not on main yet,
//! so this is a stub with the same shape: `compile` builds a small summary
//! instead of a `Compiled`, and `EngineLink` keeps the commands, plugin
//! events, and compiled states it is given instead of feeding an audio
//! thread. When the engine lands, only this file changes: `Compiled` becomes
//! `Box<engine::Compiled>`, `compile` calls `engine::compile` with a
//! `engine::Slots` built from our `SlotAllocator`, and `EngineLink` wraps
//! `engine::Engine`.

use std::collections::VecDeque;
use std::sync::Arc;

use protocol::engine::{
    ControlTable, EngineCommand, EngineEvent, EngineStatus, ParamTable, PluginEvent,
};

use crate::compiler::CompileJob;

/// Stand-in for `engine::Compiled`.
#[derive(Debug, PartialEq, Eq)]
pub struct Compiled {
    pub revision: u64,
    pub channels: usize,
    pub notes: usize,
}

/// Stand-in for `engine::compile`. Runs on the compiler thread.
pub fn compile(job: &CompileJob) -> Box<Compiled> {
    Box::new(Compiled {
        revision: job.revision,
        channels: job.project.channels.len(),
        notes: job.project.note_count(),
    })
}

/// The GTK-thread side of the engine: the shared tables and the producer
/// ends of the rings.
pub struct EngineLink {
    pub controls: Arc<ControlTable>,
    pub params: Arc<ParamTable>,
    pub status: Arc<EngineStatus>,
    sample_rate: f64,
    /// Capacity of the stub's rings; `None` means unlimited. Tests set it to
    /// exercise the "ring full, retry" paths.
    pub ring_capacity: Option<usize>,
    pub submitted: Vec<Box<Compiled>>,
    pub commands: Vec<EngineCommand>,
    pub plugin_events: Vec<PluginEvent>,
    pub events: VecDeque<EngineEvent>,
}

impl EngineLink {
    /// A link that is not connected to an audio stream.
    pub fn stub(sample_rate: f64) -> EngineLink {
        EngineLink {
            controls: Arc::new(ControlTable::new()),
            params: Arc::new(ParamTable::new()),
            status: Arc::new(EngineStatus::new()),
            sample_rate,
            ring_capacity: None,
            submitted: Vec::new(),
            commands: Vec::new(),
            plugin_events: Vec::new(),
            events: VecDeque::new(),
        }
    }

    pub fn sample_rate(&self) -> f64 {
        self.sample_rate
    }

    /// State ring. `Err` means full: retry on the next tick.
    pub fn submit(&mut self, c: Box<Compiled>) -> Result<(), Box<Compiled>> {
        if self
            .ring_capacity
            .is_some_and(|n| self.submitted.len() >= n)
        {
            return Err(c);
        }
        self.submitted.push(c);
        Ok(())
    }

    /// Command ring. `Err` means full.
    pub fn command(&mut self, c: EngineCommand) -> Result<(), EngineCommand> {
        if self.ring_capacity.is_some_and(|n| self.commands.len() >= n) {
            return Err(c);
        }
        self.commands.push(c);
        Ok(())
    }

    /// Plugin event ring. `Err` means full.
    pub fn plugin_event(&mut self, e: PluginEvent) -> Result<(), PluginEvent> {
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
        while let Some(e) = self.events.pop_front() {
            f(e);
        }
    }
}
