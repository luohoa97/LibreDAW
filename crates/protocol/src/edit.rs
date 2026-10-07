// SPDX-License-Identifier: GPL-3.0-or-later
//! Every document mutation, as one enum (6). The UI, the Deno bridge, and
//! the MCP bridge all produce these values; `apply()` in the `ui` crate
//! turns a `Document` plus an `Edit` into a new `Document` or an error.
//!
//! Edits that create entities carry no ids: `apply()` allocates them from
//! the document's counter and reports them in `Applied::created`.
//! Adding variants is an interface change (orchestrator only).

use serde::{Deserialize, Serialize};

use crate::beats::{Bass808Param, BuiltinFxKind, SampleMode, SamplerParam, SaturatorCurve};
use crate::ids::{ChannelId, ClipId, GroupId, InstanceId, NoteId, PatternId, ShapeId, TrackId};
use crate::model::{Mix, SampleRef, SynthParam, SynthParams, Wave};
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
        /// Factory preset loaded right after creation, as a path under the
        /// plugin's preset root (crates/plugin-host/presets/instruments.toml),
        /// for example "Basses/Sub 1.fxp". The saved plugin state is what
        /// persists; this field only records the choice.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        preset: Option<String>,
    },
    /// A sampler playing a registered sample (15.1). `sample` must already be
    /// in the project (`Edit::AddSample`), or `None` for an empty sampler.
    Sampler {
        sample: Option<String>,
        mode: SampleMode,
    },
    /// The native 808 (15.2) with default parameters.
    Bass808 {
        mono: bool,
    },
    /// An audio row for audio clips (21.1).
    Audio,
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

    // Clip contents (the former patterns; one instrument each, 20.2).
    /// Creates content for an instrument without placing it; `AddClip`
    /// with `pattern: None` is the usual way to make a clip.
    AddPattern {
        instrument: ChannelId,
        name: String,
        length_steps: u8,
    },
    /// Also removes every clip that uses it.
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
    /// step note at that step, whatever its pitch offset (5.2, 17.2).
    SetStep {
        pattern: PatternId,
        step: u8,
        on: bool,
        vel: Option<u8>,
    },
    AddNotes {
        pattern: PatternId,
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

    // ----- Milestone B (15, 17.2) -----

    // Steps and groove
    /// Sets the lanes of an existing step note: velocity, pitch offset
    /// (`key` becomes `root_key + off`), ratchet count. `None` keeps a value.
    /// Fails if there is no step note at that step.
    SetStepLanes {
        pattern: PatternId,
        step: u8,
        vel: Option<u8>,
        off: Option<i8>,
        repeat: Option<u8>,
    },
    /// Ratchet count of piano-roll notes; `len` must be divisible by it.
    SetNoteRepeat {
        pattern: PatternId,
        notes: Vec<NoteId>,
        repeat: u8,
    },
    SetSwing {
        pattern: PatternId,
        swing: u16,
    },
    SetChokeGroup {
        channel: ChannelId,
        group: u8,
    },

    // Samples
    /// Registers a sample file already written to the bundle (or recorded
    /// as local-only, 17.2). Adding an existing hash is a no-op.
    AddSample {
        sample: SampleRef,
    },
    /// Fails if a sampler still uses it.
    RemoveSample {
        hash: String,
    },

    // Sampler
    SetSamplerSample {
        channel: ChannelId,
        sample: Option<String>,
    },
    SetSamplerMode {
        channel: ChannelId,
        mode: SampleMode,
        reverse: bool,
    },
    SetSamplerParam {
        channel: ChannelId,
        param: SamplerParam,
        value: f64,
    },

    // 808
    SetBass808Mono {
        channel: ChannelId,
        mono: bool,
    },
    SetBass808Param {
        channel: ChannelId,
        param: Bass808Param,
        value: f64,
    },

    // Built-in effects and routing
    /// Adds a built-in effect with default parameters at `index`.
    AddBuiltinInsert {
        track: TrackId,
        index: u8,
        fx: BuiltinFxKind,
    },
    /// Continuous parameter by table index (`BuiltinFx::param_name`).
    SetFxParam {
        track: TrackId,
        instance: InstanceId,
        param: u8,
        value: f64,
    },
    SetSaturatorCurve {
        track: TrackId,
        instance: InstanceId,
        curve: SaturatorCurve,
    },
    SetDelayPingPong {
        track: TrackId,
        instance: InstanceId,
        ping_pong: bool,
    },
    /// Key input of a compressor; fails if it would create a loop.
    SetSidechain {
        track: TrackId,
        instance: InstanceId,
        source: Option<TrackId>,
    },
    /// Moves an insert to a new position on the same track.
    MoveInsert {
        track: TrackId,
        instance: InstanceId,
        index: u8,
    },
    /// Adds or updates the send from `track` to `to`.
    SetSend {
        track: TrackId,
        to: TrackId,
        level_db: f64,
        pre_fader: bool,
    },
    RemoveSend {
        track: TrackId,
        to: TrackId,
    },

    // Timeline (20.2, 20.3)
    /// Places a clip on an instrument's row. `pattern: None` creates new
    /// empty content (one bar of 16 steps) for it; `Some` makes a linked
    /// copy of existing content of the same instrument. Fails on overlap.
    AddClip {
        instrument: ChannelId,
        pattern: Option<PatternId>,
        start: u32,
        len: u32,
    },
    /// Copies clips to `start + dt` on the same rows. `linked: true` shares
    /// content (Duplicate); `false` copies content (Copy). Fails on overlap.
    DuplicateClips {
        clips: Vec<ClipId>,
        dt: i64,
        linked: bool,
    },
    /// Removes clips; content no clip uses any more is removed too.
    RemoveClips {
        clips: Vec<ClipId>,
    },
    /// Moves clips in time. Fails on overlap or out of range.
    MoveClips {
        clips: Vec<ClipId>,
        dt: i64,
    },
    /// Moves one clip to another instrument's row; its content is copied
    /// (made unique) and retargeted to that instrument.
    MoveClipToInstrument {
        clip: ClipId,
        instrument: ChannelId,
    },
    /// Changes clip length at the end (`from_start: false`) or at the start
    /// (`true`, which also shifts `start` and `offset`).
    ResizeClips {
        clips: Vec<ClipId>,
        dlen: i64,
        from_start: bool,
    },
    /// Splits a clip at an absolute tick into two clips sharing content.
    SplitClip {
        clip: ClipId,
        at: u32,
    },
    /// Gives a clip its own copy of its content.
    MakeUnique {
        clip: ClipId,
    },
    SetClipMuted {
        clips: Vec<ClipId>,
        muted: bool,
    },
    SetLoopRegion {
        start: u32,
        end: u32,
        enabled: bool,
    },
    /// An audio clip of a sample already in the project (21.1), on an
    /// Audio row. `offset` trims the start; `len` the visible length.
    AddAudioClip {
        instrument: ChannelId,
        sample: crate::model::SampleHash,
        start: u32,
        len: u32,
        offset: u32,
    },
    SetClipAudio {
        clip: ClipId,
        /// Gain in thousandths of a dB.
        gain_mdb: i32,
        fade_in: u32,
        fade_out: u32,
    },
    /// Groups clips (one or more per row) into a new pattern instance
    /// (20.7). They are moved to share the earliest start.
    MakePattern {
        clips: Vec<ClipId>,
        name: String,
    },
    /// Places another instance of a pattern at `start`: linked copies of
    /// the members of its first instance.
    PlacePattern {
        group: GroupId,
        start: u32,
    },
    /// Removes clips from their pattern instance (they stay on the rows).
    Ungroup {
        clips: Vec<ClipId>,
    },
    RenameGroup {
        group: GroupId,
        name: String,
    },
    AddShape {
        target: crate::model::ShapeTarget,
        points: Vec<crate::model::ShapePoint>,
    },
    /// Replaces all points; they are sorted by tick.
    SetShapePoints {
        shape: ShapeId,
        points: Vec<crate::model::ShapePoint>,
    },
    RemoveShape {
        shape: ShapeId,
    },
    SetInsertBypass {
        track: TrackId,
        instance: InstanceId,
        bypass: bool,
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
                | Edit::SetSamplerParam { .. }
                | Edit::SetBass808Param { .. }
                | Edit::SetFxParam { .. }
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
