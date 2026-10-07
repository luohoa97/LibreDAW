// SPDX-License-Identifier: GPL-3.0-or-later
//! `Runtime`: the mutable audio-thread state (SPEC 4.1, 4.2, 4.5). Allocated
//! once, sized to the fixed maxima, never reallocated while the stream runs.
//! The live callback and the offline renderer call the same `process_*`.

use crate::compiled::{Compiled, InstrumentC};
use crate::metronome::Click;
use crate::mixer::{Fader, MuteSolo, SMOOTH_SECONDS, resolve_solo};
use crate::plugins::{
    OUT_EVENT_CAP, OutEvents, PluginApi, PluginNote, ProcessArgs, note, out_event_to_engine,
};
use crate::preview::{PREVIEW_CAP, PreviewNote, Previews, TAIL_SECONDS, insert_sorted};
use crate::rt::{RtGuard, enter_rt_fp_mode, restore_fp_mode};
use crate::sequencer::{BEAT_CAP, Beat, EVENT_CAP, SeqEvent, Sequencer, TraceEvent};
use crate::synth::{Synth, SynthCtl};
use protocol::consts::{
    COMMAND_RING_CAP, EVENT_RING_CAP, MAX_BLOCK, MAX_CHANNELS, MAX_INSERTS, MAX_TEMPO_BPM,
    MIN_TEMPO_BPM, PLUGIN_EVENT_RING_CAP, RETIRE_RING_CAP, STATE_RING_CAP, TRACK_SLOTS,
};
use protocol::engine::{
    CTL_METRONOME_ENABLED, CTL_METRONOME_GAIN_DB, ChannelSlot, ControlTable, EngineCommand,
    EngineEvent, EngineStatus, MixControl, PLUGIN_SLOTS, PREVIEW_MAX_SECONDS, ParamTable,
    PluginEvent, PluginHandle, PluginSlot, SlotGen, TrackSlot, channel_control, track_control,
};
use rtrb::{Consumer, Producer, RingBuffer};
use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;
use std::time::Instant;

/// The atomics shared with the GTK thread (SPEC 4.3, 4.4, 17.1).
#[derive(Clone)]
pub struct Shared {
    pub controls: Arc<ControlTable>,
    pub params: Arc<ParamTable>,
    pub status: Arc<EngineStatus>,
}

impl Shared {
    pub fn new() -> Shared {
        Shared {
            controls: Arc::new(ControlTable::new()),
            params: Arc::new(ParamTable::new()),
            status: Arc::new(EngineStatus::new()),
        }
    }
}

impl Default for Shared {
    fn default() -> Shared {
        Shared::new()
    }
}

/// Ring ends held by the audio thread.
pub struct RtEnds {
    pub state: Consumer<Box<Compiled>>,
    pub retire: Producer<Box<Compiled>>,
    pub commands: Consumer<EngineCommand>,
    pub plugin_events: Consumer<PluginEvent>,
    pub events: Producer<EngineEvent>,
}

/// Ring ends held by the GTK side (the retired end goes to the disposal thread).
pub struct UiEnds {
    pub state: Producer<Box<Compiled>>,
    pub retired: Consumer<Box<Compiled>>,
    pub commands: Producer<EngineCommand>,
    pub plugin_events: Producer<PluginEvent>,
    pub events: Consumer<EngineEvent>,
}

/// Creates all rings with the capacities from `protocol::consts`.
pub fn rings() -> (UiEnds, RtEnds) {
    const { assert!(RETIRE_RING_CAP > STATE_RING_CAP) };
    let (state_p, state_c) = RingBuffer::new(STATE_RING_CAP);
    let (retire_p, retire_c) = RingBuffer::new(RETIRE_RING_CAP);
    let (cmd_p, cmd_c) = RingBuffer::new(COMMAND_RING_CAP);
    let (pe_p, pe_c) = RingBuffer::new(PLUGIN_EVENT_RING_CAP);
    let (ev_p, ev_c) = RingBuffer::new(EVENT_RING_CAP);
    (
        UiEnds {
            state: state_p,
            retired: retire_c,
            commands: cmd_p,
            plugin_events: pe_p,
            events: ev_c,
        },
        RtEnds {
            state: state_c,
            retire: retire_p,
            commands: cmd_c,
            plugin_events: pe_c,
            events: ev_p,
        },
    )
}

