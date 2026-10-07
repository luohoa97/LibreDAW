// SPDX-License-Identifier: GPL-3.0-or-later
//! Audio-thread API (docs/phase2-interfaces.md). Real-time safe on our side.

use protocol::engine::{PluginEvent, PluginHandle};

/// One note event for a block. `frame` is relative to the block start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RtNote {
    pub frame: u32,
    pub key: u8,
    pub vel: u8,
    pub on: bool,
    pub note_id: u32,
}

pub struct RtBlock<'a> {
    pub frames: u32,
    pub steady_time: u64,
    pub inputs: [&'a [f32]; 2],
    pub outputs: [&'a mut [f32]; 2],
    pub notes: &'a [RtNote],
    pub params: &'a [PluginEvent],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RtStatus {
    Ok,
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RtOutKind {
    ParamValue,
    GestureBegin,
    GestureEnd,
}

/// A plugin-originated parameter event.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RtOutEvent {
    pub kind: RtOutKind,
    pub param_id: u32,
    pub value: f64,
}

impl RtOutEvent {
    pub const EMPTY: RtOutEvent = RtOutEvent {
        kind: RtOutKind::ParamValue,
        param_id: 0,
        value: 0.0,
    };
}

/// Fixed-capacity sink over a slice owned by the engine. Pushing never
/// allocates; events beyond the capacity are counted in `dropped`.
pub struct RtEventSink<'a> {
    buf: &'a mut [RtOutEvent],
    len: usize,
    dropped: u32,
}

impl<'a> RtEventSink<'a> {
    pub fn new(buf: &'a mut [RtOutEvent]) -> Self {
        RtEventSink {
            buf,
            len: 0,
            dropped: 0,
        }
    }
    pub fn push(&mut self, e: RtOutEvent) {
        if self.len < self.buf.len() {
            self.buf[self.len] = e;
            self.len += 1;
        } else {
            self.dropped = self.dropped.saturating_add(1);
        }
    }
    pub fn clear(&mut self) {
        self.len = 0;
        self.dropped = 0;
    }
    pub fn as_slice(&self) -> &[RtOutEvent] {
        &self.buf[..self.len]
    }
    pub fn dropped(&self) -> u32 {
        self.dropped
    }
}

/// # Safety
/// `h` comes from a live `host::Instance` that is activated and attached.
pub unsafe fn start_processing(_h: PluginHandle) -> bool {
    false
}

/// # Safety
/// As `start_processing`.
pub unsafe fn stop_processing(_h: PluginHandle) {}

/// # Safety
/// As `start_processing`, and `start_processing` returned true.
pub unsafe fn process(_h: PluginHandle, _block: &mut RtBlock, _out: &mut RtEventSink) -> RtStatus {
    RtStatus::Error
}
