// SPDX-License-Identifier: GPL-3.0-or-later
//! Pattern playback on the anchor transport (SPEC 4.5, 4.6, 8, 17.1).
//!
//! Note-ons come from per-channel sorted arrays through a cursor. Note-offs
//! come from the active-note table, which is independent of pattern data and
//! is keyed by (channel slot, key) with the owning note id, so an overlapping
//! same-key note is never cut by an older note's off, and deleting a sounding
//! note never leaves it hanging.

use crate::compiled::{Compiled, PatternC};
use crate::transport::{PPQ, Transport, samples_per_tick};
use protocol::consts::MAX_CHANNELS;
use protocol::ids::PatternId;

/// A note event inside a sub-block. `offset` is in frames from its start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SeqEvent {
    pub offset: u32,
    pub slot: u16,
    pub key: u8,
    pub vel: u8,
    pub on: bool,
    pub id: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Beat {
    pub offset: u32,
    pub accent: bool,
}

/// Capacities of the per-sub-block scratch lists (fixed at start).
pub const EVENT_CAP: usize = 32768;
/// Note-ons per sub-block; the rest wait for the next sub-block.
const ON_LIMIT: usize = 8192;
pub const BEAT_CAP: usize = 64;
const MAX_ITERS: usize = 4096;

pub fn push_event(v: &mut Vec<SeqEvent>, e: SeqEvent) {
    if v.len() < v.capacity() {
        v.push(e);
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Off,
    Wrap,
    On,
    Beat,
}

pub struct Sequencer {
    sample_rate: f64,
    tempo_bits: u64,
    spt: f64,
    transport: Transport,
    pub playing: bool,
    /// Pattern tick where the next `play` starts.
    start_tick: i64,
    pattern: Option<PatternId>,
    cursors: [u32; MAX_CHANNELS],
    owner: Box<[u32]>,
    end_tick: Box<[i64]>,
    live: [u128; MAX_CHANNELS],
    live_slots: u64,
    next_beat: i64,
    /// Absolute sample of the next sub-block start.
    pub pos: u64,
    pub loops: u64,
}

impl Sequencer {
    pub fn new(sample_rate: f64, bpm: f64) -> Sequencer {
        let spt = samples_per_tick(sample_rate, bpm);
        Sequencer {
            sample_rate,
            tempo_bits: bpm.to_bits(),
            spt,
            transport: Transport::at(0, 0, spt),
            playing: false,
            start_tick: 0,
            pattern: None,
            cursors: [0; MAX_CHANNELS],
            owner: vec![0; MAX_CHANNELS * 128].into_boxed_slice(),
            end_tick: vec![0; MAX_CHANNELS * 128].into_boxed_slice(),
            live: [0; MAX_CHANNELS],
            live_slots: 0,
            next_beat: 0,
            pos: 0,
            loops: 0,
        }
    }

    /// Tempo is read once per sub-block; a change re-anchors at `pos`.
    pub fn set_tempo(&mut self, bpm: f64) {
        let bits = bpm.to_bits();
        if bits != self.tempo_bits {
            self.tempo_bits = bits;
            self.spt = samples_per_tick(self.sample_rate, bpm);
            self.transport.set_tempo(self.pos, self.spt);
        }
    }

    pub fn set_pattern_id(&mut self, id: PatternId) {
        self.pattern = Some(id);
    }

    pub fn pattern_id(&self) -> Option<PatternId> {
        self.pattern
    }

    fn pattern<'a>(&self, c: &'a Compiled) -> Option<&'a PatternC> {
        match self.pattern {
            Some(id) => c.pattern(id),
            None => c.patterns.first(),
        }
    }

    fn loop_len(&self, c: &Compiled) -> Option<i64> {
        self.pattern(c).map(|p| p.len_ticks as i64)
    }

    /// Tick of the playhead within the pattern pass (floored, never negative).
    pub fn playhead_tick(&self) -> u64 {
        if self.playing {
            self.transport.tick_at(self.pos).max(0.0) as u64
        } else {
            self.start_tick.max(0) as u64
        }
    }

    pub fn live_notes(&self, slot: u16) -> u128 {
        self.live[slot as usize]
    }

    /// Positions cursors for the current anchor: the first note whose
    /// sample is not before `pos`.
    pub fn sync_cursors(&mut self, c: &Compiled) {
        self.cursors = [0; MAX_CHANNELS];
        let Some(p) = self.pattern(c) else { return };
        for &s in &p.active_slots {
            let notes = &p.notes[s as usize];
            let t = &self.transport;
            let pos = self.pos;
            self.cursors[s as usize] =
                notes.partition_point(|n| t.sample_of_tick(n.start as i64) < pos) as u32;
        }
    }

    fn sync_beat(&mut self, tick: i64) {
        let ppq = PPQ as i64;
        self.next_beat = (tick + ppq - 1).div_euclid(ppq) * ppq;
    }

    pub fn play(&mut self, c: &Compiled) {
        if self.playing {
            return;
        }
        self.playing = true;
        self.transport = Transport::at(self.pos, self.start_tick, self.spt);
        self.sync_cursors(c);
        self.sync_beat(self.start_tick);
    }

    /// Returns the tick the playhead was at.
    pub fn stop(&mut self, out: &mut Vec<SeqEvent>) -> u64 {
        let tick = self.playhead_tick();
        self.release_all(0, out);
        self.playing = false;
        self.start_tick = 0;
        tick
    }

    pub fn seek(&mut self, c: &Compiled, tick: u64, out: &mut Vec<SeqEvent>) {
        let len = self.loop_len(c);
        let mut t = tick.min(i64::MAX as u64) as i64;
        if let Some(l) = len {
            t = t.rem_euclid(l);
        }
        if self.playing {
            self.release_all(0, out);
            self.transport = Transport::at(self.pos, t, self.spt);
            self.sync_cursors(c);
            self.sync_beat(t);
        } else {
            self.start_tick = t;
        }
    }

    pub fn set_pattern(&mut self, c: &Compiled, id: PatternId, out: &mut Vec<SeqEvent>) {
        if self.pattern == Some(id) {
            return;
        }
        self.pattern = Some(id);
        if self.playing {
            self.release_all(0, out);
            let len = self.loop_len(c);
            let mut t = self.transport.tick_at(self.pos).max(0.0) as i64;
            if let Some(l) = len {
                t = t.rem_euclid(l);
            }
            self.transport = Transport::at(self.pos, t, self.spt);
            self.sync_cursors(c);
            self.sync_beat(t);
        } else {
            self.start_tick = 0;
        }
    }

    /// A new `Compiled` was installed at a sub-block boundary.
    pub fn on_install(&mut self, c: &Compiled) {
        if self.playing {
            self.sync_cursors(c);
        }
    }

    /// Note-offs for every live note of `slot` (slot reset, SPEC 4.1).
    pub fn release_slot(&mut self, slot: u16, offset: u32, out: &mut Vec<SeqEvent>) {
        let mut bits = self.live[slot as usize];
        while bits != 0 {
            let key = bits.trailing_zeros() as u8;
            bits &= bits - 1;
            let id = self.owner[slot as usize * 128 + key as usize];
            push_event(
                out,
                SeqEvent {
                    offset,
                    slot,
                    key,
                    vel: 0,
                    on: false,
                    id,
                },
            );
        }
        self.live[slot as usize] = 0;
        self.live_slots &= !(1u64 << slot);
    }

    pub fn release_all(&mut self, offset: u32, out: &mut Vec<SeqEvent>) {
        let mut slots = self.live_slots;
        while slots != 0 {
            let s = slots.trailing_zeros() as u16;
            slots &= slots - 1;
            self.release_slot(s, offset, out);
        }
    }

    fn min_off(&self) -> Option<(i64, u16, u8)> {
        let mut best: Option<(i64, u16, u8)> = None;
        let mut slots = self.live_slots;
        while slots != 0 {
            let s = slots.trailing_zeros() as u16;
            slots &= slots - 1;
            let mut bits = self.live[s as usize];
            while bits != 0 {
                let key = bits.trailing_zeros() as u8;
                bits &= bits - 1;
                let t = self.end_tick[s as usize * 128 + key as usize];
                if best.is_none_or(|b| t < b.0) {
                    best = Some((t, s, key));
                }
            }
        }
        best
    }

    fn min_on(&self, p: &PatternC) -> Option<(i64, u16)> {
        let mut best: Option<(i64, u16)> = None;
        for &s in &p.active_slots {
            let notes = &p.notes[s as usize];
            if let Some(n) = notes.get(self.cursors[s as usize] as usize) {
                let t = n.start as i64;
                if best.is_none_or(|b| t < b.0) {
                    best = Some((t, s));
                }
            }
        }
        best
    }

    /// Emits this sub-block's events (`n` frames starting at `self.pos`) in
    /// time order and advances nothing but the musical state; the caller
    /// moves `pos`. `time_sig_num` sets the accent.
    pub fn schedule(
        &mut self,
        c: &Compiled,
        n: usize,
        metronome: bool,
        events: &mut Vec<SeqEvent>,
        beats: &mut Vec<Beat>,
    ) {
        if !self.playing {
            return;
        }
        let start = self.pos;
        let end = start + n as u64;
        let pat = self.pattern(c);
        let loop_len = pat.map(|p| p.len_ticks as i64);
        let beats_per_bar = c.time_sig_num.max(1) as i64;
        let mut ons = 0usize;

        for _ in 0..MAX_ITERS {
            let mut best: Option<(i64, Kind)> = None;
            let mut consider = |t: i64, k: Kind| {
                if best.is_none_or(|b| t < b.0) {
                    best = Some((t, k));
                }
            };
            let off = self.min_off();
            if let Some((t, _, _)) = off {
                consider(t, Kind::Off);
            }
            if let Some(l) = loop_len {
                consider(l, Kind::Wrap);
            }
            let on = pat.and_then(|p| self.min_on(p));
            if let Some((t, _)) = on {
                consider(t, Kind::On);
            }
            if loop_len.is_none_or(|l| self.next_beat < l) {
                consider(self.next_beat, Kind::Beat);
            }
            let Some((tick, kind)) = best else { break };
            let s = self.transport.sample_of_tick(tick).max(start);
            if s >= end {
                break;
            }
            let offset = (s - start) as u32;
            match kind {
                Kind::Off => {
                    let (_, slot, key) = off.expect("off candidate");
                    let idx = slot as usize * 128 + key as usize;
                    let id = self.owner[idx];
                    self.live[slot as usize] &= !(1u128 << key);
                    if self.live[slot as usize] == 0 {
                        self.live_slots &= !(1u64 << slot);
                    }
                    self.emit(events, offset, slot, key, 0, false, id);
                }
                Kind::Wrap => {
                    let mut slots = self.live_slots;
                    while slots != 0 {
                        let sl = slots.trailing_zeros() as u16;
                        slots &= slots - 1;
                        let mut bits = self.live[sl as usize];
                        while bits != 0 {
                            let key = bits.trailing_zeros() as u8;
                            bits &= bits - 1;
                            let id = self.owner[sl as usize * 128 + key as usize];
                            self.emit(events, offset, sl, key, 0, false, id);
                        }
                    }
                    self.live = [0; MAX_CHANNELS];
                    self.live_slots = 0;
                    self.transport.wrap(loop_len.expect("wrap needs a loop"));
                    self.cursors = [0; MAX_CHANNELS];
                    self.next_beat = 0;
                    self.loops += 1;
                }
                Kind::On => {
                    if ons >= ON_LIMIT {
                        break;
                    }
                    ons += 1;
                    let (_, slot) = on.expect("on candidate");
                    let p = pat.expect("pattern");
                    let note = p.notes[slot as usize][self.cursors[slot as usize] as usize];
                    self.cursors[slot as usize] += 1;
                    let idx = slot as usize * 128 + note.key as usize;
                    if self.live[slot as usize] & (1u128 << note.key) != 0 {
                        // Retrigger: the old note ends first.
                        let old = self.owner[idx];
                        self.emit(events, offset, slot, note.key, 0, false, old);
                    }
                    self.owner[idx] = note.id;
                    self.end_tick[idx] = match loop_len {
                        Some(l) => (note.end as i64).min(l),
                        None => note.end as i64,
                    };
                    self.live[slot as usize] |= 1u128 << note.key;
                    self.live_slots |= 1u64 << slot;
                    self.emit(events, offset, slot, note.key, note.vel, true, note.id);
                }
                Kind::Beat => {
                    if metronome && beats.len() < BEAT_CAP {
                        beats.push(Beat {
                            offset,
                            accent: (self.next_beat / PPQ as i64) % beats_per_bar == 0,
                        });
                    }
                    self.next_beat += PPQ as i64;
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn emit(
        &self,
        events: &mut Vec<SeqEvent>,
        offset: u32,
        slot: u16,
        key: u8,
        vel: u8,
        on: bool,
        id: u32,
    ) {
        push_event(
            events,
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
}

/// Test hook: with a capacity set by `Runtime::enable_trace`, every note
/// event is recorded with its absolute sample. With capacity 0 nothing is
/// recorded and nothing allocates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TraceEvent {
    pub sample: u64,
    pub slot: u16,
    pub key: u8,
    pub id: u32,
    pub on: bool,
}