#[derive(Clone, Copy)]
struct PluginEntry {
    handle: Option<PluginHandle>,
    started: bool,
    failed: bool,
}

/// The plugin table and the scratch the plugin calls need.
struct PluginState {
    entries: Vec<PluginEntry>,
    api: PluginApi,
    out_events: OutEvents,
    rt_notes: Vec<PluginNote>,
    /// This sub-block's host-to-plugin events, and the current plugin's share.
    pe: Vec<PluginEvent>,
    pe_slot: Vec<PluginEvent>,
    /// Detach acks that did not fit in the event ring yet.
    pending_acks: Vec<bool>,
    n_pending: usize,
}

fn push_engine_event(tx: &mut Producer<EngineEvent>, status: &EngineStatus, e: EngineEvent) {
    if tx.push(e).is_err() {
        status.event_overflows.fetch_add(1, Relaxed);
    }
}

impl PluginState {
    fn new() -> PluginState {
        PluginState {
            entries: vec![
                PluginEntry {
                    handle: None,
                    started: false,
                    failed: false
                };
                PLUGIN_SLOTS
            ],
            api: PluginApi::real(),
            out_events: OutEvents::new(),
            rt_notes: Vec::with_capacity(EVENT_CAP),
            pe: Vec::with_capacity(PLUGIN_EVENT_RING_CAP),
            pe_slot: Vec::with_capacity(PLUGIN_EVENT_RING_CAP),
            pending_acks: vec![false; PLUGIN_SLOTS],
            n_pending: 0,
        }
    }

    fn attached(&self, ps: PluginSlot) -> bool {
        self.entries[ps.index()].handle.is_some()
    }

    fn attach(&mut self, ps: PluginSlot, handle: PluginHandle) {
        let e = &mut self.entries[ps.index()];
        if e.handle.is_none() {
            *e = PluginEntry {
                handle: Some(handle),
                started: false,
                failed: false,
            };
        }
    }

    /// Stops and clears the slot; the caller acks.
    fn detach(&mut self, ps: PluginSlot) {
        let e = &mut self.entries[ps.index()];
        if let Some(h) = e.handle.take()
            && e.started
        {
            // SAFETY: attached and started; the GTK thread keeps the instance
            // alive until it sees the ack.
            unsafe { (self.api.stop)(h) };
        }
        *e = PluginEntry {
            handle: None,
            started: false,
            failed: false,
        };
    }

    /// Runs one plugin for `n` frames. `notes_slot` selects the channel
    /// whose note events are sent (instruments). Returns whether the output
    /// is valid. Records `process()` time (SPEC 3.3).
    #[allow(clippy::too_many_arguments)]
    fn call(
        &mut self,
        ps: PluginSlot,
        n: usize,
        steady: u64,
        (in_l, in_r): (&[f32], &[f32]),
        (out_l, out_r): (&mut [f32], &mut [f32]),
        events: &[SeqEvent],
        notes_slot: Option<u16>,
        status: &EngineStatus,
        tx: &mut Producer<EngineEvent>,
    ) -> bool {
        let idx = ps.index();
        let Some(h) = self.entries[idx].handle else {
            return false;
        };
        if self.entries[idx].failed {
            return false;
        }
        if !self.entries[idx].started {
            // SAFETY: attached by command; first use starts processing (9.1).
            let ok = unsafe { (self.api.start)(h) };
            self.entries[idx].started = ok;
            self.entries[idx].failed = !ok;
            if !ok {
                return false;
            }
        }
        self.rt_notes.clear();
        if let Some(s) = notes_slot {
            for e in events.iter().filter(|e| e.slot == s) {
                if self.rt_notes.len() < self.rt_notes.capacity() {
                    self.rt_notes.push(note(e.offset, e.key, e.vel, e.on, e.id));
                }
            }
        }
        self.pe_slot.clear();
        for e in self.pe.iter().filter(|e| e.slot == ps) {
            if self.pe_slot.len() < self.pe_slot.capacity() {
                self.pe_slot.push(*e);
            }
        }
        let mut args = ProcessArgs {
            frames: n,
            steady_time: steady,
            in_l,
            in_r,
            out_l,
            out_r,
            notes: &self.rt_notes,
            params: &self.pe_slot,
        };
        let t0 = Instant::now();
        // SAFETY: attached and started above.
        let ok = unsafe { (self.api.process)(h, &mut args, &mut self.out_events) };
        let us = t0.elapsed().as_micros().min(u32::MAX as u128) as u32;
        status.plugin_last_us[idx].store(us, Relaxed);
        status.plugin_max_us[idx].fetch_max(us, Relaxed);
        if self.out_events.dropped > 0 {
            status
                .event_overflows
                .fetch_add(self.out_events.dropped as u64, Relaxed);
        }
        for e in &self.out_events.buf[..self.out_events.len.min(OUT_EVENT_CAP)] {
            push_engine_event(tx, status, out_event_to_engine(ps, e));
        }
        ok
    }
}

