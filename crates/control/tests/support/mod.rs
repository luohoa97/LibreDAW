// SPDX-License-Identifier: GPL-3.0-or-later
//! Shared test support: a reference bridge and a raw MCP client.
//!
//! `RefDaw` answers control requests the way `crates/control/BRIDGE.md`
//! says the ui bridge must: with the real `doc` document and change tree
//! (`doc::history::Editor`), the request's client as the commit author, the
//! strict revision rule, and author-scoped undo. It runs inside the fake UI
//! loop (`control::fake_ui`), so tool calls travel the real socket, the real
//! control server and the real `apply()`.

#![allow(dead_code)]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use control::fake_ui::{Action, FakeUi};
use control::{ControlConfig, ControlServer, Incoming, PROTOCOL_VERSION};
use doc::document::Document;
use doc::history::{Author, BranchError, Editor, HistoryError, Scope, Submitted};
use protocol::beats::SampleMode;
use protocol::control::{
    BufferSize, ControlError, Focus, JobState, Outcome, ReplyBody, RequestBody, Settings,
    SoundInfo, Theme, Transport,
};
use protocol::edit::{Applied, Edit, NewInstrument};
use protocol::ids::{ChannelId, TrackId};
use protocol::model::SynthParams;
use serde_json::{Value, json};

static COUNTER: AtomicU32 = AtomicU32::new(0);

// ---- the reference bridge ------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum JobKind {
    Export,
    Analyze,
}

/// What the fake user does with the Hum sheet.
pub enum HumReply {
    /// Closes it.
    Decline,
    /// Hums these notes (positions in ticks from the clip start).
    Notes(Vec<protocol::edit::NewNote>),
}

pub struct RefDaw {
    /// Sheets agents asked for through `hum_prepare`.
    pub hum_asked: Vec<control::hum::Prepare>,
    pub hum_reply: HumReply,
    hum_applied: Option<Applied>,
    pub editor: Editor,
    pub playing: bool,
    pub activity: Vec<(Option<String>, Option<Focus>)>,
    jobs: Vec<(JobKind, u32)>,
    token: u64,
}

fn ok(body: ReplyBody) -> Outcome {
    Outcome::Ok { body }
}

fn err(error: ControlError) -> Outcome {
    Outcome::Err { error }
}

fn bad(reason: impl Into<String>) -> Outcome {
    err(ControlError::BadRequest {
        reason: reason.into(),
    })
}

fn branch_err(e: BranchError) -> Outcome {
    match e {
        BranchError::GestureOpen => err(ControlError::Busy),
        BranchError::UnknownBranch(b) => err(ControlError::NotFound {
            what: format!("branch {b}"),
        }),
        BranchError::UnknownCommit(c) => err(ControlError::NotFound {
            what: format!("commit {c}"),
        }),
        other => bad(other.to_string()),
    }
}

/// The author of a client's commits (the ui's `author_of`).
pub fn author_of(inc: &Incoming) -> Author {
    match inc.client.transport {
        Transport::Script => Author::Script,
        Transport::Agent => Author::Agent(format!("{}-{}", inc.client.name, inc.client.id)),
    }
}

impl RefDaw {
    /// An empty project.
    pub fn empty() -> RefDaw {
        RefDaw {
            hum_asked: Vec::new(),
            hum_reply: HumReply::Decline,
            hum_applied: None,
            editor: Editor::new(Document::new(), true),
            playing: false,
            activity: Vec::new(),
            jobs: Vec::new(),
            token: 0,
        }
    }

