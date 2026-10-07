// SPDX-License-Identifier: GPL-3.0-or-later
//! Layouts shared between the GTK thread and the audio thread (4.2 to 4.4,
//! 9.1, 17.1): slot indexing, the control and parameter tables, the engine
//! status atomics, and the fixed-size records sent through SPSC rings.
//!
//! Everything here is either an atomic or a `Copy` record of fixed size,
//! so the audio thread can use it without allocating or locking.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use crate::consts::{MAX_CHANNELS, MAX_INSERTS, MAX_PARAMS_PER_SLOT, TRACK_SLOTS};
use crate::ids::PatternId;

/// Stable per-channel slot index, `0..MAX_CHANNELS`, assigned by the GTK
/// thread for the channel's lifetime (4.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ChannelSlot(pub u16);

/// Stable per-track slot index, `0..TRACK_SLOTS`. The master is slot 0.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TrackSlot(pub u16);

impl TrackSlot {
    pub const MASTER: TrackSlot = TrackSlot(0);
}

/// Where a plugin instance sits in the engine's plugin table.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PluginSlot {
    Instrument(ChannelSlot),
    Insert { track: TrackSlot, index: u8 },
}

/// Number of plugin table entries: one instrument per channel plus every
/// insert slot of every track.
pub const PLUGIN_SLOTS: usize = MAX_CHANNELS + TRACK_SLOTS * MAX_INSERTS;

impl PluginSlot {
    /// Index into the engine's plugin table, `0..PLUGIN_SLOTS`.
    pub fn index(self) -> usize {
        match self {
            PluginSlot::Instrument(c) => c.0 as usize,
            PluginSlot::Insert { track, index } => {
                MAX_CHANNELS + track.0 as usize * MAX_INSERTS + index as usize
            }
        }
    }
}

/// Generation counter per slot (4.1). When the audio thread installs a
/// `Compiled` whose generation for a slot differs from `Runtime`'s, it
/// resets that slot's runtime state.
pub type SlotGen = u32;

// ---------------------------------------------------------------------------
// Control table (4.3)

/// The four fader controls of a channel or track.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum MixControl {
    VolumeDb = 0,
    Pan = 1,
    /// 0.0 or 1.0.
    Mute = 2,
    /// 0.0 or 1.0.
    Solo = 3,
}

const MIX_CONTROLS: usize = 4;
const CHANNEL_BASE: usize = 0;
const TRACK_BASE: usize = CHANNEL_BASE + MAX_CHANNELS * MIX_CONTROLS;
/// Metronome gain in dB.
pub const CTL_METRONOME_GAIN_DB: usize = TRACK_BASE + TRACK_SLOTS * MIX_CONTROLS;
/// Metronome on (1.0) or off (0.0).
pub const CTL_METRONOME_ENABLED: usize = CTL_METRONOME_GAIN_DB + 1;
/// Number of `f32` entries in the control table.
pub const CONTROL_TABLE_LEN: usize = CTL_METRONOME_ENABLED + 1;

pub fn channel_control(slot: ChannelSlot, c: MixControl) -> usize {
    CHANNEL_BASE + slot.0 as usize * MIX_CONTROLS + c as usize
}

pub fn track_control(slot: TrackSlot, c: MixControl) -> usize {
    TRACK_BASE + slot.0 as usize * MIX_CONTROLS + c as usize
}

/// Continuous values written by the GTK thread (only writer) and read by the
/// audio thread once per sub-block. After any document replacement the GTK
/// thread rewrites every entry from the new document (4.3).
pub struct ControlTable {
    values: Box<[AtomicU32]>,
    /// Tempo in BPM as `f64` bits (f32 is not precise enough for the
    /// exact-grid tests of section 14).
    tempo_bpm: AtomicU64,
}

impl ControlTable {
    /// Allocates the table. Call off the audio thread.
    pub fn new() -> ControlTable {
        ControlTable {
            values: (0..CONTROL_TABLE_LEN).map(|_| AtomicU32::new(0)).collect(),
            tempo_bpm: AtomicU64::new(120f64.to_bits()),
        }
    }