type Lr<'a> = (&'a [f32], &'a [f32]);
type LrMut<'a> = (&'a mut [f32], &'a mut [f32]);

/// Stereo bus scratch: `TRACK_SLOTS` buses of left then right.
struct Buses {
    data: Vec<f32>,
}

impl Buses {
    fn new() -> Buses {
        Buses {
            data: vec![0.0; TRACK_SLOTS * 2 * MAX_BLOCK],
        }
    }

    /// Zeroes the first `n` frames of every lane.
    fn clear(&mut self, n: usize) {
        for lane in self.data.chunks_exact_mut(MAX_BLOCK) {
            lane[..n].fill(0.0);
        }
    }

    fn lr(&mut self, t: usize, n: usize) -> (&mut [f32], &mut [f32]) {
        let (l, r) = self.data[t * 2 * MAX_BLOCK..(t + 1) * 2 * MAX_BLOCK].split_at_mut(MAX_BLOCK);
        (&mut l[..n], &mut r[..n])
    }

    /// Track `t` (>0) as read-only and the master as writable.
    fn track_and_master(&mut self, t: usize, n: usize) -> (Lr<'_>, LrMut<'_>) {
        debug_assert!(t > 0);
        let (head, tail) = self.data.split_at_mut(2 * MAX_BLOCK);
        let off = (t - 1) * 2 * MAX_BLOCK;
        let (tl, tr) = tail[off..off + 2 * MAX_BLOCK].split_at(MAX_BLOCK);
        let (ml, mr) = head.split_at_mut(MAX_BLOCK);
        ((&tl[..n], &tr[..n]), (&mut ml[..n], &mut mr[..n]))
    }
}

pub struct Runtime {
    sample_rate: f64,
    ramp: u32,
    shared: Shared,
    ends: RtEnds,
    compiled: Option<Box<Compiled>>,
    chan_gen: [SlotGen; MAX_CHANNELS],
    track_gen: [SlotGen; TRACK_SLOTS],
    seq: Sequencer,
    synths: Vec<Synth>,
    chan_fader: Vec<Fader>,
    track_fader: Vec<Fader>,
    plug: PluginState,
    events: Vec<SeqEvent>,
    beats: Vec<Beat>,
    trace: Vec<TraceEvent>,
    buses: Buses,
    track_dirty: [bool; TRACK_SLOTS],
    mono: Vec<f32>,
    chan_l: Vec<f32>,
    chan_r: Vec<f32>,
    tmp_l: Vec<f32>,
    tmp_r: Vec<f32>,
    silence: Vec<f32>,
    click_buf: Vec<f32>,
    click: Click,
    /// Offline render turns the metronome off (SPEC 8).
    metronome_allowed: bool,
    ch_ms: Vec<MuteSolo>,
    ch_track: Vec<u16>,
    tr_ms: Vec<MuteSolo>,
    ch_audible: Vec<bool>,
    tr_audible: Vec<bool>,
    previews: Previews,
    previews_allowed: bool,
    /// Frames left in which a previewed channel stays solo-exempt.
    preview_tail: [u32; MAX_CHANNELS],
}

