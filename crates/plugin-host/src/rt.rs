// SPDX-License-Identifier: GPL-3.0-or-later
//! Audio-thread API (docs/phase2-interfaces.md). Real-time safe on our side:
//! no allocation, locks, logging, or syscalls. All buffers were allocated when
//! the instance was created.

use crate::inst::{AudioScope, EvSlot, Inner, MAX_IN_EVENTS};
use clap_sys::events::*;
use clap_sys::process::*;
use protocol::consts::MAX_BLOCK;
use protocol::engine::{PluginEvent, PluginHandle};
use std::sync::atomic::Ordering::*;

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

fn inner<'a>(h: PluginHandle) -> &'a Inner {
    // SAFETY: by the contract of the public fns, `h` is `Instance::handle()`
    // of a live instance; the Inner is heap-pinned.
    unsafe { &*(h.0 as *const Inner) }
}

/// Call on the audio thread when the slot is first used (SPEC 9.1).
///
/// # Safety
/// `h` comes from a live, activated `host::Instance` that the engine has
/// attached; one thread uses it as the audio thread.
pub unsafe fn start_processing(h: PluginHandle) -> bool {
    let i = inner(h);
    if !i.active.load(Acquire) {
        return false;
    }
    let _scope = AudioScope::enter(i);
    let p = i.plugin();
    // SAFETY: valid plugin; this is the audio thread.
    let ok = unsafe { (*p).start_processing.is_none_or(|f| f(p)) };
    if ok {
        i.processing.store(true, Release);
    }
    ok
}

/// # Safety
/// As `start_processing`.
pub unsafe fn stop_processing(h: PluginHandle) {
    let i = inner(h);
    if i.processing.swap(false, AcqRel) {
        let _scope = AudioScope::enter(i);
        let p = i.plugin();
        // SAFETY: valid plugin; this is the audio thread.
        unsafe {
            if let Some(f) = (*p).stop_processing {
                f(p);
            }
        }
    }
}

fn header(size: usize, time: u32, type_: u16) -> clap_event_header {
    clap_event_header {
        size: size as u32,
        time,
        space_id: CLAP_CORE_EVENT_SPACE_ID,
        type_,
        flags: 0,
    }
}

/// Process one block.
///
/// # Safety
/// As `start_processing`, and `start_processing` returned true.
pub unsafe fn process(h: PluginHandle, block: &mut RtBlock, out: &mut RtEventSink) -> RtStatus {
    let i = inner(h);
    let n = block.frames as usize;
    if !i.processing.load(Acquire) || n > MAX_BLOCK {
        return RtStatus::Error;
    }
    if n == 0 {
        return RtStatus::Ok;
    }
    if block.outputs.iter().any(|b| b.len() < n) {
        return RtStatus::Error;
    }
    let _scope = AudioScope::enter(i);
    let rt = i.rt.get();
    // SAFETY: the audio thread is the only user of `rt` outside plugin
    // callbacks, which reach it through the same raw pointer. No references
    // into it are held across the plugin call.
    unsafe {
        if (*rt).has_input {
            if block.inputs.iter().any(|b| b.len() < n) {
                return RtStatus::Error;
            }
            (*rt).in_ptrs = [
                block.inputs[0].as_ptr() as *mut f32,
                block.inputs[1].as_ptr() as *mut f32,
            ];
            (*rt).in_buf.data32 = (*rt).in_ptrs.as_mut_ptr();
        }
        (*rt).out_ptrs = [block.outputs[0].as_mut_ptr(), block.outputs[1].as_mut_ptr()];
        (*rt).out_buf.data32 = (*rt).out_ptrs.as_mut_ptr();
        (*rt).n_events = 0;
        (*rt).n_out = 0;

        let push = |ev: EvSlot| {
            let k = (*rt).n_events;
            if k < MAX_IN_EVENTS {
                *(*rt).events.as_mut_ptr().add(k) = ev;
                (*rt).n_events = k + 1;
            } else {
                (*rt).dropped = (*rt).dropped.saturating_add(1);
            }
        };
        // Parameter values first (time 0), then notes in frame order.
        for p in block.params {
            push(EvSlot {
                param: clap_event_param_value {
                    header: header(
                        size_of::<clap_event_param_value>(),
                        0,
                        CLAP_EVENT_PARAM_VALUE,
                    ),
                    param_id: p.param_id,
                    cookie: std::ptr::null_mut(),
                    note_id: -1,
                    port_index: -1,
                    channel: -1,
                    key: -1,
                    value: p.value,
                },
            });
        }
        for nt in block.notes {
            push(EvSlot {
                note: clap_event_note {
                    header: header(
                        size_of::<clap_event_note>(),
                        nt.frame.min(block.frames - 1),
                        if nt.on {
                            CLAP_EVENT_NOTE_ON
                        } else {
                            CLAP_EVENT_NOTE_OFF
                        },
                    ),
                    note_id: nt.note_id as i32,
                    port_index: 0,
                    channel: 0,
                    key: i16::from(nt.key),
                    velocity: f64::from(nt.vel) / 127.0,
                },
            });
        }

        let pr = clap_process {
            steady_time: block.steady_time as i64,
            frames_count: block.frames,
            transport: std::ptr::null(),
            audio_inputs: std::ptr::addr_of!((*rt).in_buf),
            audio_outputs: std::ptr::addr_of_mut!((*rt).out_buf),
            audio_inputs_count: u32::from((*rt).has_input),
            audio_outputs_count: 1,
            in_events: std::ptr::addr_of!((*rt).in_list),
            out_events: std::ptr::addr_of!((*rt).out_list),
        };
        let p = i.plugin();
        let status = (*p).process.map_or(CLAP_PROCESS_ERROR, |f| f(p, &pr));
        for k in 0..(*rt).n_out {
            out.push(*(*rt).out.as_ptr().add(k));
        }
        if status == CLAP_PROCESS_ERROR {
            RtStatus::Error
        } else {
            RtStatus::Ok
        }
    }
}