    /// Kick (instrument 1, root C2) and hat (2, root F#2) synths on the
    /// master, each with a one-bar clip: kick content 3 in clip 4, hat
    /// content 5 in clip 6. Revision 1.
    pub fn kick_and_hat() -> RefDaw {
        let mut d = RefDaw::empty();
        let synth = |name: &str, key: u8| Edit::AddChannel {
            name: name.into(),
            instrument: NewInstrument::Synth {
                params: SynthParams::default(),
            },
            root_key: key,
            track: TrackId::MASTER,
        };
        d.user_edit(vec![
            synth("kick", 36),
            synth("hat", 42),
            Edit::AddClip {
                instrument: ChannelId(1),
                pattern: None,
                start: 0,
                len: 3840,
            },
            Edit::AddClip {
                instrument: ChannelId(2),
                pattern: None,
                start: 0,
                len: 3840,
            },
        ]);
        d
    }

    pub fn revision(&self) -> u64 {
        self.editor.document().revision
    }

    pub fn project(&self) -> Arc<protocol::model::Project> {
        self.editor.document().project.clone()
    }

    /// An edit the user makes in the window.
    pub fn user_edit(&mut self, edits: Vec<Edit>) -> Applied {
        self.token += 1;
        match self
            .editor
            .submit(Author::User, None, edits, self.token)
            .expect("user edit applies")
        {
            Submitted::Applied(a) => Applied {
                revision: a.revision,
                created: a.created,
            },
            Submitted::Queued => panic!("no gesture in tests"),
        }
    }

    fn stale(&self, base: Option<u64>) -> Option<Outcome> {
        let current = self.revision();
        base.filter(|b| *b < current)
            .map(|_| err(ControlError::Stale { current }))
    }

    fn applied_now(&self) -> Outcome {
        ok(ReplyBody::Applied(Applied {
            revision: self.revision(),
            created: Vec::new(),
        }))
    }

    fn branches(&self) -> ReplyBody {
        ReplyBody::Branches {
            current: self.editor.current_branch().to_string(),
            branches: self.editor.branch_infos(),
        }
    }