impl Runtime {
    /// Allocates everything. Call off the audio thread.
    pub fn new(sample_rate: f64, shared: Shared, ends: RtEnds) -> Runtime {
        let bpm = shared.controls.tempo().clamp(MIN_TEMPO_BPM, MAX_TEMPO_BPM);
        Runtime {
            sample_rate,
            ramp: (sample_rate * SMOOTH_SECONDS).round().max(1.0) as u32,
            seq: Sequencer::new(sample_rate, bpm),
            shared,
            ends,
            compiled: None,
            chan_gen: [0; MAX_CHANNELS],
            track_gen: [0; TRACK_SLOTS],
            synths: vec![Synth::new(); MAX_CHANNELS],
            chan_fader: vec![Fader::default(); MAX_CHANNELS],
            track_fader: vec![Fader::default(); TRACK_SLOTS],
            plug: PluginState::new(),
            events: Vec::with_capacity(EVENT_CAP),
            beats: Vec::with_capacity(BEAT_CAP),
            trace: Vec::new(),
            buses: Buses::new(),
            track_dirty: [false; TRACK_SLOTS],
            mono: vec![0.0; MAX_BLOCK],
            chan_l: vec![0.0; MAX_BLOCK],
            chan_r: vec![0.0; MAX_BLOCK],
            tmp_l: vec![0.0; MAX_BLOCK],
            tmp_r: vec![0.0; MAX_BLOCK],
            silence: vec![0.0; MAX_BLOCK],
            click_buf: vec![0.0; MAX_BLOCK],
            click: Click::IDLE,
            metronome_allowed: true,
            ch_ms: vec![MuteSolo::default(); MAX_CHANNELS],
            ch_track: vec![0; MAX_CHANNELS],
            tr_ms: vec![MuteSolo::default(); TRACK_SLOTS],
            ch_audible: vec![false; MAX_CHANNELS],
            tr_audible: vec![false; TRACK_SLOTS],
            previews: Previews::new(),
            previews_allowed: true,
            preview_tail: [0; MAX_CHANNELS],
        }
    }

    /// Offline render turns previews off (they are an audition feature).
    pub fn set_previews_allowed(&mut self, allowed: bool) {
        self.previews_allowed = allowed;
    }

    fn preview_event(&mut self, offset: u32, slot: u16, key: u8, vel: u8, on: bool, id: u32) {
        insert_sorted(
            &mut self.events,
            SeqEvent {
                offset,
                slot,
                key,
                vel,
                on,
                id,
            },
        );
    }

    fn tail_frames(&self) -> u32 {
        (TAIL_SECONDS * self.sample_rate) as u32
    }

    fn preview_command(&mut self, channel: ChannelSlot, key: u8, vel: u8, on: bool) {
        let slot = channel.0;
        if !self.previews_allowed || slot as usize >= MAX_CHANNELS || key > 127 {
            return;
        }
        let existing = self.previews.find(slot, key);
        if !on {
            if let Some(i) = existing {
                let id = self.previews.notes[i].id;
                self.previews.notes[i].active = false;
                self.preview_event(0, slot, key, 0, false, id);
                self.preview_tail[slot as usize] = self.tail_frames();
            }
            return;
        }
        let i = match existing {
            Some(i) => {
                // Retrigger: release the old note first.
                let id = self.previews.notes[i].id;
                self.preview_event(0, slot, key, 0, false, id);
                i
            }
            None => match self.previews.free_index() {
                Some(i) => i,
                None => return,
            },
        };
        let id = self.previews.fresh_id();
        let left = (PREVIEW_MAX_SECONDS * self.sample_rate).round() as u32;
        self.previews.notes[i] = PreviewNote {
            active: true,
            slot,
            key,
            id,
            left,
        };
        self.preview_event(0, slot, key, vel.clamp(1, 127), true, id);
        self.preview_tail[slot as usize] = self.tail_frames();
    }

    /// Releases every preview note on `slot` (or all slots with `None`).
    fn release_previews(&mut self, only: Option<u16>) {
        for i in 0..PREVIEW_CAP {
            let n = self.previews.notes[i];
            if n.active && only.is_none_or(|s| s == n.slot) {
                self.previews.notes[i].active = false;
                self.preview_event(0, n.slot, n.key, 0, false, n.id);
            }
        }
    }

    /// Auto-release for a sub-block of `n` frames; call after the events are final.
    fn preview_timers(&mut self, n: usize) {
        for i in 0..PREVIEW_CAP {
            let p = self.previews.notes[i];
            if !p.active {
                continue;
            }
            if (p.left as usize) < n {
                self.previews.notes[i].active = false;
                self.preview_event(p.left, p.slot, p.key, 0, false, p.id);
            } else {
                self.previews.notes[i].left -= n as u32;
            }
        }
    }

    pub fn sample_rate(&self) -> f64 {
        self.sample_rate
    }

    /// Replaces the plugin entry points (tests use a fake plugin).
    pub fn set_plugin_api(&mut self, api: PluginApi) {
        self.plug.api = api;
    }

