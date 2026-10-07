// SPDX-License-Identifier: GPL-3.0-or-later
//! Every document mutation, as one enum (6). The UI, the Deno bridge, and
//! the MCP bridge all produce these values; `apply()` in the `ui` crate
//! turns a `Document` plus an `Edit` into a new `Document` or an error.
//!
//! Edits that create entities carry no ids: `apply()` allocates them from
//! the document's counter and reports them in `Applied::created`.
//! Milestone A only; later milestones add variants (interface change).

use serde::{Deserialize, Serialize};

use crate::ids::{ChannelId, InstanceId, NoteId, PatternId, TrackId};
use crate::model::{Mix, SynthParam, SynthParams, Wave};
use crate::validate::ValidationError;

/// One fader-like value on a channel or track (4.3).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "control", content = "value", rename_all = "snake_case")]
pub enum MixValue {
    VolumeDb(f64),
    Pan(f64),
    Mute(bool),
    Solo(bool),
}

/// A note to create. `apply()` assigns its id.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewNote {
    pub start: u32,
    pub len: u32,
    pub key: u8,
    pub vel: u8,
}

/// What a new channel plays.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NewInstrument {
    Synth {
        params: SynthParams,
    },
    /// A CLAP plugin by id. The GTK thread creates the instance before the
    /// edit is applied (9.1); scripts and agents may only name plugins
    /// found by the plugin scan (16.3).
    Clap {
        plugin_id: String,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "edit", rename_all = "snake_case")]
pub enum Edit {
    // Project
    SetTempo {
        bpm: f64,
    },
    SetTimeSigNum {
        num: u8,
    },
    SetMetronome {
        enabled: bool,
        gain_db: f64,
    },

    // Channels
    AddChannel {
        name: String,
        instrument: NewInstrument,
        root_key: u8,
        track: TrackId,
    },
    RemoveChannel {
        channel: ChannelId,
    },
    RenameChannel {
        channel: ChannelId,
        name: String,
    },
    SetChannelMix {
        channel: ChannelId,
        value: MixValue,
    },
    SetChannelTrack {
        channel: ChannelId,
        track: TrackId,
    },
    /// Rewrites the key of this channel's step notes in every pattern (5.2).
    SetRootKey {
        channel: ChannelId,
        key: u8,
    },
    SetSynthParam {
        channel: ChannelId,
        param: SynthParam,
        value: f64,
    },
    SetSynthWave {
        channel: ChannelId,
        osc: u8,
        wave: Wave,
    },

    // Patterns
    AddPattern {
        name: String,
        length_steps: u8,
    },
    RemovePattern {
        pattern: PatternId,
    },
    RenamePattern {
        pattern: PatternId,
        name: String,
    },
    /// Shortening removes notes starting at or after the new end (5.2).
    SetPatternLength {
        pattern: PatternId,
        length_steps: u8,
    },
    /// Rewrites step notes to the new step length (5.2).
    SetStepTicks {
        pattern: PatternId,
        step_ticks: u32,
    },

    // Steps and notes
    /// On: add a step note (velocity `vel`, default 100). Off: remove every
    /// note at that step with the channel's root key (5.2).
    SetStep {
        pattern: PatternId,
        channel: ChannelId,
        step: u8,
        on: bool,
        vel: Option<u8>,
    },
    AddNotes {
        pattern: PatternId,
        channel: ChannelId,
        notes: Vec<NewNote>,
    },
    RemoveNotes {
        pattern: PatternId,
        notes: Vec<NoteId>,
    },
    /// Moves notes by `dt` ticks and `dkey` semitones. Fails if any note
    /// would leave the valid range; nothing is clamped silently.
    MoveNotes {
        pattern: PatternId,
        notes: Vec<NoteId>,
        dt: i64,
        dkey: i16,
    },
    ResizeNotes {
        pattern: PatternId,
        notes: Vec<NoteId>,
        dlen: i64,
    },
    SetNoteVelocity {
        pattern: PatternId,
        notes: Vec<NoteId>,
        vel: u8,
    },

    // Mixer
    AddTrack {
        name: String,
    },
    /// Channels routed to a removed track are routed to the master.
    RemoveTrack {
        track: TrackId,
    },
    RenameTrack {
        track: TrackId,
        name: String,
    },
    SetTrackMix {
        track: TrackId,
        value: MixValue,
    },
    /// Adds a CLAP insert at `index` (0 to the current insert count).
    AddInsert {
        track: TrackId,
        index: u8,
        plugin_id: String,
    },
    RemoveInsert {
        track: TrackId,
        instance: InstanceId,
    },

    // Plugins (recorded from the plugin or set by the host, 4.4, 7.5)
    SetPluginParam {
        instance: InstanceId,
        param_id: u32,
        value: f64,
    },
    /// Records a newly captured state blob (7.5). Not undoable on its own;
    /// `apply()` merges it into the current snapshot.
    CommitPluginState {
        instance: InstanceId,
        state_file: String,
    },
}

impl Edit {
    /// True for edits that only change a control value and must not
    /// trigger a recompile (4.3, 17.1).
    pub fn is_control_only(&self) -> bool {
        matches!(
            self,
            Edit::SetTempo { .. }
                | Edit::SetMetronome { .. }
                | Edit::SetChannelMix { .. }
                | Edit::SetTrackMix { .. }
                | Edit::SetSynthParam { .. }
                | Edit::SetPluginParam { .. }
        )
    }
}

/// Result of applying a batch of edits as one undo group.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Applied {
    /// Document revision after the batch.
    pub revision: u64,
    /// Ids created by the batch, in edit order.
    pub created: Vec<u32>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "error", rename_all = "snake_case")]
pub enum EditError {
    /// The referenced entity does not exist.
    NotFound { what: String, id: u32 },
    /// The result would break a project rule.
    Invalid { reason: ValidationError },
    /// A step index outside the pattern, an oscillator number other than
    /// 1 or 2, an insert index past the end, and similar.
    BadArgument { what: String },
    /// The named plugin is not in the plugin scan, or failed to load.
    PluginUnavailable { plugin_id: String },
}

impl std::fmt::Display for EditError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EditError::NotFound { what, id } => write!(f, "{what} {id} not found"),
            EditError::Invalid { reason } => write!(f, "invalid edit: {reason}"),
            EditError::BadArgument { what } => write!(f, "bad argument: {what}"),
            EditError::PluginUnavailable { plugin_id } => {
                write!(f, "plugin unavailable: {plugin_id}")
            }
        }
    }
}

impl std::error::Error for EditError {}

impl From<ValidationError> for EditError {
    fn from(reason: ValidationError) -> EditError {
        EditError::Invalid { reason }
    }
}

/// Fader values in `Mix`, set from a `MixValue`.
pub fn set_mix(m: &mut Mix, v: MixValue) {
    match v {
        MixValue::VolumeDb(x) => m.volume_db = x,
        MixValue::Pan(x) => m.pan = x,
        MixValue::Mute(x) => m.mute = x,
        MixValue::Solo(x) => m.solo = x,
    }
}