    pub fn set(&self, index: usize, v: f32) {
        self.values[index].store(v.to_bits(), Ordering::Relaxed);
    }

    pub fn get(&self, index: usize) -> f32 {
        f32::from_bits(self.values[index].load(Ordering::Relaxed))
    }

    pub fn set_tempo(&self, bpm: f64) {
        self.tempo_bpm.store(bpm.to_bits(), Ordering::Relaxed);
    }

    pub fn tempo(&self) -> f64 {
        f64::from_bits(self.tempo_bpm.load(Ordering::Relaxed))
    }
}

impl Default for ControlTable {
    fn default() -> ControlTable {
        ControlTable::new()
    }
}

// ---------------------------------------------------------------------------
// Parameter table (17.1)

/// Index of a native instrument parameter: `param` is, for the built-in
/// synth, `SynthParam::index()`.
pub fn param_index(slot: ChannelSlot, param: usize) -> usize {
    debug_assert!(param < MAX_PARAMS_PER_SLOT);
    slot.0 as usize * MAX_PARAMS_PER_SLOT + param
}

pub const PARAM_TABLE_LEN: usize = MAX_CHANNELS * MAX_PARAMS_PER_SLOT;

/// Native instrument parameters, same rules as `ControlTable`.
pub struct ParamTable {
    values: Box<[AtomicU32]>,
}

impl ParamTable {
    pub fn new() -> ParamTable {
        ParamTable {
            values: (0..PARAM_TABLE_LEN).map(|_| AtomicU32::new(0)).collect(),
        }
    }

    pub fn set(&self, index: usize, v: f32) {
        self.values[index].store(v.to_bits(), Ordering::Relaxed);
    }

    pub fn get(&self, index: usize) -> f32 {
        f32::from_bits(self.values[index].load(Ordering::Relaxed))
    }
}

impl Default for ParamTable {
    fn default() -> ParamTable {
        ParamTable::new()
    }
}

// ---------------------------------------------------------------------------
// Engine status (4.4): written by the audio thread, read by the UI.

pub struct EngineStatus {
    pub playing: AtomicBool,
    /// Playhead in ticks.
    pub playhead_tick: AtomicU64,
    /// Backend-reported xruns plus callback gaps over 1.5 periods.
    pub xruns: AtomicU64,
    /// Events dropped because the `EventRing` was full (4.4).
    pub event_overflows: AtomicU64,
    /// Peak since last read, `f32` bits, left and right per track slot.
    /// The UI swaps in 0 when it reads.
    pub track_peaks: Box<[AtomicU32]>,
    /// Longest `process()` call per plugin slot in microseconds (3.3).
    pub plugin_max_us: Box<[AtomicU32]>,
    /// Last `process()` call per plugin slot in microseconds.
    pub plugin_last_us: Box<[AtomicU32]>,
}

impl EngineStatus {
    pub fn new() -> EngineStatus {
        let zeros = |n: usize| {
            (0..n)
                .map(|_| AtomicU32::new(0))
                .collect::<Box<[AtomicU32]>>()
        };
        EngineStatus {
            playing: AtomicBool::new(false),
            playhead_tick: AtomicU64::new(0),
            xruns: AtomicU64::new(0),
            event_overflows: AtomicU64::new(0),
            track_peaks: zeros(TRACK_SLOTS * 2),
            plugin_max_us: zeros(PLUGIN_SLOTS),
            plugin_last_us: zeros(PLUGIN_SLOTS),
        }
    }
}

impl Default for EngineStatus {
    fn default() -> EngineStatus {
        EngineStatus::new()
    }
}

// ---------------------------------------------------------------------------
// Ring records

/// Opaque pointer to a live plugin instance's audio-side data. Created and
/// owned by `plugin-host`; the engine only passes it back to `plugin-host`
/// functions while the slot is attached (9.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PluginHandle(pub *mut core::ffi::c_void);

// SAFETY: the handle is moved to the audio thread through the command ring
// and used there only; the GTK thread does not touch it again until the
// detach ack (9.1).
unsafe impl Send for PluginHandle {}