    pub fn set_metronome_allowed(&mut self, allowed: bool) {
        self.metronome_allowed = allowed;
    }

    /// Records every note event, up to `cap` of them, with its absolute
    /// sample. Allocates; call before streaming.
    pub fn enable_trace(&mut self, cap: usize) {
        self.trace = Vec::with_capacity(cap);
    }

    pub fn trace(&self) -> &[TraceEvent] {
        &self.trace
    }

    /// Absolute sample of the next sub-block.
    pub fn position(&self) -> u64 {
        self.seq.pos
    }

    pub fn is_playing(&self) -> bool {
        self.seq.playing
    }

    /// Live note bits of a channel slot (bit = key).
    pub fn live_notes(&self, slot: ChannelSlot) -> u128 {
        self.seq.live_notes(slot.0)
    }

    pub fn active_voices(&self, slot: ChannelSlot) -> usize {
        self.synths[slot.0 as usize].active_voices()
    }

    pub fn has_compiled(&self) -> bool {
        self.compiled.is_some()
    }

    /// The installed state, for tests and diagnostics.
    pub fn compiled(&self) -> Option<&Compiled> {
        self.compiled.as_deref()
    }

    /// Installs `c` as the current state without the ring (offline render,
    /// stream start). Returns the previous state for the caller to free.
    pub fn install(&mut self, c: Box<Compiled>) -> Option<Box<Compiled>> {
        self.apply_generations(&c);
        self.seq.on_install(&c);
        self.compiled.replace(c)
    }

    fn apply_generations(&mut self, new: &Compiled) {
        for s in 0..MAX_CHANNELS {
            if new.channel_gen[s] != self.chan_gen[s] {
                // Note-offs for the slot's active notes first (SPEC 4.1).
                self.seq.release_slot(s as u16, 0, &mut self.events);
                self.release_previews(Some(s as u16));
                self.preview_tail[s] = 0;
                self.synths[s].reset();
                self.chan_fader[s].reset();
                self.chan_gen[s] = new.channel_gen[s];
            }
        }
        for t in 0..TRACK_SLOTS {
            if new.track_gen[t] != self.track_gen[t] {
                self.track_fader[t].reset();
                self.track_gen[t] = new.track_gen[t];
            }
        }
    }

    /// SPEC 4.2. At a sub-block boundary: if states are pending and the
    /// retire ring has room for all of them plus the current one, pop all,
    /// keep the newest, and retire the rest. Never frees here.
    fn try_swap(&mut self) {
        let n = self.ends.state.slots();
        if n == 0 || self.ends.retire.slots() < n + 1 {
            return;
        }
        let mut newest: Option<Box<Compiled>> = None;
        for _ in 0..n {
            if let Ok(b) = self.ends.state.pop()
                && let Some(skipped) = newest.replace(b)
            {
                let _ = self.ends.retire.push(skipped);
            }
        }
        if let Some(new) = newest
            && let Some(old) = self.install(new)
        {
            let _ = self.ends.retire.push(old);
        }
    }

    /// Applies one command; used for ring commands and directly by offline
    /// render.
    pub fn command(&mut self, cmd: EngineCommand) {
        match cmd {
            EngineCommand::Play => {
                if let Some(c) = self.compiled.as_deref() {
                    self.seq.play(c);
                }
            }
            EngineCommand::Stop => {
                self.release_previews(None);
                if self.seq.playing {
                    let tick = self.seq.stop(&mut self.events);
                    push_engine_event(
                        &mut self.ends.events,
                        &self.shared.status,
                        EngineEvent::Stopped { tick },
                    );
                }
            }
            EngineCommand::Seek { tick } => {
                if let Some(c) = self.compiled.as_deref() {
                    self.seq.seek(c, tick, &mut self.events);
                }
            }
            EngineCommand::SetPlayingPattern { pattern } => {
                if let Some(c) = self.compiled.as_deref() {
                    self.seq.set_pattern(c, pattern, &mut self.events);
                } else {
                    self.seq.set_pattern_id(pattern);
                }
            }
            EngineCommand::Preview {
                channel,
                key,
                vel,
                on,
            } => self.preview_command(channel, key, vel, on),
            EngineCommand::AttachPlugin { slot, handle } => self.plug.attach(slot, handle),
            EngineCommand::DetachPlugin { slot } => {
                self.plug.detach(slot);
                if !self.plug.pending_acks[slot.index()] {
                    self.plug.pending_acks[slot.index()] = true;
                    self.plug.n_pending += 1;
                }
            }
        }
    }