    /// One request, as BRIDGE.md specifies.
    pub fn answer(&mut self, inc: &Incoming) -> Outcome {
        let author = author_of(inc);
        let scope = match &author {
            Author::User => Scope::Any,
            a => Scope::Only(a.clone()),
        };
        let base = inc.request.base_revision;
        match &inc.request.body {
            RequestBody::ProjectGet => ok(ReplyBody::Project {
                revision: self.revision(),
                project: self.project(),
                next_id: self.editor.document().next_id,
            }),
            RequestBody::ProjectInfo => {
                ok(ReplyBody::ProjectInfo(protocol::control::ProjectInfo {
                    name: "Test".into(),
                    path: "/projects/Test.ldaw".into(),
                    tempo_bpm: self.project().tempo_bpm,
                    modified_unix_s: 0,
                    dirty: self.editor.is_dirty(),
                }))
            }
            RequestBody::ProjectList => ok(ReplyBody::Projects { projects: vec![] }),
            RequestBody::ProjectNew { .. } | RequestBody::ProjectClose => {
                *self = RefDaw::empty();
                ok(ReplyBody::Done)
            }
            RequestBody::ProjectOpen { .. } => err(ControlError::NotFound {
                what: "project in the projects folder".into(),
            }),
            RequestBody::ProjectSave => {
                self.editor.mark_saved();
                ok(ReplyBody::Done)
            }
            RequestBody::Edit { edits } => {
                if let Some(s) = self.stale(base) {
                    return s;
                }
                self.token += 1;
                match self.editor.submit(author, None, edits.clone(), self.token) {
                    Ok(Submitted::Applied(a)) => ok(ReplyBody::Applied(Applied {
                        revision: a.revision,
                        created: a.created,
                    })),
                    Ok(Submitted::Queued) => err(ControlError::Busy),
                    Err(f) => err(ControlError::Edit {
                        index: f.index,
                        error: f.error,
                    }),
                }
            }
            RequestBody::NotesList { pattern } => match self.project().pattern(*pattern) {
                Some(p) => ok(ReplyBody::Notes {
                    notes: p.notes.clone(),
                }),
                None => err(ControlError::NotFound {
                    what: format!("content {pattern}"),
                }),
            },
            RequestBody::Play => {
                self.playing = true;
                ok(ReplyBody::Done)
            }
            RequestBody::Stop => {
                self.playing = false;
                ok(ReplyBody::Done)
            }
            RequestBody::TransportState => ok(ReplyBody::Transport {
                playing: self.playing,
                tick: if self.playing { 1920 } else { 0 },
                tempo_bpm: self.project().tempo_bpm,
                loop_region: self.project().loop_region,
            }),
            RequestBody::Undo | RequestBody::Redo => {
                let r = if matches!(inc.request.body, RequestBody::Undo) {
                    self.editor.undo(&scope)
                } else {
                    self.editor.redo(&scope)
                };
                match r {
                    Ok(()) => self.applied_now(),
                    Err(HistoryError::GestureOpen) => err(ControlError::Busy),
                    Err(e) => bad(e.to_string()),
                }
            }
            RequestBody::History => ok(ReplyBody::History {
                entries: self.editor.history().infos(),
            }),
            RequestBody::HistoryTree { since, limit } => ok(ReplyBody::HistoryTree {
                head: self.editor.head_name(),
                nodes: self.editor.history_nodes(since.as_deref(), *limit as usize),
            }),
            RequestBody::HistoryDiff { from, to } => match self.editor.diff(from, to) {
                Ok(lines) => ok(ReplyBody::HistoryDiff { lines }),
                Err(e) => branch_err(e),
            },
            RequestBody::VersionSave { name } => match self.editor.save_version(name) {
                Ok(_) => ok(ReplyBody::Done),
                Err(e) => branch_err(e),
            },
            RequestBody::BranchCreate { name, from } => {
                match self.editor.create_branch(&author, name, from.as_deref()) {
                    Ok(_) => ok(self.branches()),
                    Err(e) => branch_err(e),
                }
            }
            RequestBody::BranchSwitch { branch } => {
                if let Some(s) = self.stale(base) {
                    return s;
                }
                match self.editor.switch_branch(branch) {
                    Ok(()) => self.applied_now(),
                    Err(e) => branch_err(e),
                }
            }
            RequestBody::BranchList => ok(self.branches()),
            RequestBody::BranchRename { branch, name } => {
                match self.editor.rename_branch(branch, name) {
                    Ok(()) => ok(ReplyBody::Done),
                    Err(e) => branch_err(e),
                }
            }
            RequestBody::BranchArchive { branch } => match self.editor.archive_branch(branch) {
                Ok(()) => ok(ReplyBody::Done),
                Err(e) => branch_err(e),
            },
            RequestBody::VersionRestore { commit } => {
                if let Some(s) = self.stale(base) {
                    return s;
                }
                match self.editor.restore_version(author, commit) {
                    Ok(a) => ok(ReplyBody::Applied(Applied {
                        revision: a.revision,
                        created: a.created,
                    })),
                    Err(e) => branch_err(e),
                }
            }
            RequestBody::ExportWav { .. } | RequestBody::Analyze { .. } => {
                let kind = if matches!(inc.request.body, RequestBody::Analyze { .. }) {
                    JobKind::Analyze
                } else {
                    JobKind::Export
                };
                self.jobs.push((kind, 0));
                ok(ReplyBody::Job {
                    job: self.jobs.len() as u64,
                    revision: self.revision(),
                })
            }
            RequestBody::JobStatus { job } if *job == control::hum::JOB => {
                if self.hum_asked.is_empty() {
                    return err(ControlError::NotFound { what: "job".into() });
                }
                let state = match (&self.hum_reply, &self.hum_applied) {
                    (HumReply::Decline, _) => JobState::Cancelled,
                    (HumReply::Notes(_), Some(_)) => JobState::Done,
                    (HumReply::Notes(notes), None) => {
                        let notes = notes.clone();
                        let made = self.user_edit(vec![Edit::AddClip {
                            instrument: ChannelId(1),
                            pattern: None,
                            start: 3840,
                            len: 3840,
                        }]);
                        self.user_edit(vec![Edit::AddNotes {
                            pattern: protocol::ids::PatternId(made.created[0]),
                            notes,
                        }]);
                        self.hum_applied = Some(made);
                        JobState::Running
                    }
                };
                ok(ReplyBody::JobStatus {
                    job: *job,
                    state,
                    progress: 0.0,
                })
            }
            RequestBody::JobResult { job } if *job == control::hum::JOB => {
                match &self.hum_applied {
                    Some(a) => ok(ReplyBody::Applied(a.clone())),
                    None => bad("the hum has not finished"),
                }
            }
            RequestBody::JobStatus { job } => {
                match self.jobs.get_mut((*job as usize).wrapping_sub(1)) {
                    Some((_, polls)) => {
                        *polls += 1;
                        let (state, progress) = if *polls >= 3 {
                            (JobState::Done, 1.0)
                        } else {
                            (JobState::Running, *polls as f32 * 0.3)
                        };
                        ok(ReplyBody::JobStatus {
                            job: *job,
                            state,
                            progress,
                        })
                    }
                    None => err(ControlError::NotFound { what: "job".into() }),
                }
            }
            RequestBody::JobResult { job } => {
                match self.jobs.get((*job as usize).wrapping_sub(1)) {
                    Some((JobKind::Export, _)) => ok(ReplyBody::Exported {
                        path: "/exports/Test.wav".into(),
                    }),
                    Some((JobKind::Analyze, _)) => {
                        let frames: Vec<[f32; 2]> = (0..48_000)
                            .map(|i| {
                                let s = (i as f32 * 0.05).sin() * 0.25;
                                [s, s]
                            })
                            .collect();
                        let mut a = control::analysis::analyze(&frames, 48_000, &[]);
                        a.revision = self.revision();
                        ok(ReplyBody::Analysis(a))
                    }
                    None => err(ControlError::NotFound { what: "job".into() }),
                }
            }
            RequestBody::JobCancel { .. } => ok(ReplyBody::Done),
            RequestBody::SettingsGet => ok(ReplyBody::Settings(Settings {
                audio_device: "Default".into(),
                audio_devices: vec!["Default".into()],
                buffer_size: BufferSize::F256,
                sample_rate: 48_000,
                theme: Theme::System,
                metronome_enabled: false,
            })),
            RequestBody::Seek { .. } => ok(ReplyBody::Done),
            RequestBody::SettingsSet { .. } | RequestBody::PluginScan => ok(ReplyBody::Done),
            RequestBody::PluginList => ok(ReplyBody::Plugins { plugins: vec![] }),
            RequestBody::SetActivity { text, focus } => {
                if let Some(p) = text.as_deref().and_then(control::hum::decode) {
                    self.hum_asked.push(p);
                    self.hum_applied = None;
                }
                self.activity.push((text.clone(), *focus));
                ok(ReplyBody::Done)
            }
            RequestBody::SoundSearch { .. } => ok(ReplyBody::Sounds {
                sounds: vec![SoundInfo {
                    id: "k1".into(),
                    name: "Big\nKick".into(),
                    role: "kick".into(),
                    genres: vec!["trap".into()],
                    tags: vec![],
                    pack: "core".into(),
                    kit: Some("trap-kit".into()),
                    source: "Your Folder".into(),
                    kind: "single sound".into(),
                    kit_name: Some("Trap Kit".into()),
                }],
                total: 99,
                notes: vec![],
            }),
            RequestBody::KitAdd { kit, track, .. } | RequestBody::SoundAdd { id: kit, track } => {
                if let Some(s) = self.stale(base) {
                    return s;
                }
                // The ui knows the counter, so it can name the new track.
                let mut edits = Vec::new();
                let track = match track {
                    Some(t) => *t,
                    None => {
                        edits.push(Edit::AddTrack { name: kit.clone() });
                        TrackId(self.editor.document().next_id)
                    }
                };
                for piece in ["Kick", "Snare", "Hat"] {
                    edits.push(Edit::AddChannel {
                        name: piece.into(),
                        instrument: NewInstrument::Sampler {
                            sample: None,
                            mode: SampleMode::OneShot,
                        },
                        root_key: 60,
                        track,
                    });
                }
                self.token += 1;
                match self
                    .editor
                    .submit(author, Some("Add kit"), edits, self.token)
                {
                    Ok(Submitted::Applied(a)) => ok(ReplyBody::Applied(Applied {
                        revision: a.revision,
                        created: a.created,
                    })),
                    Ok(Submitted::Queued) => err(ControlError::Busy),
                    Err(f) => err(ControlError::Edit {
                        index: f.index,
                        error: f.error,
                    }),
                }
            }
        }
    }
}

