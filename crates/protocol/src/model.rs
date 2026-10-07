// SPDX-License-Identifier: GPL-3.0-or-later
//! The document schema for Milestone A (SPEC 5.1, 17.1).
//!
//! `Project` is an `Arc` tree so undo snapshots share unchanged parts
//! (section 6). Edits clone only the touched path with `Arc::make_mut`.
//! Collections are kept sorted by id; the emitter in `format` sorts again
//! so the file is canonical even if a caller forgets.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::beats::{Bass808, BuiltinFx, Sampler};
use crate::consts::{DEFAULT_STEP_TICKS, PPQ};
use crate::ids::{ChannelId, ClipId, GroupId, InstanceId, NoteId, PatternId, ShapeId, TrackId};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub tempo_bpm: f64,
    /// Beats per bar. The denominator is always 4.
    pub time_sig_num: u8,
    pub metronome: Metronome,
    pub channels: Vec<Arc<Channel>>,
    pub patterns: Vec<Arc<Pattern>>,
    /// Mixer tracks. `tracks[0]` is the master (`TrackId::MASTER`).
    pub tracks: Vec<Arc<Track>>,
    /// Samples used by the project, sorted by hash (17.2).
    #[serde(default)]
    pub samples: Vec<SampleRef>,
    /// Clips on the timeline (20.2), sorted by `(instrument, start, id)`,
    /// never overlapping on one instrument's row.
    #[serde(default)]
    pub clips: Vec<Clip>,
    /// Loop region of the timeline (20.2).
    #[serde(default)]
    pub loop_region: LoopRegion,
    /// Patterns (20.7).
    #[serde(default)]
    pub groups: Vec<PatternGroup>,
    /// Automation curves (24.2-1), sorted by id.
    #[serde(default)]
    pub shapes: Vec<Shape>,
}

impl Project {
    /// A project with only the master track: 120 BPM, 4/4, metronome off.
    pub fn empty() -> Project {
        Project {
            tempo_bpm: 120.0,
            time_sig_num: 4,
            metronome: Metronome::default(),
            channels: Vec::new(),
            patterns: Vec::new(),
            tracks: vec![Arc::new(Track {
                id: TrackId::MASTER,
                name: "Master".to_string(),
                mix: Mix::default(),
                inserts: Vec::new(),
                sends: Vec::new(),
            })],
            samples: Vec::new(),
            clips: Vec::new(),
            loop_region: LoopRegion::default(),
            groups: Vec::new(),
            shapes: Vec::new(),
        }
    }

    pub fn channel(&self, id: ChannelId) -> Option<&Arc<Channel>> {
        self.channels.iter().find(|c| c.id == id)
    }

    pub fn pattern(&self, id: PatternId) -> Option<&Arc<Pattern>> {
        self.patterns.iter().find(|p| p.id == id)
    }

    pub fn track(&self, id: TrackId) -> Option<&Arc<Track>> {
        self.tracks.iter().find(|t| t.id == id)
    }

    /// Largest id used anywhere in the project (0 if only the master exists).
    pub fn max_id(&self) -> u32 {
        let mut m = 0;
        for c in &self.channels {
            m = m.max(c.id.0);
            if let Instrument::Clap(r) = &c.instrument {
                m = m.max(r.instance.0);
            }
        }
        for p in &self.patterns {
            m = m.max(p.id.0);
            for n in &p.notes {
                m = m.max(n.id.0);
            }
        }
        for t in &self.tracks {
            m = m.max(t.id.0);
            for i in &t.inserts {
                m = m.max(i.instance().0);
            }
        }
        for c in &self.clips {
            m = m.max(c.id.0);
        }
        for g in &self.groups {
            m = m.max(g.id.0);
        }
        for s in &self.shapes {
            m = m.max(s.id.0);
        }
        m
    }