    fn retry_acks(&mut self) {
        if self.plug.n_pending == 0 {
            return;
        }
        for i in 0..PLUGIN_SLOTS {
            if self.plug.pending_acks[i] {
                let slot = slot_of_index(i);
                if self
                    .ends
                    .events
                    .push(EngineEvent::DetachAck { slot })
                    .is_ok()
                {
                    self.plug.pending_acks[i] = false;
                    self.plug.n_pending -= 1;
                }
            }
        }
    }

    /// Renders interleaved stereo. Any length; split into sub-blocks.
    pub fn process_interleaved(&mut self, data: &mut [f32]) {
        let fp = enter_rt_fp_mode();
        let _rt = RtGuard::enter();
        let mut l = [0.0f32; MAX_BLOCK];
        let mut r = [0.0f32; MAX_BLOCK];
        for chunk in data.chunks_mut(MAX_BLOCK * 2) {
            let n = chunk.len() / 2;
            self.sub_block(&mut l[..n], &mut r[..n]);
            for (i, f) in chunk.chunks_exact_mut(2).enumerate() {
                f[0] = l[i];
                f[1] = r[i];
            }
        }
        restore_fp_mode(fp);
    }

    /// Renders planar stereo. Any length; split into sub-blocks.
    pub fn process_planar(&mut self, left: &mut [f32], right: &mut [f32]) {
        let fp = enter_rt_fp_mode();
        let _rt = RtGuard::enter();
        for (l, r) in left.chunks_mut(MAX_BLOCK).zip(right.chunks_mut(MAX_BLOCK)) {
            self.sub_block(l, r);
        }
        restore_fp_mode(fp);
    }

