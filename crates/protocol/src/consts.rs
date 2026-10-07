// SPDX-License-Identifier: GPL-3.0-or-later
//! Fixed limits and constants (SPEC 4.1, 4.5, 4.6, 7, 17.1).

/// Ticks per quarter note (4.6).
pub const PPQ: u32 = 960;
/// Default step length: one sixteenth note (5.1).
pub const DEFAULT_STEP_TICKS: u32 = PPQ / 4;
/// Largest tick position or note end stored in a document (4.6).
pub const MAX_TICK: u32 = 1 << 31;

/// Largest sub-block the engine processes at once (4.5).
pub const MAX_BLOCK: usize = 256;

/// Channels per project (4.1).
pub const MAX_CHANNELS: usize = 64;
/// Mixer tracks per project, not counting the master (4.1).
pub const MAX_TRACKS: usize = 32;
/// Track slots including the master, which is always slot 0.
pub const TRACK_SLOTS: usize = MAX_TRACKS + 1;
/// Insert slots per mixer track (4.1).
pub const MAX_INSERTS: usize = 8;
/// Voices per built-in synth channel (8).
pub const SYNTH_VOICES: usize = 16;
/// Native instrument parameters per channel slot in the `ParamTable` (17.1).
pub const MAX_PARAMS_PER_SLOT: usize = 64;

/// Steps per pattern (5.1).
pub const MIN_STEPS: u8 = 1;
pub const MAX_STEPS: u8 = 64;
/// Notes per pattern and per project (17.1).
pub const MAX_NOTES_PER_PATTERN: usize = 100_000;
pub const MAX_NOTES_PER_PROJECT: usize = 500_000;
/// Patterns per project. Not in the spec text; chosen so a project file
/// stays loadable. Changing it is an interface change.
pub const MAX_PATTERNS: usize = 999;

/// Tempo range in BPM (8).
pub const MIN_TEMPO_BPM: f64 = 20.0;
pub const MAX_TEMPO_BPM: f64 = 999.0;
/// Time signature numerator range; the denominator is always 4 (5.1).
pub const MIN_TIME_SIG_NUM: u8 = 1;
pub const MAX_TIME_SIG_NUM: u8 = 16;

/// Gain range in dB for faders and the metronome. `MIN_GAIN_DB` means silence.
pub const MIN_GAIN_DB: f64 = -96.0;
pub const MAX_GAIN_DB: f64 = 12.0;

/// Longest entity or plugin name, in characters.
pub const MAX_NAME_CHARS: usize = 128;

/// Ring capacities (4.2, 4.4, 9.1). `RETIRE_RING_CAP >= STATE_RING_CAP + 1`.
pub const STATE_RING_CAP: usize = 2;
pub const RETIRE_RING_CAP: usize = STATE_RING_CAP + 2;
pub const COMMAND_RING_CAP: usize = 256;
pub const EVENT_RING_CAP: usize = 1024;
pub const PLUGIN_EVENT_RING_CAP: usize = 1024;

/// Control API limits (17.1).
pub const MAX_REQUEST_LINE_BYTES: usize = 1 << 20;
pub const MAX_EDITS_PER_REQUEST: usize = 10_000;
/// Longest untrusted string returned to an agent (17.1).
pub const MAX_AGENT_STRING_CHARS: usize = 64;

/// Project file format version (7.3). Bump on any schema change.
pub const FORMAT_VERSION: u32 = 2;

const _: () = assert!(RETIRE_RING_CAP > STATE_RING_CAP);

// Milestone B limits (15.1 to 15.6, 17.2).

/// Continuous parameters per built-in effect insert in the `FxParamTable`.
pub const FX_PARAMS_PER_INSERT: usize = 16;
/// Sends per mixer track.
pub const MAX_SENDS: usize = 4;
/// Choke groups are 1 to `MAX_CHOKE_GROUP`; 0 means none.
pub const MAX_CHOKE_GROUP: u8 = 16;
/// Swing in 1/1000 of a step; 750 delays every second step by 3/4 step.
pub const MAX_SWING: u16 = 750;
/// Step pitch lane range in semitones.
pub const MAX_STEP_OFFSET: i8 = 24;
/// Allowed ratchet counts (notes per step).
pub const RATCHETS: [u8; 6] = [1, 2, 3, 4, 6, 8];
/// Samples registered in a project.
pub const MAX_SAMPLES: usize = 4096;
/// Playlist tracks and clips.
pub const MAX_PLAYLIST_TRACKS: usize = 128;
pub const MAX_CLIPS: usize = 20_000;