pub fn handler(daw: Arc<Mutex<RefDaw>>) -> impl FnMut(&Incoming) -> Action + Send + 'static {
    move |inc| {
        let mut d = daw.lock().unwrap();
        // Restoring a version is PRIVILEGED for agents (17.1).
        if matches!(inc.request.body, RequestBody::VersionRestore { .. })
            && inc.client.transport == Transport::Agent
        {
            let then = d.answer(inc);
            return Action::Approve {
                summary: "restore an older version".into(),
                allow: true,
                then,
            };
        }
        Action::Reply(d.answer(inc))
    }
}

// ---- rig -------------------------------------------------------------------

pub struct Rig {
    pub ui: FakeUi,
    pub daw: Arc<Mutex<RefDaw>>,
    pub socket: PathBuf,
}

impl Rig {
    /// The user edits in the window; subscribed agents hear about it.
    pub fn user_edit(&self, edits: Vec<Edit>) -> Applied {
        let a = self.daw.lock().unwrap().user_edit(edits);
        self.ui.server().notify_revision(a.revision);
        a
    }

    pub fn project(&self) -> Arc<protocol::model::Project> {
        self.daw.lock().unwrap().project()
    }

    pub fn revision(&self) -> u64 {
        self.daw.lock().unwrap().revision()
    }