    fn sub_block(&mut self, out_l: &mut [f32], out_r: &mut [f32]) {
        let n = out_l.len();
        debug_assert!(n <= MAX_BLOCK && out_r.len() == n);
        // `events` may already hold note-offs from commands applied directly
        // (offline render, stream start); it is cleared at the end of the block.
        self.beats.clear();

        // Boundary work: commands, swap, acks, plugin events, controls.
        while let Ok(cmd) = self.ends.commands.pop() {
            self.command(cmd);
        }
        self.try_swap();
        self.retry_acks();
        self.plug.pe.clear();
        while let Ok(e) = self.ends.plugin_events.pop() {
            if self.plug.pe.len() < self.plug.pe.capacity() {
                self.plug.pe.push(e);
            }
        }
        // SAFETY: `controls` is an `Arc` owned by `self.shared` for the whole life of
        // `self` and never replaced, so the pointee outlives this call. The
        // reference is only used to read atomics, and going through a pointer
        // lets `self` stay usable for the &mut methods below.
        let ctl: &ControlTable = unsafe { &*Arc::as_ptr(&self.shared.controls) };
        self.seq
            .set_tempo(ctl.tempo().clamp(MIN_TEMPO_BPM, MAX_TEMPO_BPM));

        let Some(c) = self.compiled.take() else {
            for p in &mut self.previews.notes {
                p.active = false;
            }
            out_l.fill(0.0);
            out_r.fill(0.0);
            self.events.clear();
            self.seq.pos += n as u64;
            return;
        };
        let metronome_on = self.metronome_allowed && ctl.get(CTL_METRONOME_ENABLED) >= 0.5;
        self.seq
            .schedule(&c, n, metronome_on, &mut self.events, &mut self.beats);
        // Previews: auto-release, once the sequencer's events are final.
        self.preview_timers(n);
        let pos = self.seq.pos;
        for e in &self.events {
            if self.trace.len() < self.trace.capacity() {
                self.trace.push(TraceEvent {
                    sample: pos + e.offset as u64,
                    slot: e.slot,
                    key: e.key,
                    id: e.id,
                    on: e.on,
                });
            }
        }
        let mut ev_mask = 0u64;
        for e in &self.events {
            ev_mask |= 1u64 << e.slot;
        }

        self.read_mixer(&c);

        // Buses
        self.buses.clear(n);
        self.track_dirty = [false; TRACK_SLOTS];

        // Channels -> track buses
        for s in 0..MAX_CHANNELS {
            let Some(ch) = c.channels[s] else { continue };
            let t = ch.track.0 as usize;
            let cs = ChannelSlot(s as u16);
            let vol = ctl.get(channel_control(cs, MixControl::VolumeDb));
            let pan = ctl.get(channel_control(cs, MixControl::Pan));
            let audible = self.ch_audible[s];
            match ch.instrument {
                InstrumentC::Synth { osc1, osc2 } => {
                    let has_events = ev_mask & (1u64 << s) != 0;
                    if !has_events && !self.synths[s].is_active() {
                        continue;
                    }
                    let sctl = SynthCtl::read(&self.shared.params, cs, self.sample_rate);
                    self.mono[..n].fill(0.0);
                    self.synths[s].render(
                        &sctl,
                        (osc1, osc2),
                        self.sample_rate,
                        s as u16,
                        &self.events,
                        &mut self.mono[..n],
                    );
                    let f = &mut self.chan_fader[s];
                    f.set(vol, pan, audible, true, self.ramp);
                    let (bl, br) = self.buses.lr(t, n);
                    for i in 0..n {
                        let x = self.mono[i];
                        bl[i] += x * f.l.tick();
                        br[i] += x * f.r.tick();
                    }
                    self.track_dirty[t] = true;
                }
                InstrumentC::Clap => {
                    let ps = PluginSlot::Instrument(cs);
                    if !self.plug.attached(ps) {
                        continue;
                    }
                    self.chan_l[..n].fill(0.0);
                    self.chan_r[..n].fill(0.0);
                    let ok = self.plug.call(
                        ps,
                        n,
                        pos,
                        (&self.silence[..n], &self.silence[..n]),
                        (&mut self.chan_l[..n], &mut self.chan_r[..n]),
                        &self.events,
                        Some(s as u16),
                        &self.shared.status,
                        &mut self.ends.events,
                    );
                    if !ok {
                        continue;
                    }
                    let f = &mut self.chan_fader[s];
                    f.set(vol, pan, audible, false, self.ramp);
                    let (bl, br) = self.buses.lr(t, n);
                    for i in 0..n {
                        bl[i] += self.chan_l[i] * f.l.tick();
                        br[i] += self.chan_r[i] * f.r.tick();
                    }
                    self.track_dirty[t] = true;
                }
            }
        }

        // Metronome into the master bus (before master inserts and fader).
        if metronome_on || self.click.active {
            let gain_db = ctl.get(CTL_METRONOME_GAIN_DB);
            let gain = crate::mixer::db_to_lin(gain_db);
            let mut i = 0;
            for b in &self.beats {
                let at = (b.offset as usize).min(n);
                if at > i {
                    self.click.render(&mut self.click_buf[i..at], 1, gain);
                    i = at;
                }
                self.click = Click::start(self.sample_rate, b.accent);
            }
            self.click.render(&mut self.click_buf[i..n], 1, gain);
            let (ml, mr) = self.buses.lr(0, n);
            for i in 0..n {
                ml[i] += self.click_buf[i];
                mr[i] += self.click_buf[i];
            }
            self.track_dirty[0] = true;
        }

        // Tracks 1.. then master: inserts, fader, meter.
        for t in (1..TRACK_SLOTS).chain(std::iter::once(0)) {
            if !c.tracks_present[t] {
                continue;
            }
            let ts = TrackSlot(t as u16);
            let mut has_inserts = false;
            for index in 0..MAX_INSERTS {
                let ps = PluginSlot::Insert {
                    track: ts,
                    index: index as u8,
                };
                if !self.plug.attached(ps) {
                    continue;
                }
                has_inserts = true;
                let (bl, br) = self.buses.lr(t, n);
                let ok = self.plug.call(
                    ps,
                    n,
                    pos,
                    (&*bl, &*br),
                    (&mut self.tmp_l[..n], &mut self.tmp_r[..n]),
                    &self.events,
                    None,
                    &self.shared.status,
                    &mut self.ends.events,
                );
                if ok {
                    bl.copy_from_slice(&self.tmp_l[..n]);
                    br.copy_from_slice(&self.tmp_r[..n]);
                }
            }
            if !self.track_dirty[t] && !has_inserts && t != 0 {
                continue;
            }
            let vol = ctl.get(track_control(ts, MixControl::VolumeDb));
            let pan = ctl.get(track_control(ts, MixControl::Pan));
            let f = &mut self.track_fader[t];
            f.set(vol, pan, self.tr_audible[t], false, self.ramp);
            let (mut pl, mut pr) = (0.0f32, 0.0f32);
            if t == 0 {
                let (bl, br) = self.buses.lr(0, n);
                for i in 0..n {
                    bl[i] *= f.l.tick();
                    br[i] *= f.r.tick();
                    pl = pl.max(bl[i].abs());
                    pr = pr.max(br[i].abs());
                }
            } else {
                let ((tl, tr), (ml, mr)) = self.buses.track_and_master(t, n);
                for i in 0..n {
                    let l = tl[i] * f.l.tick();
                    let r = tr[i] * f.r.tick();
                    pl = pl.max(l.abs());
                    pr = pr.max(r.abs());
                    ml[i] += l;
                    mr[i] += r;
                }
            }
            let st = &self.shared.status;
            st.track_peaks[t * 2].fetch_max(pl.to_bits(), Relaxed);
            st.track_peaks[t * 2 + 1].fetch_max(pr.to_bits(), Relaxed);
        }

        let (ml, mr) = self.buses.lr(0, n);
        out_l.copy_from_slice(ml);
        out_r.copy_from_slice(mr);

        for t in &mut self.preview_tail {
            *t = t.saturating_sub(n as u32);
        }
        self.compiled = Some(c);
        self.events.clear();
        self.seq.pos += n as u64;
        let st = &self.shared.status;
        st.playing.store(self.seq.playing, Relaxed);
        st.playhead_tick.store(self.seq.playhead_tick(), Relaxed);
    }

