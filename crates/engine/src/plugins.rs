// SPDX-License-Identifier: GPL-3.0-or-later
//! The engine's only contact with `plugin_host::rt` (docs/phase2-interfaces.md).
//! Everything else in the engine goes through `PluginApi`, so tests can swap
//! in a fake plugin and a signature change in `plugin_host` touches this file
//! only.

use plugin_host::rt::{self, RtBlock, RtEventSink, RtNote, RtOutEvent, RtOutKind, RtStatus};
use protocol::engine::{EngineEvent, PluginEvent, PluginHandle, PluginSlot};

pub use plugin_host::rt::{RtNote as PluginNote, RtOutEvent as PluginOutEvent};

/// Capacity of the plugin output event scratch per `process()` call.
pub const OUT_EVENT_CAP: usize = 512;

/// Plugin-originated events of one call (the sink is preallocated).
pub struct OutEvents {
    pub buf: Vec<RtOutEvent>,
    pub len: usize,
    /// Events the host dropped because the sink was full.
    pub dropped: u32,
}

impl OutEvents {
    pub fn new() -> OutEvents {
        OutEvents {
            buf: vec![RtOutEvent::EMPTY; OUT_EVENT_CAP],
            len: 0,
            dropped: 0,
        }
    }
}

impl Default for OutEvents {
    fn default() -> OutEvents {
        OutEvents::new()
    }
}

/// One `process()` call.
pub struct ProcessArgs<'a> {
    pub frames: usize,
    pub steady_time: u64,
    pub in_l: &'a [f32],
    pub in_r: &'a [f32],
    pub out_l: &'a mut [f32],
    pub out_r: &'a mut [f32],
    pub notes: &'a [PluginNote],
    pub params: &'a [PluginEvent],
}

/// The three audio-thread entry points. The default is the real host.
#[derive(Clone, Copy)]
pub struct PluginApi {
    pub start: unsafe fn(PluginHandle) -> bool,
    pub stop: unsafe fn(PluginHandle),
    /// Returns true on success; output events go into `out`.
    pub process: unsafe fn(PluginHandle, &mut ProcessArgs, &mut OutEvents) -> bool,
}

unsafe fn real_start(h: PluginHandle) -> bool {
    unsafe { rt::start_processing(h) }
}

unsafe fn real_stop(h: PluginHandle) {
    unsafe { rt::stop_processing(h) }
}

unsafe fn real_process(h: PluginHandle, a: &mut ProcessArgs, out: &mut OutEvents) -> bool {
    let frames = a.frames;
    let mut sink = RtEventSink::new(&mut out.buf);
    let [ol, or] = [&mut *a.out_l, &mut *a.out_r];
    let mut block = RtBlock {
        frames: frames as u32,
        steady_time: a.steady_time,
        inputs: [&a.in_l[..frames], &a.in_r[..frames]],
        outputs: [&mut ol[..frames], &mut or[..frames]],
        notes: a.notes,
        params: a.params,
    };
    // SAFETY: the caller guarantees `h` is attached and processing was started.
    let status = unsafe { rt::process(h, &mut block, &mut sink) };
    out.len = sink.as_slice().len();
    out.dropped = sink.dropped();
    status == RtStatus::Ok
}

impl PluginApi {
    pub const fn real() -> PluginApi {
        PluginApi {
            start: real_start,
            stop: real_stop,
            process: real_process,
        }
    }
}

/// Converts a plugin output event to the ring record.
pub fn out_event_to_engine(slot: PluginSlot, e: &RtOutEvent) -> EngineEvent {
    match e.kind {
        RtOutKind::ParamValue => EngineEvent::PluginParamChanged {
            slot,
            param_id: e.param_id,
            value: e.value,
        },
        RtOutKind::GestureBegin => EngineEvent::PluginGestureBegin {
            slot,
            param_id: e.param_id,
        },
        RtOutKind::GestureEnd => EngineEvent::PluginGestureEnd {
            slot,
            param_id: e.param_id,
        },
    }
}

pub fn note(frame: u32, key: u8, vel: u8, on: bool, note_id: u32) -> RtNote {
    RtNote {
        frame,
        key,
        vel,
        on,
        note_id,
    }
}