    /// Every edit batch that reached the UI.
    pub fn edit_batches(&self) -> Vec<Vec<Edit>> {
        self.ui
            .requests()
            .into_iter()
            .filter_map(|q| match q.body {
                RequestBody::Edit { edits } => Some(edits),
                _ => None,
            })
            .collect()
    }
}

/// A temp dir unique to this test process and call.
pub fn temp_dir(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("ldaw-{tag}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

pub fn rig_with(daw: RefDaw, agents: bool, tweak: impl FnOnce(&mut ControlConfig)) -> Rig {
    let mut cfg = ControlConfig::new(temp_dir("mcp").join("libredaw"));
    cfg.agents_enabled = agents;
    cfg.agents_wait = Duration::from_millis(300);
    cfg.approval_timeout = Duration::from_secs(2);
    tweak(&mut cfg);
    let server = ControlServer::start(cfg).expect("server");
    let socket = server.socket_path().to_path_buf();
    let daw = Arc::new(Mutex::new(daw));
    Rig {
        ui: FakeUi::start(server, handler(Arc::clone(&daw))),
        daw,
        socket,
    }
}

pub fn rig(agents: bool, tweak: impl FnOnce(&mut ControlConfig)) -> Rig {
    rig_with(RefDaw::kick_and_hat(), agents, tweak)
}

// ---- a raw MCP client -------------------------------------------------------

pub struct Mcp {
    pub w: UnixStream,
    pub r: BufReader<UnixStream>,
    next: u64,
    /// Notifications and server requests seen while waiting for replies.
    pub other: Vec<Value>,
}

impl Mcp {
    pub fn connect(rig: &Rig) -> Mcp {
        let s = UnixStream::connect(&rig.socket).expect("connect");
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        Mcp {
            r: BufReader::new(s.try_clone().unwrap()),
            w: s,
            next: 1,
            other: Vec::new(),
        }
    }

    pub fn send(&mut self, v: &Value) {
        writeln!(self.w, "{v}").unwrap();
    }

    pub fn read(&mut self) -> Option<Value> {
        let mut line = String::new();
        match self.r.read_line(&mut line) {
            Ok(0) | Err(_) => None,
            Ok(_) => Some(serde_json::from_str(&line).expect("json line")),
        }
    }

    /// Sends a request and returns the whole response message.
    pub fn raw(&mut self, method: &str, params: Value) -> Value {
        let id = self.next;
        self.next += 1;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        loop {
            let m = self.read().expect("connection closed while waiting");
            if m.get("id") == Some(&json!(id)) && m.get("method").is_none() {
                return m;
            }
            self.other.push(m);
        }
    }

    pub fn rpc(&mut self, method: &str, params: Value) -> Value {
        let m = self.raw(method, params);
        assert!(m.get("error").is_none(), "{method}: {m}");
        m["result"].clone()
    }

    pub fn init_with(&mut self, caps: Value) -> Value {
        let r = self.raw(
            "initialize",
            json!({"protocolVersion": PROTOCOL_VERSION, "capabilities": caps,
                   "clientInfo": {"name": "test-agent", "version": "1"}}),
        );
        self.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        r
    }

    pub fn init(&mut self) -> Value {
        let r = self.init_with(json!({}));
        assert!(r.get("error").is_none(), "{r}");
        r["result"].clone()
    }

    pub fn tool(&mut self, name: &str, args: Value) -> Value {
        self.rpc("tools/call", json!({"name": name, "arguments": args}))
    }

    /// The structured result of a tool call that must succeed.
    pub fn ok(&mut self, name: &str, args: Value) -> Value {
        let r = self.tool(name, args);
        assert_eq!(r["isError"], json!(false), "{name}: {r}");
        r["structuredContent"].clone()
    }

    /// The message of a tool call that must fail.
    pub fn err(&mut self, name: &str, args: Value) -> String {
        let r = self.tool(name, args);
        assert_eq!(r["isError"], json!(true), "{name}: {r}");
        r["structuredContent"]["message"]
            .as_str()
            .unwrap_or("")
            .to_string()
    }

    /// Waits for a notification with `method` (also searches earlier ones).
    pub fn wait_notification(&mut self, method: &str) -> Value {
        if let Some(p) = self.other.iter().position(|m| m["method"] == method) {
            return self.other.remove(p);
        }
        loop {
            let m = self.read().expect("connection closed");
            if m["method"] == method {
                return m;
            }
            self.other.push(m);
        }
    }

    pub fn no_notification_soon(&mut self, method: &str) {
        self.r
            .get_ref()
            .set_read_timeout(Some(Duration::from_millis(300)))
            .unwrap();
        let mut line = String::new();
        while let Ok(n) = self.r.read_line(&mut line) {
            if n == 0 {
                break;
            }
            let m: Value = serde_json::from_str(&line).unwrap();
            assert_ne!(m["method"], method, "unexpected {m}");
            self.other.push(m);
            line.clear();
        }
        self.r
            .get_ref()
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
    }

    /// Waits for a request the server sends (sampling).
    pub fn wait_server_request(&mut self, method: &str) -> Value {
        if let Some(p) = self.other.iter().position(|m| m["method"] == method) {
            return self.other.remove(p);
        }
        loop {
            let m = self.read().expect("closed");
            if m["method"] == method && m.get("id").is_some() {
                return m;
            }
            self.other.push(m);
        }
    }
}

pub fn text_of(r: &Value) -> String {
    r["content"][0]["text"].as_str().unwrap_or("").to_string()
}

pub fn wait_for<T>(mut f: impl FnMut() -> Option<T>) -> T {
    let end = Instant::now() + Duration::from_secs(5);
    while Instant::now() < end {
        if let Some(v) = f() {
            return v;
        }
        thread::sleep(Duration::from_millis(5));
    }
    panic!("timed out");
}