    /// Reads mute and solo for every present channel and track and resolves
    /// solo-in-place into `ch_audible` / `tr_audible`.
    fn read_mixer(&mut self, c: &Compiled) {
        let ctl = &self.shared.controls;
        for s in 0..MAX_CHANNELS {
            match c.channels[s] {
                Some(ch) => {
                    let cs = ChannelSlot(s as u16);
                    self.ch_ms[s] = MuteSolo {
                        present: true,
                        mute: ctl.get(channel_control(cs, MixControl::Mute)) >= 0.5,
                        solo: ctl.get(channel_control(cs, MixControl::Solo)) >= 0.5,
                    };
                    self.ch_track[s] = ch.track.0;
                }
                None => {
                    self.ch_ms[s] = MuteSolo::default();
                    self.ch_track[s] = 0;
                }
            }
        }
        for t in 0..TRACK_SLOTS {
            let ts = TrackSlot(t as u16);
            self.tr_ms[t] = MuteSolo {
                present: c.tracks_present[t],
                mute: ctl.get(track_control(ts, MixControl::Mute)) >= 0.5,
                solo: ctl.get(track_control(ts, MixControl::Solo)) >= 0.5,
            };
        }
        resolve_solo(
            &self.ch_ms,
            &self.ch_track,
            &self.tr_ms,
            &mut self.ch_audible,
            &mut self.tr_audible,
        );
        // A previewed channel is exempt from solo elsewhere (mute still wins).
        let active = self.previews.active_slots();
        for s in 0..MAX_CHANNELS {
            if self.preview_tail[s] == 0 && active & (1u64 << s) == 0 {
                continue;
            }
            let ms = self.ch_ms[s];
            if !ms.present || ms.mute {
                continue;
            }
            self.ch_audible[s] = true;
            let t = self.ch_track[s] as usize;
            if self.tr_ms[t].present && !self.tr_ms[t].mute {
                self.tr_audible[t] = true;
            }
        }
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        // The stream is gone and this runs on the thread that dropped it
        // (the GTK thread, SPEC 4.7): stop processing of every started plugin.
        for e in &mut self.plug.entries {
            if let Some(h) = e.handle.take()
                && e.started
            {
                // SAFETY: the audio thread no longer exists; the GTK thread
                // has not yet deactivated or destroyed the instance.
                unsafe { (self.plug.api.stop)(h) };
            }
        }
    }
}

fn slot_of_index(i: usize) -> PluginSlot {
    use protocol::consts::MAX_CHANNELS as MC;
    if i < MC {
        PluginSlot::Instrument(ChannelSlot(i as u16))
    } else {
        let k = i - MC;
        PluginSlot::Insert {
            track: TrackSlot((k / MAX_INSERTS) as u16),
            index: (k % MAX_INSERTS) as u8,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slot_index_round_trips() {
        for i in 0..PLUGIN_SLOTS {
            assert_eq!(slot_of_index(i).index(), i);
        }
    }
}
