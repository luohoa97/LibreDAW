// SPDX-License-Identifier: GPL-3.0-or-later
//! The document schema for Milestone A (SPEC 5.1, 17.1).
//!
//! `Project` is an `Arc` tree so undo snapshots share unchanged parts
//! (section 6). Edits clone only the touched path with `Arc::make_mut`.
//! Collections are kept sorted by id; the emitter in `format` sorts again
//! so the file is canonical even if a caller forgets.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::consts::{DEFAULT_STEP_TICKS, PPQ};
use crate::ids::{ChannelId, InstanceId, NoteId, PatternId, TrackId};

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
            })],
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
            for cn in &p.notes {
                for n in &cn.notes {
                    m = m.max(n.id.0);
                }
            }
        }
        for t in &self.tracks {
            m = m.max(t.id.0);
            for Insert::Clap(r) in &t.inserts {
                m = m.max(r.instance.0);
            }
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
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Instrument {
    Synth(SynthParams),
    Clap(ClapRef),
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
    pub length_steps: u8,
    pub step_ticks: u32,
    /// Notes per channel, sorted by channel id. Channels with no notes in
    /// this pattern have no entry.
    #[serde(default)]
    pub notes: Vec<ChannelNotes>,
}

impl Pattern {
    pub fn new(id: PatternId, name: String) -> Pattern {
        Pattern {
            id,
            name,
            length_steps: 16,
            step_ticks: DEFAULT_STEP_TICKS,
            notes: Vec::new(),
        }
    }

    pub fn length_ticks(&self) -> u32 {
        self.length_steps as u32 * self.step_ticks
    }

    pub fn notes_of(&self, channel: ChannelId) -> &[Note] {
        self.notes
            .iter()
            .find(|c| c.channel == channel)
            .map(|c| c.notes.as_slice())
            .unwrap_or(&[])
    }

    pub fn note_count(&self) -> usize {
        self.notes.iter().map(|c| c.notes.len()).sum()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChannelNotes {
    pub channel: ChannelId,
    /// Sorted by `(start, key, id)`.
    pub notes: Vec<Note>,
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
}

impl Note {
    pub fn end(&self) -> u32 {
        self.start + self.len
    }

    /// The step-note predicate of 5.2 for Milestone A. Milestone B adds the
    /// pitch offset of 17.2 with a format version bump.
    pub fn is_step_note(&self, root_key: u8, pattern: &Pattern) -> bool {
        self.start.is_multiple_of(pattern.step_ticks)
            && self.len == pattern.step_ticks
            && self.key == root_key
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
}

/// An insert slot on a mixer track. Milestone B adds built-in effects as a
/// second variant (15.5).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Insert {
    Clap(ClapRef),
}

/// Ticks per bar for a project's time signature.
pub fn ticks_per_bar(time_sig_num: u8) -> u32 {
    time_sig_num as u32 * PPQ
}
