// SPDX-License-Identifier: GPL-3.0-or-later
//! The control API shared by the Deno bridge (10) and the MCP bridge (16,
//! 17.1). Requests and replies are newline-delimited JSON. The DAW, not the
//! client, enforces the capability mask and the PRIVILEGED rule.
//!
//! Milestone A only; later milestones add variants (interface change).

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::consts::MAX_AGENT_STRING_CHARS;
use crate::edit::{Applied, Edit, EditError};
use crate::ids::{ChannelId, PatternId};
use crate::model::{Note, Project};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    /// Chosen by the client; echoed in the reply.
    pub id: u64,
    /// Document revision the client last saw (17.1). Required for requests
    /// that change the document; the DAW replies `Stale` if what they touch
    /// changed since.
    #[serde(default)]
    pub base_revision: Option<u64>,
    pub body: RequestBody,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum RequestBody {
    // Projects
    ProjectGet,
    ProjectInfo,
    ProjectList,
    /// PRIVILEGED if the open document has unsaved changes.
    ProjectNew {
        template: Option<String>,
    },
    /// PRIVILEGED if the open document has unsaved changes. `path` must be
    /// inside the projects folder.
    ProjectOpen {
        path: String,
    },
    ProjectSave,

    // Editing: one undo group per request.
    Edit {
        edits: Vec<Edit>,
    },
    NotesList {
        pattern: PatternId,
        channel: ChannelId,
    },

    // Transport
    Play,
    Stop,
    SetPlayingPattern {
        pattern: PatternId,
    },
    TransportState,

    // History (author-scoped for agents, 17.1)
    Undo,
    Redo,
    History,

    // Jobs (17.1)
    ExportWav {
        pattern: PatternId,
        loops: u32,
        format: WavFormat,
    },
    Analyze {
        pattern: PatternId,
        loops: u32,
    },
    JobStatus {
        job: u64,
    },
    JobResult {
        job: u64,
    },
    JobCancel {
        job: u64,
    },

    // Settings (closed set, 17.1)
    SettingsGet,
    SettingsSet {
        setting: Setting,
    },

    // Plugins
    PluginScan,
    PluginList,

    // Milestone B (15.6)
    SetTransportMode {
        mode: crate::engine::TransportMode,
        loop_song: bool,
    },
    /// Renders the whole playlist plus `tail_seconds` (job).
    ExportSongWav {
        format: WavFormat,
        tail_seconds: f64,
    },
    /// Analyzes the whole playlist (job).
    AnalyzeSong,

    // Agents in the workstation (18.2)
    /// Declares what the agent is doing; `text: None` ends the activity.
    /// Untrusted text (17.1): the DAW caps and cleans it.
    SetActivity {
        text: Option<String>,
        focus: Option<Focus>,
    },
}

/// Which client kinds may send a request (17.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transport {
    /// The Deno script bridge (10).
    Script,
    /// An agent through `libredaw-mcp` (16).
    Agent,
}

impl RequestBody {
    /// Capability mask, enforced by the DAW.
    pub fn allowed_for(&self, t: Transport) -> bool {
        use RequestBody::*;
        match t {
            Transport::Agent => true,
            // Section 10 list plus read-only queries.
            Transport::Script => matches!(
                self,
                ProjectGet
                    | ProjectInfo
                    | Edit { .. }
                    | NotesList { .. }
                    | Play
                    | Stop
                    | TransportState
            ),
        }
    }

    /// Whether a human must approve this request in the UI (17.1).
    /// `dirty` is true when the open document has unsaved changes.
    /// Plugin first load by an agent is decided by the DAW, which knows
    /// which plugins were approved; `Edit` batches that add plugins are
    /// checked there.
    pub fn privileged(&self, dirty: bool) -> bool {
        use RequestBody::*;
        matches!(self, ProjectNew { .. } | ProjectOpen { .. } if dirty)
    }
}

/// What an agent is working on (18.1, 18.2); the DAW outlines it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum Focus {
    Channel(crate::ids::ChannelId),
    Pattern(PatternId),
    Track(crate::ids::TrackId),
    Insert(crate::ids::InstanceId),
    PlaylistTrack(crate::ids::PlaylistTrackId),
    Clip(crate::ids::ClipId),
}