/// GTK thread to audio thread, through the command ring.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum EngineCommand {
    Play,
    Stop,
    /// Move the playhead.
    Seek {
        tick: u64,
    },
    /// The pattern looped in pattern mode.
    SetPlayingPattern {
        pattern: PatternId,
    },
    /// Fill a plugin slot. `start_processing` happens on first use.
    AttachPlugin {
        slot: PluginSlot,
        handle: PluginHandle,
    },
    /// The audio thread calls `stop_processing`, clears the slot, and
    /// replies `EngineEvent::DetachAck`.
    DetachPlugin {
        slot: PluginSlot,
    },
    /// Audition: play `key` on a channel's instrument now, whether or not
    /// the transport runs, through the channel's mixer path (owner request;
    /// the full preview slot of 17.2 comes in Milestone B). `on: false`
    /// releases it. Preview notes use their own note ids and never touch
    /// the document. The engine releases a preview note by itself after
    /// `PREVIEW_MAX_SECONDS` if no `on: false` arrives.
    Preview {
        channel: ChannelSlot,
        key: u8,
        vel: u8,
        on: bool,
    },
}

/// Longest a preview note sounds without a release (seconds).
pub const PREVIEW_MAX_SECONDS: f64 = 4.0;

/// Host-to-plugin events, through the plugin event ring (4.3, 9.1).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PluginEvent {
    pub slot: PluginSlot,
    pub param_id: u32,
    pub value: f64,
}

/// Audio thread to GTK thread, through the event ring (4.4).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum EngineEvent {
    DetachAck {
        slot: PluginSlot,
    },
    PluginParamChanged {
        slot: PluginSlot,
        param_id: u32,
        value: f64,
    },
    PluginGestureBegin {
        slot: PluginSlot,
        param_id: u32,
    },
    PluginGestureEnd {
        slot: PluginSlot,
        param_id: u32,
    },
    /// The transport stopped (by command or end of export).
    Stopped {
        tick: u64,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_indices_are_unique_and_in_bounds() {
        let mut seen = std::collections::HashSet::new();
        let controls = [
            MixControl::VolumeDb,
            MixControl::Pan,
            MixControl::Mute,
            MixControl::Solo,
        ];
        for s in 0..MAX_CHANNELS {
            for c in controls {
                assert!(seen.insert(channel_control(ChannelSlot(s as u16), c)));
            }
        }
        for s in 0..TRACK_SLOTS {
            for c in controls {
                assert!(seen.insert(track_control(TrackSlot(s as u16), c)));
            }
        }
        assert!(seen.insert(CTL_METRONOME_GAIN_DB));
        assert!(seen.insert(CTL_METRONOME_ENABLED));
        assert_eq!(seen.len(), CONTROL_TABLE_LEN);
        assert!(seen.iter().all(|&i| i < CONTROL_TABLE_LEN));
    }

    #[test]
    fn plugin_slot_indices_are_unique_and_in_bounds() {
        let mut seen = std::collections::HashSet::new();
        for c in 0..MAX_CHANNELS {
            assert!(seen.insert(PluginSlot::Instrument(ChannelSlot(c as u16)).index()));
        }
        for t in 0..TRACK_SLOTS {
            for i in 0..MAX_INSERTS {
                let s = PluginSlot::Insert {
                    track: TrackSlot(t as u16),
                    index: i as u8,
                };
                assert!(seen.insert(s.index()));
            }
        }
        assert_eq!(seen.len(), PLUGIN_SLOTS);
        assert!(seen.iter().all(|&i| i < PLUGIN_SLOTS));
    }

    #[test]
    fn tables_round_trip_values() {
        let t = ControlTable::new();
        t.set(CTL_METRONOME_GAIN_DB, -6.5);
        assert_eq!(t.get(CTL_METRONOME_GAIN_DB), -6.5);
        t.set_tempo(133.33);
        assert_eq!(t.tempo(), 133.33);
        let p = ParamTable::new();
        let i = param_index(ChannelSlot(63), MAX_PARAMS_PER_SLOT - 1);
        assert_eq!(i, PARAM_TABLE_LEN - 1);
        p.set(i, 0.25);
        assert_eq!(p.get(i), 0.25);
    }
}