    /// Total number of notes in all patterns.
    pub fn note_count(&self) -> usize {
        self.patterns.iter().map(|p| p.note_count()).sum()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Metronome {
    pub enabled: bool,
    pub gain_db: f64,
}

impl Default for Metronome {
    fn default() -> Metronome {
        Metronome {
            enabled: false,
            gain_db: -6.0,
        }
    }
}

/// Fader values shared by channels and tracks. These are control values
/// (4.3): the engine reads them from the `ControlTable`, not from `Compiled`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mix {
    pub volume_db: f64,
    /// -1.0 (left) to 1.0 (right).
    pub pan: f64,
    pub mute: bool,
    pub solo: bool,
}

impl Default for Mix {
    fn default() -> Mix {
        Mix {
            volume_db: 0.0,
            pan: 0.0,
            mute: false,
            solo: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Channel {
    pub id: ChannelId,
    pub name: String,
    /// Key a step plays (5.2).
    pub root_key: u8,
    /// Mixer track this channel feeds.
    pub track: TrackId,
    pub mix: Mix,
    pub instrument: Instrument,
    /// Choke group 1 to 16; 0 = none (15.1). A note on a channel stops the
    /// voices of the other channels in the same group.
    #[serde(default)]
    pub choke_group: u8,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Instrument {
    Synth(SynthParams),
    Clap(ClapRef),
    Sampler(Sampler),
    Bass808(Bass808),
    /// An audio row: plays the audio clips on it (21.1).
    Audio,
}

/// A CLAP plugin instance as stored in the document (5.1, 7.5, 17.1).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClapRef {
    pub instance: InstanceId,
    /// CLAP plugin id, for example `org.surge-synth-team.surge-xt`.
    pub plugin_id: String,
    /// Plugin version seen when the state was captured.
    pub plugin_version: String,
    /// File name of the last captured state blob in `plugin-state/`
    /// (`<instance>-<generation>.bin`, 7.1). `None` until first capture.
    #[serde(default)]
    pub state_file: Option<String>,
    /// The blob bytes, shared between undo snapshots. Not written to the
    /// project file; the bundle loader fills it from `state_file`.
    #[serde(skip)]
    pub state_bytes: Option<Arc<[u8]>>,
    /// Last known parameter values, sorted by id (4.4, 17.1). Undo replays
    /// differences as parameter events.
    #[serde(default)]
    pub params: Vec<ParamValue>,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ParamValue {
    pub id: u32,
    pub value: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Wave {
    Sine,
    Saw,
    Square,
    Triangle,
}

/// Oscillator settings. `wave` is structural (in `Compiled`); `semitones`
/// and `cents` are continuous (in the `ParamTable`).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Osc {
    pub wave: Wave,
    /// -36 to 36.
    pub semitones: f64,
    /// -100 to 100.
    pub cents: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Adsr {
    /// 0 to 10000 ms.
    pub attack_ms: f64,
    pub decay_ms: f64,
    /// 0 to 1.
    pub sustain: f64,
    pub release_ms: f64,
}

/// Built-in synth (8): two oscillators, state-variable lowpass, two ADSRs.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SynthParams {
    pub osc1: Osc,
    pub osc2: Osc,
    /// 0 = only osc1, 1 = only osc2.
    pub osc_mix: f64,
    /// 20 to 20000 Hz.
    pub cutoff_hz: f64,
    /// 0 to 1.
    pub resonance: f64,
    /// Filter envelope depth in octaves, -8 to 8.
    pub filter_env_octaves: f64,
    pub amp_env: Adsr,
    pub filter_env: Adsr,
    pub gain_db: f64,
}

impl Default for SynthParams {
    fn default() -> SynthParams {
        SynthParams {
            osc1: Osc {
                wave: Wave::Saw,
                semitones: 0.0,
                cents: 0.0,
            },
            osc2: Osc {
                wave: Wave::Square,
                semitones: -12.0,
                cents: 0.0,
            },
            osc_mix: 0.3,
            cutoff_hz: 2000.0,
            resonance: 0.2,
            filter_env_octaves: 2.0,
            amp_env: Adsr {
                attack_ms: 2.0,
                decay_ms: 200.0,
                sustain: 0.7,
                release_ms: 150.0,
            },
            filter_env: Adsr {
                attack_ms: 2.0,
                decay_ms: 300.0,
                sustain: 0.2,
                release_ms: 200.0,
            },
            gain_db: -6.0,
        }
    }
}

/// Continuous synth parameters and their `ParamTable` index (17.1).
/// The engine reads these from the table; waveforms are structural.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum SynthParam {
    Osc1Semitones,
    Osc1Cents,
    Osc2Semitones,
    Osc2Cents,
    OscMix,
    CutoffHz,
    Resonance,
    FilterEnvOctaves,
    AmpAttackMs,
    AmpDecayMs,
    AmpSustain,
    AmpReleaseMs,
    FilterAttackMs,
    FilterDecayMs,
    FilterSustain,
    FilterReleaseMs,
    GainDb,
}

impl SynthParam {
    pub const ALL: [SynthParam; 17] = [
        SynthParam::Osc1Semitones,
        SynthParam::Osc1Cents,
        SynthParam::Osc2Semitones,
        SynthParam::Osc2Cents,
        SynthParam::OscMix,
        SynthParam::CutoffHz,
        SynthParam::Resonance,
        SynthParam::FilterEnvOctaves,
        SynthParam::AmpAttackMs,
        SynthParam::AmpDecayMs,
        SynthParam::AmpSustain,
        SynthParam::AmpReleaseMs,
        SynthParam::FilterAttackMs,
        SynthParam::FilterDecayMs,
        SynthParam::FilterSustain,
        SynthParam::FilterReleaseMs,
        SynthParam::GainDb,
    ];

    /// Index into the channel's `ParamTable` slot.
    pub fn index(self) -> usize {
        self as usize
    }

    /// Inclusive valid range.
    pub fn range(self) -> (f64, f64) {
        use SynthParam::*;
        match self {
            Osc1Semitones | Osc2Semitones => (-36.0, 36.0),
            Osc1Cents | Osc2Cents => (-100.0, 100.0),
            OscMix | Resonance | AmpSustain | FilterSustain => (0.0, 1.0),
            CutoffHz => (20.0, 20000.0),
            FilterEnvOctaves => (-8.0, 8.0),
            AmpAttackMs | AmpDecayMs | AmpReleaseMs | FilterAttackMs | FilterDecayMs
            | FilterReleaseMs => (0.0, 10000.0),
            GainDb => (crate::consts::MIN_GAIN_DB, crate::consts::MAX_GAIN_DB),
        }
    }
}

impl SynthParams {
    pub fn get(&self, p: SynthParam) -> f64 {
        use SynthParam::*;
        match p {
            Osc1Semitones => self.osc1.semitones,
            Osc1Cents => self.osc1.cents,
            Osc2Semitones => self.osc2.semitones,
            Osc2Cents => self.osc2.cents,
            OscMix => self.osc_mix,
            CutoffHz => self.cutoff_hz,
            Resonance => self.resonance,
            FilterEnvOctaves => self.filter_env_octaves,
            AmpAttackMs => self.amp_env.attack_ms,
            AmpDecayMs => self.amp_env.decay_ms,
            AmpSustain => self.amp_env.sustain,
            AmpReleaseMs => self.amp_env.release_ms,
            FilterAttackMs => self.filter_env.attack_ms,
            FilterDecayMs => self.filter_env.decay_ms,
            FilterSustain => self.filter_env.sustain,
            FilterReleaseMs => self.filter_env.release_ms,
            GainDb => self.gain_db,
        }
    }

    pub fn set(&mut self, p: SynthParam, v: f64) {
        use SynthParam::*;
        let slot = match p {
            Osc1Semitones => &mut self.osc1.semitones,
            Osc1Cents => &mut self.osc1.cents,
            Osc2Semitones => &mut self.osc2.semitones,
            Osc2Cents => &mut self.osc2.cents,
            OscMix => &mut self.osc_mix,
            CutoffHz => &mut self.cutoff_hz,
            Resonance => &mut self.resonance,
            FilterEnvOctaves => &mut self.filter_env_octaves,
            AmpAttackMs => &mut self.amp_env.attack_ms,
            AmpDecayMs => &mut self.amp_env.decay_ms,
            AmpSustain => &mut self.amp_env.sustain,
            AmpReleaseMs => &mut self.amp_env.release_ms,
            FilterAttackMs => &mut self.filter_env.attack_ms,
            FilterDecayMs => &mut self.filter_env.decay_ms,
            FilterSustain => &mut self.filter_env.sustain,
            FilterReleaseMs => &mut self.filter_env.release_ms,
            GainDb => &mut self.gain_db,
        };
        *slot = v;
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pattern {
    pub id: PatternId,
    pub name: String,
    /// The instrument whose notes this clip content holds (20.2). Clips
    /// that share this content are linked copies.
    pub instrument: ChannelId,
    pub length_steps: u8,
    pub step_ticks: u32,
    /// Swing in 1/1000 of a step (0 to `MAX_SWING`), applied by the
    /// compiler to step notes on odd steps (17.2).
    #[serde(default)]
    pub swing: u16,
    /// Sorted by `(start, key, id)`.
    #[serde(default)]
    pub notes: Vec<Note>,
}

impl Pattern {
    pub fn new(id: PatternId, name: String, instrument: ChannelId) -> Pattern {
        Pattern {
            id,
            name,
            instrument,
            length_steps: 16,
            step_ticks: DEFAULT_STEP_TICKS,
            swing: 0,
            notes: Vec::new(),
        }
    }

    pub fn length_ticks(&self) -> u32 {
        self.length_steps as u32 * self.step_ticks
    }

    pub fn note_count(&self) -> usize {
        self.notes.len()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Note {
    pub id: NoteId,
    pub start: u32,
    /// At least 1 tick.
    pub len: u32,
    /// 0 to 127.
    pub key: u8,
    /// 1 to 127.
    pub vel: u8,
    /// Step pitch-lane offset, -24 to 24 (17.2). Non-zero only on step
    /// notes, where `key == root_key + off`.
    #[serde(default)]
    pub off: i8,
    /// Ratchet: notes played inside this note, one of `RATCHETS` (17.2).
    #[serde(default = "one")]
    pub repeat: u8,
}

fn one() -> u8 {
    1
}

impl Note {
    pub fn end(&self) -> u32 {
        self.start + self.len
    }

    /// The step-note predicate of 5.2 with the pitch lane of 17.2: on the
    /// step grid, one step long, `key == root_key + off`, inside the pattern.
    pub fn is_step_note(&self, root_key: u8, pattern: &Pattern) -> bool {
        self.start.is_multiple_of(pattern.step_ticks)
            && self.len == pattern.step_ticks
            && self.key as i16 == root_key as i16 + self.off as i16
            && self.start < pattern.length_ticks()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Track {
    pub id: TrackId,
    pub name: String,
    pub mix: Mix,
    #[serde(default)]
    pub inserts: Vec<Insert>,
    /// Sends to other tracks, sorted by target id, at most `MAX_SENDS`.
    #[serde(default)]
    pub sends: Vec<Send>,
}

/// An insert slot on a mixer track: a CLAP plugin or a built-in effect (15.5).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Insert {
    Clap(ClapRef),
    Builtin {
        instance: InstanceId,
        fx: BuiltinFx,
        /// Skipped by the engine when true (24.1).
        #[serde(default)]
        bypass: bool,
    },
}

impl Insert {
    pub fn instance(&self) -> InstanceId {
        match self {
            Insert::Clap(r) => r.instance,
            Insert::Builtin { instance, .. } => *instance,
        }
    }
}

/// A send from a track to another (return) track (15.5).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Send {
    pub to: TrackId,
    pub level_db: f64,
    /// Tap before the fader (true) or after it (false).
    pub pre_fader: bool,
}

/// A sample in the bundle (17.2): `samples/<hash>.wav`, or for
/// `local_only` samples a per-machine path outside the bundle.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SampleRef {
    /// SHA-256 of the file, 64 lowercase hex digits.
    pub hash: String,
    /// Original file name, for display.
    pub orig_name: String,
    pub size: u64,
    pub local_only: bool,
}

/// A clip on an instrument's row (20.2). It plays its content (`pattern`)
/// starting `offset` ticks into the content; when `len` is longer than the
/// content the content repeats, when shorter it is cut.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Clip {
    pub id: ClipId,
    pub instrument: ChannelId,
    pub pattern: PatternId,
    pub start: u32,
    pub len: u32,
    #[serde(default)]
    pub offset: u32,
    #[serde(default)]
    pub muted: bool,
    /// Set for an audio clip (21.1); then `pattern` is `PatternId::NONE`.
    #[serde(default)]
    pub audio: Option<AudioSource>,
    /// Set when the clip belongs to a pattern instance (20.7).
    #[serde(default)]
    pub group: Option<ClipGroup>,
}

/// What an audio clip plays (21.1). Offsets and lengths are ticks on the
/// timeline; the engine converts with the tempo, without stretching.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioSource {
    /// A sample in `Project::samples`.
    pub sample: SampleHash,
    /// Gain in thousandths of a dB (an integer keeps `Clip` `Eq`).
    #[serde(default)]
    pub gain_mdb: i32,
    #[serde(default)]
    pub fade_in: u32,
    #[serde(default)]
    pub fade_out: u32,
}

/// SHA-256 of a sample; serialized as 64 lowercase hex digits, like
/// `SampleRef::hash`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SampleHash(pub [u8; 32]);

impl SampleHash {
    pub fn parse(hex: &str) -> Option<SampleHash> {
        if hex.len() != 64 {
            return None;
        }
        let mut out = [0u8; 32];
        for (i, b) in out.iter_mut().enumerate() {
            *b = u8::from_str_radix(hex.get(i * 2..i * 2 + 2)?, 16).ok()?;
        }
        Some(SampleHash(out))
    }

    pub fn to_hex(&self) -> String {
        self.0.iter().map(|b| format!("{b:02x}")).collect()
    }
}

impl Serialize for SampleHash {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for SampleHash {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<SampleHash, D::Error> {
        let s = String::deserialize(d)?;
        SampleHash::parse(&s)
            .ok_or_else(|| serde::de::Error::custom("sample hash must be 64 hex digits"))
    }
}

/// Membership of a clip in one placed instance of a pattern (20.7).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClipGroup {
    pub group: GroupId,
    /// Which placement of the pattern; unique per group.
    pub instance: u32,
}

/// A named group of clips across rows, placed as one block (20.7).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PatternGroup {
    pub id: GroupId,
    pub name: String,
    /// 0xRRGGBB.
    pub color: u32,
}

/// What a shape controls (24.2-1).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ShapeTarget {
    /// Track volume in dB (-60..+6).
    Volume { track: TrackId },
    /// Track pan (-1..1).
    Pan { track: TrackId },
    /// Instrument pitch offset in semitones (-24..24).
    Pitch { instrument: ChannelId },
    /// Instrument low-pass cutoff, 0..1 (closed..open).
    Filter { instrument: ChannelId },
    /// A built-in effect parameter, in its own range.
    FxParam {
        track: TrackId,
        instance: InstanceId,
        param: u16,
    },
}

/// How a shape moves from one point to the next (24.2-1).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Curve {
    #[default]
    Smooth,
    Linear,
    Hold,
    Stairs,
    Pulse,
    Wave,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShapePoint {
    /// Ticks on the timeline.
    pub tick: u32,
    pub value: f32,
    /// Shape of the segment that starts at this point.
    #[serde(default)]
    pub curve: Curve,
}

/// An automation curve on the timeline (24.2-1). Points are sorted by
/// tick; before the first and after the last the value holds.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Shape {
    pub id: ShapeId,
    pub target: ShapeTarget,
    pub points: Vec<ShapePoint>,
}

impl Clip {
    pub fn end(&self) -> u32 {
        self.start + self.len
    }
}

/// The timeline loop region (20.2). `end > start` when set.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoopRegion {
    pub start: u32,
    pub end: u32,
    pub enabled: bool,
}

/// Ticks per bar for a project's time signature.
pub fn ticks_per_bar(time_sig_num: u8) -> u32 {
    time_sig_num as u32 * PPQ
}