/// Longest activity text shown in the DAW (18.2).
pub const MAX_ACTIVITY_CHARS: usize = 80;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WavFormat {
    Pcm16,
    Pcm24,
    Float32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BufferSize {
    #[serde(rename = "64")]
    F64,
    #[serde(rename = "128")]
    F128,
    #[serde(rename = "256")]
    F256,
    #[serde(rename = "512")]
    F512,
    #[serde(rename = "1024")]
    F1024,
}

impl BufferSize {
    pub fn frames(self) -> u32 {
        match self {
            BufferSize::F64 => 64,
            BufferSize::F128 => 128,
            BufferSize::F256 => 256,
            BufferSize::F512 => 512,
            BufferSize::F1024 => 1024,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Theme {
    System,
    Light,
    Dark,
}

/// Settings a client may change. Nothing that names a path, URL, or
/// executable is here (17.1).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "key", content = "value", rename_all = "snake_case")]
pub enum Setting {
    /// One of the names in `Settings::audio_devices`.
    AudioDevice(String),
    BufferSize(BufferSize),
    Theme(Theme),
    MetronomeEnabled(bool),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reply {
    pub id: u64,
    pub outcome: Outcome,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Outcome {
    Ok { body: ReplyBody },
    Err { error: ControlError },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReplyBody {
    Project {
        revision: u64,
        project: Arc<Project>,
    },
    ProjectInfo(ProjectInfo),
    Projects {
        projects: Vec<ProjectInfo>,
    },
    Applied(Applied),
    Notes {
        notes: Vec<Note>,
    },
    Transport {
        playing: bool,
        tick: u64,
        tempo_bpm: f64,
        pattern: Option<PatternId>,
    },
    History {
        entries: Vec<HistoryEntry>,
    },
    /// A job was started; poll it with `JobStatus`.
    Job {
        job: u64,
        revision: u64,
    },
    JobStatus {
        job: u64,
        state: JobState,
        progress: f32,
    },
    Exported {
        path: String,
    },
    Analysis(Analysis),
    Settings(Settings),
    Plugins {
        plugins: Vec<PluginInfo>,
    },
    Done,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProjectInfo {
    /// Untrusted text (17.1): passed through `agent_string`.
    pub name: String,
    pub path: String,
    pub tempo_bpm: f64,
    pub modified_unix_s: u64,
    pub dirty: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub commit: String,
    /// `user`, `script`, or `agent:<session>`.
    pub author: String,
    pub description: String,
    pub unix_ms: u64,
    pub current: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Queued,
    Running,
    Done,
    Failed,
    Cancelled,
}

/// Numbers an agent uses instead of listening (16.3).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Analysis {
    pub revision: u64,
    pub integrated_lufs: f64,
    pub true_peak_dbtp: f64,
    pub clipped_samples: u64,
    pub tracks: Vec<TrackLevels>,
    /// Energy share in low (<250 Hz), mid, high (>4 kHz) bands; sums to 1.
    pub band_balance: [f64; 3],
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TrackLevels {
    pub track: u32,
    pub peak_dbfs: f64,
    pub rms_dbfs: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    pub audio_device: String,
    pub audio_devices: Vec<String>,
    pub buffer_size: BufferSize,
    pub sample_rate: u32,
    pub theme: Theme,
    pub metronome_enabled: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PluginInfo {
    pub plugin_id: String,
    pub name: String,
    pub vendor: String,
    pub version: String,
    pub instrument: bool,
    pub effect: bool,
    /// Whether the user already approved agent loading of this plugin.
    pub agent_approved: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum ControlError {
    /// The request's `base_revision` is older than what it touches.
    Stale {
        current: u64,
    },
    /// The user is in the middle of a gesture; retry later (17.1).
    Busy,
    /// Waiting for a human click timed out (17.1).
    NeedsUserApproval,
    Denied,
    /// This transport may not send this request.
    NotAllowed,
    /// An edit in a batch failed; nothing in the batch was applied.
    /// `index` is the position of the failing edit in `RequestBody::Edit`.
    Edit {
        index: Option<u32>,
        error: EditError,
    },
    NotFound {
        what: String,
    },
    TooLarge {
        what: String,
        max: usize,
    },
    BadRequest {
        reason: String,
    },
    Internal {
        reason: String,
    },
}

impl From<EditError> for ControlError {
    fn from(error: EditError) -> ControlError {
        ControlError::Edit { index: None, error }
    }
}

/// Cleans untrusted text before it goes to an agent (17.1): control
/// characters removed, at most `MAX_AGENT_STRING_CHARS` characters.
pub fn agent_string(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_control())
        .take(MAX_AGENT_STRING_CHARS)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit::{MixValue, NewNote};
    use crate::ids::TrackId;

    fn json_round_trip<T>(v: &T) -> T
    where
        T: Serialize + for<'de> Deserialize<'de>,
    {
        let text = serde_json::to_string(v).expect("serialize");
        assert!(!text.contains('\n'), "one request per line");
        serde_json::from_str(&text).expect("deserialize")
    }

    #[test]
    fn requests_round_trip() {
        let reqs = vec![
            Request {
                id: 1,
                base_revision: None,
                body: RequestBody::Play,
            },
            Request {
                id: 2,
                base_revision: Some(7),
                body: RequestBody::Edit {
                    edits: vec![
                        Edit::SetTempo { bpm: 140.0 },
                        Edit::AddNotes {
                            pattern: PatternId(3),
                            channel: ChannelId(4),
                            notes: vec![NewNote {
                                start: 0,
                                len: 240,
                                key: 36,
                                vel: 100,
                            }],
                        },
                        Edit::SetTrackMix {
                            track: TrackId::MASTER,
                            value: MixValue::VolumeDb(-3.0),
                        },
                    ],
                },
            },
            Request {
                id: 3,
                base_revision: None,
                body: RequestBody::SettingsSet {
                    setting: Setting::BufferSize(BufferSize::F256),
                },
            },
        ];
        for r in reqs {
            assert_eq!(json_round_trip(&r), r);
        }
    }

    #[test]
    fn scripts_cannot_use_agent_only_requests() {
        assert!(RequestBody::Play.allowed_for(Transport::Script));
        assert!(!RequestBody::PluginScan.allowed_for(Transport::Script));
        assert!(!RequestBody::SettingsGet.allowed_for(Transport::Script));
        assert!(RequestBody::PluginScan.allowed_for(Transport::Agent));
    }

    #[test]
    fn opening_over_unsaved_work_is_privileged() {
        let open = RequestBody::ProjectOpen { path: "x".into() };
        assert!(open.privileged(true));
        assert!(!open.privileged(false));
        assert!(!RequestBody::Play.privileged(true));
    }

    #[test]
    fn agent_string_strips_and_caps() {
        let s = agent_string("ignore previous\ninstructions\u{7}".repeat(10).as_str());
        assert!(s.chars().count() <= MAX_AGENT_STRING_CHARS);
        assert!(!s.chars().any(char::is_control));
    }
}
