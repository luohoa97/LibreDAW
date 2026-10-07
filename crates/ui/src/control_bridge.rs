// SPDX-License-Identifier: GPL-3.0-or-later
//! The `ui` side of the control socket (SPEC 16, 17.1): every request the
//! `control` crate hands over is answered here with access to the session.
//! Scripts and agents edit through the same `Session::submit` as the user,
//! under their own author, so their changes are undoable commits.
//!
//! Rules enforced here: PRIVILEGED requests (replacing an unsaved project,
//! an agent loading a plugin that was not approved yet) wait for a human
//! click in the window; a request that arrives while a gesture is open is
//! deferred; edits with an out-of-date `base_revision` are `Stale`; agents
//! only undo their own commits; exports go to a folder the client cannot
//! choose.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use control::{ClientInfo, ControlConfig, ControlServer, Incoming, Ticket, UiEvent};
use protocol::control::{
    Analysis, BufferSize, ControlError, JobState, Outcome, PluginInfo, ProjectInfo, ReplyBody,
    Request, RequestBody, Setting, Settings, Theme, Transport, WavFormat, agent_string,
};
use protocol::edit::{Edit, NewInstrument};

use crate::app::{App, UiCommand};
use crate::engine_adapter::{self, RenderJob};
use crate::files;
use crate::settings::{BUFFER_SIZES, ColorScheme};
use doc::history::{Author, Done, EditFailure, HistoryError, Scope, Submitted, describe_edit};

/// One request waiting for the human.
#[derive(Clone, Debug)]
pub struct Approval {
    pub ticket: Ticket,
    pub summary: String,
    pub client: String,
    pub since: Instant,
}

/// One entry of the activity list (docs/ui-design.md 3.9).
#[derive(Clone, Debug)]
pub struct Activity {
    pub text: String,
    pub author: String,
    pub unix_s: u64,
}

/// What the banner, the indicator, and the Agent page show.
#[derive(Clone, Debug, Default)]
pub struct AgentUi {
    pub enabled: bool,
    pub pending: Vec<Approval>,
    pub recent: Vec<Activity>,
    pub clients: Vec<ClientInfo>,
    /// An agent tried to connect while control was off.
    pub wants_control: bool,
    /// The ticket of an approval that timed out last (for the toast).
    pub timed_out: bool,
}

struct Job {
    state: JobState,
    progress: Arc<AtomicU32>,
    cancel: Arc<AtomicBool>,
    result: Option<Result<ReplyBody, String>>,
}

pub struct Bridge {
    server: Rc<ControlServer>,
    held: HashMap<Ticket, Incoming>,
    /// Queued batches: token to (ticket, who).
    queued: HashMap<u64, (Ticket, Author)>,
    next_token: u64,
    approved_plugins: HashSet<String>,
    jobs: Rc<RefCell<HashMap<u64, Job>>>,
    next_job: u64,
    pub ui: AgentUi,
}

fn ok(body: ReplyBody) -> Outcome {
    Outcome::Ok { body }
}

fn err(error: ControlError) -> Outcome {
    Outcome::Err { error }
}

fn bad(reason: &str) -> Outcome {
    err(ControlError::BadRequest {
        reason: reason.into(),
    })
}

/// The author a client's commits carry.
pub fn author_of(c: &ClientInfo) -> Author {
    match c.transport {
        Transport::Script => Author::Script,
        Transport::Agent => {
            let name: String = c
                .name
                .chars()
                .filter(|ch| ch.is_alphanumeric() || *ch == '-' || *ch == '_')
                .take(24)
                .collect();
            Author::Agent(if name.is_empty() {
                format!("agent-{}", c.id)
            } else {
                format!("{name}-{}", c.id)
            })
        }
    }
}

fn scope_of(a: &Author) -> Scope {
    match a {
        Author::User => Scope::Any,
        other => Scope::Only(other.clone()),
    }
}

/// The wire form of an applied batch.
fn wire(a: doc::history::Applied) -> protocol::edit::Applied {
    protocol::edit::Applied {
        revision: a.revision,
        created: a.created,
    }
}

fn map_failure(f: EditFailure) -> ControlError {
    ControlError::Edit {
        index: f.index,
        error: f.error,
    }
}

/// Plugin ids an edit batch would load.
pub fn plugins_in(edits: &[Edit]) -> Vec<String> {
    let mut out = Vec::new();
    for e in edits {
        match e {
            Edit::AddChannel {
                instrument: NewInstrument::Clap { plugin_id },
                ..
            }
            | Edit::AddInsert { plugin_id, .. } => {
                if !out.contains(plugin_id) {
                    out.push(plugin_id.clone());
                }
            }
            _ => {}
        }
    }
    out
}

/// The edits that decide staleness: a request based on an older revision is
/// stale when anything changed since. (A finer rule needs the project of
/// that revision; the strict one is safe.)
pub fn is_stale(base: Option<u64>, current: u64) -> bool {
    base.is_some_and(|b| b < current)
}

/// Starts the server. `None` and a message if it cannot start (another
/// LibreDAW owns the socket, or there is no runtime dir).
pub fn start(agent_request: bool) -> (Option<Bridge>, Option<String>) {
    let Some(dir) = ControlConfig::default_dir() else {
        return (
            None,
            Some("No runtime directory: agent control is off".into()),
        );
    };
    start_in(dir, agent_request)
}

/// As `start`, with the socket directory given (tests use a temp dir).
pub fn start_in(dir: PathBuf, agent_request: bool) -> (Option<Bridge>, Option<String>) {
    let mut cfg = ControlConfig::new(dir);
    cfg.agent_request = agent_request;
    match ControlServer::start(cfg) {
        Ok(s) => {
            let b = Bridge {
                server: Rc::new(s),
                held: HashMap::new(),
                queued: HashMap::new(),
                next_token: 1,
                approved_plugins: HashSet::new(),
                jobs: Rc::new(RefCell::new(HashMap::new())),
                next_job: 1,
                ui: AgentUi {
                    wants_control: agent_request,
                    ..AgentUi::default()
                },
            };
            (Some(b), None)
        }
        Err(e) => (None, Some(e.to_string())),
    }
}

impl Bridge {
    pub fn set_enabled(&mut self, on: bool) {
        self.server.set_agents_enabled(on);
        self.ui.enabled = on;
        if on {
            self.ui.wants_control = false;
        }
    }

    pub fn server_clients(&self) -> Vec<ClientInfo> {
        self.server.clients()
    }

    pub fn enabled(&self) -> bool {
        self.server.agents_enabled()
    }

    pub fn shutdown(self) {
        if let Ok(s) = Rc::try_unwrap(self.server) {
            s.shutdown();
        }
    }
}

fn changed(app: &App) {
    app.command(UiCommand::AgentChanged);
}

/// Called from the 10 ms tick.
pub fn tick(app: &Rc<App>) {
    let polled = {
        let b = app.bridge.borrow();
        match b.as_ref() {
            Some(b) => b.server.poll(),
            None => return,
        }
    };
    let mut touched = !polled.events.is_empty();
    for ev in polled.events {
        on_event(app, ev);
    }
    for inc in polled.requests {
        touched = true;
        handle(app, inc);
    }
    if touched {
        refresh_clients(app);
        changed(app);
    }
}

fn refresh_clients(app: &App) {
    if let Some(b) = app.bridge.borrow_mut().as_mut() {
        b.ui.clients = b.server.clients();
        b.ui.enabled = b.server.agents_enabled();
    }
}

fn on_event(app: &Rc<App>, ev: UiEvent) {
    let mut toast = None;
    {
        let mut guard = app.bridge.borrow_mut();
        let Some(b) = guard.as_mut() else { return };
        match ev {
            UiEvent::AgentRequestedControl { .. } => b.ui.wants_control = true,
            UiEvent::ApprovalNeeded { ticket, summary } => {
                let client = b
                    .held
                    .get(&ticket)
                    .map(|i| agent_string(&i.client.name))
                    .unwrap_or_default();
                b.ui.pending.push(Approval {
                    ticket,
                    summary: agent_string(&summary),
                    client,
                    since: Instant::now(),
                });
            }
            UiEvent::ApprovalTimedOut { ticket } => {
                b.ui.pending.retain(|a| a.ticket != ticket);
                b.held.remove(&ticket);
                toast = Some("Agent request timed out");
            }
            UiEvent::DeferTimedOut { ticket } | UiEvent::TicketCancelled { ticket } => {
                b.queued.retain(|_, (t, _)| *t != ticket);
                b.ui.pending.retain(|a| a.ticket != ticket);
                b.held.remove(&ticket);
            }
            UiEvent::ClientConnected(_) | UiEvent::ClientGone { .. } => {}
        }
    }
    if let Some(t) = toast {
        app.toast(t);
    }
}

/// The human clicked Allow or Deny on a waiting request.
pub fn decide(app: &Rc<App>, ticket: Ticket, allow: bool) {
    let held = {
        let mut guard = app.bridge.borrow_mut();
        let Some(b) = guard.as_mut() else { return };
        b.ui.pending.retain(|a| a.ticket != ticket);
        let ok = b.server.approval(ticket, allow);
        let held = b.held.remove(&ticket);
        if allow && ok { held } else { None }
    };
    if let Some(inc) = held {
        // Plugins named in an approved batch are approved from now on.
        if let RequestBody::Edit { edits } = &inc.request.body
            && let Some(b) = app.bridge.borrow_mut().as_mut()
        {
            for p in plugins_in(edits) {
                b.approved_plugins.insert(p);
            }
        }
        run(app, inc);
    }
    changed(app);
}

fn reply(app: &App, ticket: Ticket, outcome: Outcome) {
    if let Some(b) = app.bridge.borrow().as_ref() {
        b.server.reply(ticket, outcome);
    }
}

/// First look at a request: PRIVILEGED ones wait for the human.
fn handle(app: &Rc<App>, inc: Incoming) {
    let dirty = app.is_dirty();
    let summary = if inc.request.body.privileged(dirty) {
        Some(
            match &inc.request.body {
                RequestBody::ProjectNew { .. } => {
                    "start a new project and replace your unsaved work"
                }
                _ => "open another project and replace your unsaved work",
            }
            .to_string(),
        )
    } else if let (Transport::Agent, RequestBody::Edit { edits }) =
        (inc.client.transport, &inc.request.body)
    {
        let unknown: Vec<String> = {
            let b = app.bridge.borrow();
            let approved = b.as_ref().map(|b| &b.approved_plugins);
            plugins_in(edits)
                .into_iter()
                .filter(|p| !approved.is_some_and(|a| a.contains(p)))
                .collect()
        };
        unknown.first().map(|id| {
            let name = app
                .session
                .borrow()
                .registry
                .find_desc(id)
                .map(|d| d.name.clone())
                .unwrap_or_else(|| id.clone());
            format!("add the plugin {name}")
        })
    } else {
        None
    };
    match summary {
        Some(s) => {
            let ticket = inc.ticket;
            let mut guard = app.bridge.borrow_mut();
            if let Some(b) = guard.as_mut() {
                b.held.insert(ticket, inc);
                b.server.require_approval(ticket, s);
            }
        }
        None => run(app, inc),
    }
}

/// Runs a request that may run now.
fn run(app: &Rc<App>, inc: Incoming) {
    let Incoming {
        ticket,
        client,
        request,
    } = inc;
    let author = author_of(&client);
    if let Some(outcome) = execute(app, ticket, &author, request) {
        reply(app, ticket, outcome);
    }
}

fn revision(app: &App) -> u64 {
    app.session.borrow().document().revision
}

fn project_info(app: &App) -> ProjectInfo {
    let path = app.ui.borrow().path.clone();
    let tempo = app.session.borrow().document().project.tempo_bpm;
    ProjectInfo {
        name: agent_string(&files::display_name(&path)),
        path: path
            .as_ref()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default(),
        tempo_bpm: tempo,
        modified_unix_s: path.as_deref().map(mtime).unwrap_or(0),
        dirty: app.is_dirty(),
    }
}

fn mtime(p: &std::path::Path) -> u64 {
    std::fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Tempo of a saved project without loading it: the `tempo_bpm` line of
/// its `project.toml`.
pub fn tempo_from_text(text: &str) -> Option<f64> {
    text.lines().find_map(|l| {
        let (k, v) = l.split_once('=')?;
        (k.trim() == "tempo_bpm").then(|| v.trim().parse().ok())?
    })
}

fn list_projects(app: &App) -> Vec<ProjectInfo> {
    let root = app.dirs.projects();
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(&root) else {
        return out;
    };
    for e in rd.flatten().take(200) {
        let p = e.path();
        if !doc::persist::is_bundle(&p) {
            continue;
        }
        let tempo = std::fs::read_to_string(p.join(doc::bundle::PROJECT_FILE))
            .ok()
            .and_then(|t| tempo_from_text(&t))
            .unwrap_or(0.0);
        out.push(ProjectInfo {
            name: agent_string(&files::display_name(&Some(p.clone()))),
            path: p.to_string_lossy().to_string(),
            tempo_bpm: tempo,
            modified_unix_s: mtime(&p),
            dirty: false,
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Whether `path` is inside the projects folder (no `..` tricks).
pub fn inside(root: &std::path::Path, path: &std::path::Path) -> bool {
    let (Ok(r), Ok(p)) = (root.canonicalize(), path.canonicalize()) else {
        return false;
    };
    p.starts_with(r)
}

fn settings(app: &App) -> Settings {
    let st = app.settings.borrow();
    let devices = engine_adapter::devices(engine_adapter::Host::PipeWire);
    Settings {
        audio_device: st
            .output_device
            .clone()
            .unwrap_or_else(|| "System Default".into()),
        audio_devices: devices,
        buffer_size: match st.buffer_frames {
            64 => BufferSize::F64,
            128 => BufferSize::F128,
            256 => BufferSize::F256,
            1024 => BufferSize::F1024,
            _ => BufferSize::F512,
        },
        sample_rate: app.session.borrow().link.sample_rate().round() as u32,
        theme: match st.color_scheme {
            ColorScheme::System => Theme::System,
            ColorScheme::Light => Theme::Light,
            ColorScheme::Dark => Theme::Dark,
        },
        metronome_enabled: app.session.borrow().document().project.metronome.enabled,
    }
}

fn set_setting(app: &Rc<App>, s: Setting, author: &Author) -> Outcome {
    match s {
        Setting::AudioDevice(name) => {
            let list = engine_adapter::devices(engine_adapter::Host::PipeWire);
            if name != "System Default" && !list.contains(&name) {
                return err(ControlError::NotFound {
                    what: "audio device".into(),
                });
            }
            app.settings.borrow_mut().output_device = (name != "System Default").then_some(name);
        }
        Setting::BufferSize(b) => {
            let f = b.frames();
            if !BUFFER_SIZES.contains(&f) {
                return bad("unsupported buffer size");
            }
            app.settings.borrow_mut().buffer_frames = f;
        }
        Setting::Theme(t) => {
            let c = match t {
                Theme::System => ColorScheme::System,
                Theme::Light => ColorScheme::Light,
                Theme::Dark => ColorScheme::Dark,
            };
            app.settings.borrow_mut().color_scheme = c;
            crate::prefs::apply_color_scheme(c);
        }
        Setting::MetronomeEnabled(on) => {
            let gain = app.session.borrow().document().project.metronome.gain_db;
            let r = app.session.borrow_mut().submit(
                author.clone(),
                Some("Metronome"),
                vec![Edit::SetMetronome {
                    enabled: on,
                    gain_db: gain,
                }],
                0,
            );
            if let Err(f) = r {
                return err(map_failure(f));
            }
            app.notify();
        }
    }
    let _ = app.settings.borrow().write(&app.dirs);
    ok(ReplyBody::Done)
}

fn note_activity(app: &App, author: &Author, edits: &[Edit]) {
    let text = match edits {
        [] => return,
        [one] => describe_edit(one),
        [first, rest @ ..] => format!("{} and {} more", describe_edit(first), rest.len()),
    };
    if let Some(b) = app.bridge.borrow_mut().as_mut() {
        b.ui.recent.insert(
            0,
            Activity {
                text: agent_string(&text),
                author: author.tag(),
                unix_s: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
            },
        );
        b.ui.recent.truncate(30);
    }
}

/// Runs one request. Returns the outcome now, or `None` when the answer
/// comes later (a queued batch, a job step, a save).
fn execute(app: &Rc<App>, ticket: Ticket, author: &Author, req: Request) -> Option<Outcome> {
    let Request {
        base_revision,
        body,
        ..
    } = req;
    Some(match body {
        RequestBody::ProjectGet => {
            let s = app.session.borrow();
            ok(ReplyBody::Project {
                revision: s.document().revision,
                project: s.document().project.clone(),
            })
        }
        RequestBody::ProjectInfo => ok(ReplyBody::ProjectInfo(project_info(app))),
        RequestBody::ProjectList => ok(ReplyBody::Projects {
            projects: list_projects(app),
        }),
        RequestBody::ProjectNew { .. } => {
            files::fresh_project(app);
            ok(ReplyBody::Done)
        }
        RequestBody::ProjectOpen { path } => {
            let p = PathBuf::from(&path);
            if !inside(&app.dirs.projects(), &p) {
                return Some(err(ControlError::NotFound {
                    what: "project in the projects folder".into(),
                }));
            }
            files::open_path(app, p);
            ok(ReplyBody::Done)
        }
        RequestBody::ProjectSave => {
            let (a2, server) = (app.clone(), app.bridge.borrow().as_ref()?.server.clone());
            files::save_current(&a2, move |r| {
                let o = match r {
                    Ok(_) => ok(ReplyBody::Done),
                    Err(e) => err(ControlError::Internal { reason: e }),
                };
                server.reply(ticket, o);
            });
            return None;
        }
        RequestBody::Edit { edits } => {
            let current = revision(app);
            if is_stale(base_revision, current) {
                return Some(err(ControlError::Stale { current }));
            }
            let token = {
                let mut guard = app.bridge.borrow_mut();
                let b = guard.as_mut()?;
                let t = b.next_token;
                b.next_token += 1;
                t
            };
            let r = app
                .session
                .borrow_mut()
                .submit(author.clone(), None, edits.clone(), token);
            match r {
                Ok(Submitted::Applied(a)) => {
                    note_activity(app, author, &edits);
                    app.notify();
                    ok(ReplyBody::Applied(wire(a)))
                }
                Ok(Submitted::Queued) => {
                    if let Some(b) = app.bridge.borrow_mut().as_mut() {
                        b.queued.insert(token, (ticket, author.clone()));
                        b.server.defer(ticket);
                    }
                    return None;
                }
                Err(f) => err(map_failure(f)),
            }
        }
        RequestBody::NotesList { pattern, channel } => {
            let s = app.session.borrow();
            match s.document().project.pattern(pattern) {
                Some(p) if s.document().project.channel(channel).is_some() => {
                    ok(ReplyBody::Notes {
                        notes: p.notes_of(channel).to_vec(),
                    })
                }
                _ => err(ControlError::NotFound {
                    what: "pattern or channel".into(),
                }),
            }
        }
        RequestBody::Play => {
            app.play();
            ok(ReplyBody::Done)
        }
        RequestBody::Stop => {
            app.stop();
            ok(ReplyBody::Done)
        }
        RequestBody::SetPlayingPattern { pattern } => {
            if app
                .session
                .borrow()
                .document()
                .project
                .pattern(pattern)
                .is_none()
            {
                return Some(err(ControlError::NotFound {
                    what: "pattern".into(),
                }));
            }
            app.select_pattern(pattern);
            ok(ReplyBody::Done)
        }
        RequestBody::TransportState => {
            let s = app.session.borrow();
            ok(ReplyBody::Transport {
                playing: app.ui.borrow().playing,
                tick: app.playhead_tick(),
                tempo_bpm: s.document().project.tempo_bpm,
                pattern: app.current_pattern(),
            })
        }
        RequestBody::Undo => undo_redo(app, author, true),
        RequestBody::Redo => undo_redo(app, author, false),
        RequestBody::History => ok(ReplyBody::History {
            entries: app.session.borrow().editor.history().infos(),
        }),
        RequestBody::ExportWav {
            pattern,
            loops,
            format,
        } => {
            return Some(start_job(
                app,
                JobTarget::Pattern { pattern, loops },
                Some(format),
            ));
        }
        RequestBody::ExportSongWav {
            format,
            tail_seconds,
        } => {
            return Some(start_job(
                app,
                JobTarget::Song { tail: tail_seconds },
                Some(format),
            ));
        }
        RequestBody::AnalyzeSong => {
            return Some(start_job(app, JobTarget::Song { tail: 2.0 }, None));
        }
        RequestBody::SetTransportMode { mode, loop_song } => {
            app.set_transport_mode(mode, loop_song);
            ok(ReplyBody::Done)
        }
        RequestBody::Analyze { pattern, loops } => {
            return Some(start_job(app, JobTarget::Pattern { pattern, loops }, None));
        }
        RequestBody::JobStatus { job } => {
            let guard = app.bridge.borrow();
            let b = guard.as_ref()?;
            let jobs = b.jobs.borrow();
            match jobs.get(&job) {
                Some(j) => ok(ReplyBody::JobStatus {
                    job,
                    state: j.state,
                    progress: j.progress.load(Ordering::Relaxed).min(100) as f32 / 100.0,
                }),
                None => err(ControlError::NotFound { what: "job".into() }),
            }
        }
        RequestBody::JobResult { job } => {
            let guard = app.bridge.borrow();
            let b = guard.as_ref()?;
            let jobs = b.jobs.borrow();
            match jobs.get(&job).map(|j| (j.state, j.result.clone())) {
                Some((JobState::Done, Some(Ok(body)))) => ok(body),
                Some((_, Some(Err(reason)))) => err(ControlError::Internal { reason }),
                Some(_) => bad("the job has not finished"),
                None => err(ControlError::NotFound { what: "job".into() }),
            }
        }
        RequestBody::JobCancel { job } => {
            let guard = app.bridge.borrow();
            let b = guard.as_ref()?;
            let mut jobs = b.jobs.borrow_mut();
            match jobs.get_mut(&job) {
                Some(j) => {
                    j.cancel.store(true, Ordering::Relaxed);
                    if j.state == JobState::Queued {
                        j.state = JobState::Cancelled;
                    }
                    ok(ReplyBody::Done)
                }
                None => err(ControlError::NotFound { what: "job".into() }),
            }
        }
        RequestBody::SettingsGet => ok(ReplyBody::Settings(settings(app))),
        RequestBody::SettingsSet { setting } => set_setting(app, setting, author),
        RequestBody::PluginScan => {
            let server = app.bridge.borrow().as_ref()?.server.clone();
            let a2 = app.clone();
            app.tasks
                .spawn("plugin-scan", crate::plugin_adapter::scan, move |found| {
                    a2.session.borrow_mut().registry.set_catalog(found);
                    server.reply(ticket, ok(ReplyBody::Done));
                });
            return None;
        }
        RequestBody::PluginList => {
            let approved = app
                .bridge
                .borrow()
                .as_ref()
                .map(|b| b.approved_plugins.clone())
                .unwrap_or_default();
            let s = app.session.borrow();
            ok(ReplyBody::Plugins {
                plugins: s
                    .registry
                    .catalog()
                    .iter()
                    .map(|d| PluginInfo {
                        plugin_id: d.id.clone(),
                        name: agent_string(&d.name),
                        vendor: agent_string(&d.vendor),
                        version: agent_string(&d.version),
                        instrument: d.instrument,
                        effect: d.effect,
                        agent_approved: approved.contains(&d.id),
                    })
                    .collect(),
            })
        }
    })
}

/// Writes finished queued batches back to their clients.
pub fn on_done(app: &Rc<App>, done: Vec<Done>) {
    for d in done {
        let found = app
            .bridge
            .borrow_mut()
            .as_mut()
            .and_then(|b| b.queued.remove(&d.token));
        let Some((ticket, author)) = found else {
            // A batch from somewhere else (not the control socket).
            if let Err(e) = d.result {
                app.toast(&e.to_string());
            }
            continue;
        };
        let outcome = match d.result {
            Ok(a) => {
                note_activity(app, &author, &[]);
                ok(ReplyBody::Applied(wire(a)))
            }
            Err(f) => err(map_failure(f)),
        };
        reply(app, ticket, outcome);
    }
}

/// What an export or analysis job renders.
#[derive(Clone, Copy)]
enum JobTarget {
    Pattern {
        pattern: protocol::ids::PatternId,
        loops: u32,
    },
    /// The whole playlist plus a tail in seconds (15.6).
    Song { tail: f64 },
}

fn start_job(app: &Rc<App>, target: JobTarget, fmt: Option<WavFormat>) -> Outcome {
    let (pattern, loops, song_tail) = match target {
        JobTarget::Pattern { pattern, loops } => {
            if !(1..=64).contains(&loops) {
                return bad("loops must be between 1 and 64");
            }
            (pattern, loops, None)
        }
        JobTarget::Song { tail } => {
            if !(0.0..=30.0).contains(&tail) {
                return bad("tail_seconds must be between 0 and 30");
            }
            (protocol::ids::PatternId(0), 1, Some(tail))
        }
    };
    let (project, slots, rate, rev, store) = {
        let s = app.session.borrow();
        if song_tail.is_none() && s.document().project.pattern(pattern).is_none() {
            return err(ControlError::NotFound {
                what: "pattern".into(),
            });
        }
        if song_tail.is_some()
            && s.document()
                .project
                .playlist
                .iter()
                .all(|t| t.clips.is_empty())
        {
            return bad("the song has no clips");
        }
        (
            s.document().project.clone(),
            s.slots.clone(),
            s.link.sample_rate().round() as u32,
            s.document().revision,
            s.store.clone(),
        )
    };
    let exports = app.dirs.projects().join("exports");
    let name = files::display_name(&app.ui.borrow().path);
    let (id, jobs, progress, cancel) = {
        let mut guard = app.bridge.borrow_mut();
        let Some(b) = guard.as_mut() else {
            return bad("control is off");
        };
        let id = b.next_job;
        b.next_job += 1;
        let (progress, cancel) = (
            Arc::new(AtomicU32::new(0)),
            Arc::new(AtomicBool::new(false)),
        );
        b.jobs.borrow_mut().insert(
            id,
            Job {
                state: JobState::Running,
                progress: progress.clone(),
                cancel: cancel.clone(),
                result: None,
            },
        );
        (id, b.jobs.clone(), progress, cancel)
    };
    let (p2, c2) = (progress.clone(), cancel.clone());
    let jobs2 = jobs.clone();
    app.tasks.spawn(
        "control-job",
        move || -> Result<ReplyBody, String> {
            let frames = engine_adapter::render(
                RenderJob {
                    project,
                    pattern,
                    loops,
                    sample_rate: rate,
                    song_tail,
                    store: Some(store),
                },
                &slots,
                &[],
                &p2,
                &c2,
            )
            .map_err(|e| e.to_string())?;
            if c2.load(Ordering::Relaxed) {
                return Err("cancelled".into());
            }
            match fmt {
                Some(f) => {
                    std::fs::create_dir_all(&exports).map_err(|e| e.to_string())?;
                    let file = exports.join(crate::export::export_name(&format!("{name}-{id}")));
                    engine_adapter::write_wav(&file, &frames, rate, f)
                        .map_err(|e| e.to_string())?;
                    Ok(ReplyBody::Exported {
                        path: file.to_string_lossy().to_string(),
                    })
                }
                None => {
                    let mut a: Analysis = control::analysis::analyze(&frames, rate, &[]);
                    a.revision = rev;
                    Ok(ReplyBody::Analysis(a))
                }
            }
        },
        move |r| {
            if let Some(j) = jobs2.borrow_mut().get_mut(&id) {
                match r {
                    Ok(body) => {
                        j.state = JobState::Done;
                        j.result = Some(Ok(body));
                    }
                    Err(e) if e == "cancelled" => j.state = JobState::Cancelled,
                    Err(e) => {
                        j.state = JobState::Failed;
                        j.result = Some(Err(e));
                    }
                }
                j.progress.store(100, Ordering::Relaxed);
            }
        },
    );
    ok(ReplyBody::Job {
        job: id,
        revision: rev,
    })
}

/// Undo or redo for a client, within its own commits (agents) or any
/// (the user is not a client).
pub fn undo_redo(app: &Rc<App>, author: &Author, undo: bool) -> Outcome {
    if app.session.borrow().editor.gesture_open() {
        return err(ControlError::Busy);
    }
    let scope = scope_of(author);
    let r = if undo {
        app.session.borrow_mut().undo(&scope)
    } else {
        app.session.borrow_mut().redo(&scope)
    };
    match r {
        Ok(()) => {
            app.notify();
            ok(ReplyBody::Done)
        }
        Err(HistoryError::GestureOpen) => err(ControlError::Busy),
        Err(e) => err(ControlError::BadRequest {
            reason: e.to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::ids::{PatternId, TrackId};

    fn client(t: Transport, name: &str) -> ClientInfo {
        ClientInfo {
            id: 7,
            transport: t,
            name: name.into(),
            pid: None,
        }
    }

    #[test]
    fn authors_name_the_session() {
        assert_eq!(author_of(&client(Transport::Script, "x")), Author::Script);
        assert_eq!(
            author_of(&client(Transport::Agent, "claude-code")),
            Author::Agent("claude-code-7".into())
        );
        assert_eq!(
            author_of(&client(Transport::Agent, "../../etc")),
            Author::Agent("etc-7".into())
        );
        assert_eq!(
            author_of(&client(Transport::Agent, "")),
            Author::Agent("agent-7".into())
        );
    }

    #[test]
    fn scopes_keep_agents_to_their_own_commits() {
        assert_eq!(scope_of(&Author::User), Scope::Any);
        assert_eq!(
            scope_of(&Author::Agent("a-1".into())),
            Scope::Only(Author::Agent("a-1".into()))
        );
    }

    #[test]
    fn plugins_named_by_a_batch() {
        let edits = vec![
            Edit::SetTempo { bpm: 100.0 },
            Edit::AddInsert {
                track: TrackId::MASTER,
                index: 0,
                plugin_id: "a.b".into(),
            },
            Edit::AddChannel {
                name: "x".into(),
                instrument: NewInstrument::Clap {
                    plugin_id: "c.d".into(),
                },
                root_key: 60,
                track: TrackId::MASTER,
            },
            Edit::AddInsert {
                track: TrackId::MASTER,
                index: 1,
                plugin_id: "a.b".into(),
            },
        ];
        assert_eq!(
            plugins_in(&edits),
            vec!["a.b".to_string(), "c.d".to_string()]
        );
        assert!(
            plugins_in(&[Edit::RemovePattern {
                pattern: PatternId(1)
            }])
            .is_empty()
        );
    }

    // ---- over the real socket ----

    use crate::engine_adapter::EngineLink;
    use crate::registry::Registry;
    use crate::session::Session;
    use doc::document::Document;
    use doc::persist::Dirs;
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;

    struct Rig {
        app: Rc<App>,
        socket: PathBuf,
        dir: PathBuf,
    }

    impl Drop for Rig {
        fn drop(&mut self) {
            if let Some(b) = self.app.bridge.borrow_mut().take() {
                b.shutdown();
            }
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn rig(name: &str) -> Rig {
        let dir = std::env::temp_dir().join(format!("ldaw-bridge-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dirs = Dirs {
            music: dir.join("music"),
            data: dir.join("data"),
            config: dir.join("config"),
        };
        let session = Session::new(
            Document::new(),
            true,
            EngineLink::stub(48000.0),
            Registry::new(Vec::new(), 48000.0),
        );
        let app = App::with_dirs(session, dirs);
        let (bridge, e) = start_in(dir.join("run"), false);
        let mut bridge = bridge.unwrap_or_else(|| panic!("start: {e:?}"));
        bridge.set_enabled(true);
        let socket = bridge.server.socket_path().to_path_buf();
        *app.bridge.borrow_mut() = Some(bridge);
        Rig { app, socket, dir }
    }

    /// Sends hello and the given request lines as an agent; answers come
    /// back in order while this thread runs the UI tick.
    fn talk(rig: &Rig, transport: &str, requests: Vec<String>) -> Vec<String> {
        let path = rig.socket.clone();
        let transport = transport.to_string();
        let h = std::thread::spawn(move || {
            let mut s = UnixStream::connect(path).unwrap();
            s.set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut r = BufReader::new(s.try_clone().unwrap());
            writeln!(
                s,
                "{{\"hello\":{{\"transport\":\"{transport}\",\"client\":\"test\"}}}}"
            )
            .unwrap();
            let mut hello = String::new();
            r.read_line(&mut hello).unwrap();
            let mut out = vec![hello];
            for q in requests {
                writeln!(s, "{q}").unwrap();
                let mut line = String::new();
                r.read_line(&mut line).unwrap();
                out.push(line);
            }
            out
        });
        let t0 = Instant::now();
        while !h.is_finished() && t0.elapsed().as_secs() < 8 {
            tick(&rig.app);
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        h.join().expect("client thread")
    }

    #[test]
    fn an_agent_reads_and_edits_through_the_socket() {
        let rig = rig("edit");
        let out = talk(
            &rig,
            "agent",
            vec![
                r#"{"id":1,"body":{"op":"project_info"}}"#.into(),
                r#"{"id":2,"body":{"op":"edit","edits":[{"edit":"set_tempo","bpm":97.0}]}}"#.into(),
                r#"{"id":3,"body":{"op":"transport_state"}}"#.into(),
            ],
        );
        assert!(out[0].contains("hello_ok"), "{}", out[0]);
        assert!(out[1].contains("\"status\":\"ok\""), "{}", out[1]);
        assert!(
            out[2].contains("\"status\":\"ok\"") && out[2].contains("applied"),
            "{}",
            out[2]
        );
        assert!(out[3].contains("97"), "{}", out[3]);
        let tempo = rig.app.session.borrow().document().project.tempo_bpm;
        assert_eq!(tempo, 97.0);
        // The change is an agent commit that shows in the activity list.
        let b = rig.app.bridge.borrow();
        assert_eq!(b.as_ref().unwrap().ui.recent.len(), 1);
        assert!(
            b.as_ref().unwrap().ui.recent[0]
                .author
                .starts_with("agent:test-")
        );
    }

    #[test]
    fn stale_edits_are_refused_and_nothing_changes() {
        let rig = rig("stale");
        rig.app.edit(vec![Edit::SetTempo { bpm: 111.0 }]);
        let out = talk(
            &rig,
            "agent",
            vec![
                r#"{"id":1,"base_revision":0,"body":{"op":"edit","edits":[{"edit":"set_tempo","bpm":50.0}]}}"#
                    .into(),
            ],
        );
        assert!(out[1].contains("stale"), "{}", out[1]);
        assert_eq!(rig.app.session.borrow().document().project.tempo_bpm, 111.0);
    }

    #[test]
    fn an_agent_only_undoes_its_own_commits() {
        let rig = rig("undo");
        rig.app.edit(vec![Edit::SetTempo { bpm: 111.0 }]);
        let out = talk(
            &rig,
            "agent",
            vec![
                r#"{"id":1,"body":{"op":"undo"}}"#.into(),
                r#"{"id":2,"body":{"op":"edit","edits":[{"edit":"set_tempo","bpm":90.0}]}}"#.into(),
                r#"{"id":3,"body":{"op":"undo"}}"#.into(),
            ],
        );
        // Nothing of its own to undo yet: the user's tempo stays.
        assert!(out[1].contains("\"status\":\"err\""), "{}", out[1]);
        assert!(out[2].contains("\"status\":\"ok\""));
        assert!(out[3].contains("\"status\":\"ok\""), "{}", out[3]);
        assert_eq!(rig.app.session.borrow().document().project.tempo_bpm, 111.0);
    }

    #[test]
    fn new_plugins_wait_for_the_human() {
        let rig = rig("approve");
        let h_rig_socket = rig.socket.clone();
        let handle = std::thread::spawn(move || {
            let mut s = UnixStream::connect(h_rig_socket).unwrap();
            s.set_read_timeout(Some(std::time::Duration::from_secs(8)))
                .unwrap();
            let mut r = BufReader::new(s.try_clone().unwrap());
            writeln!(s, r#"{{"hello":{{"transport":"agent","client":"test"}}}}"#).unwrap();
            let mut l = String::new();
            r.read_line(&mut l).unwrap();
            writeln!(
                s,
                r#"{{"id":1,"body":{{"op":"edit","edits":[{{"edit":"add_insert","track":0,"index":0,"plugin_id":"x.y"}}]}}}}"#
            )
            .unwrap();
            let mut reply = String::new();
            r.read_line(&mut reply).unwrap();
            reply
        });
        // Run the tick until the approval shows, then deny.
        let t0 = Instant::now();
        let mut ticket = None;
        while ticket.is_none() && t0.elapsed().as_secs() < 5 {
            tick(&rig.app);
            ticket = rig
                .app
                .bridge
                .borrow()
                .as_ref()
                .and_then(|b| b.ui.pending.first().map(|a| a.ticket));
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let ticket = ticket.expect("an approval is pending");
        assert!(
            rig.app.bridge.borrow().as_ref().unwrap().ui.pending[0]
                .summary
                .starts_with("add the plugin")
        );
        decide(&rig.app, ticket, false);
        let reply = handle.join().unwrap();
        assert!(reply.contains("denied"), "{reply}");
        assert!(
            rig.app.session.borrow().document().project.tracks[0]
                .inserts
                .is_empty()
        );
        assert!(
            rig.app
                .bridge
                .borrow()
                .as_ref()
                .unwrap()
                .ui
                .pending
                .is_empty()
        );
    }

    #[test]
    fn scripts_cannot_undo() {
        let rig = rig("script");
        let out = talk(
            &rig,
            "script",
            vec![r#"{"id":1,"body":{"op":"undo"}}"#.into()],
        );
        assert!(out[1].contains("not_allowed"), "{}", out[1]);
    }

    #[test]
    fn staleness() {
        assert!(!is_stale(None, 9));
        assert!(!is_stale(Some(9), 9));
        assert!(is_stale(Some(8), 9));
    }

    #[test]
    fn tempo_is_read_from_the_project_text() {
        assert_eq!(
            tempo_from_text("a = 1\ntempo_bpm = 128.5\nb = 2"),
            Some(128.5)
        );
        assert_eq!(tempo_from_text("tempo_bpm = fast"), None);
        assert_eq!(tempo_from_text("x = 1"), None);
    }

    #[test]
    fn paths_must_stay_inside_the_projects_folder() {
        let dir = std::env::temp_dir().join(format!("ldaw-bridge-{}", std::process::id()));
        let root = dir.join("projects");
        let inner = root.join("a.ldaw");
        let outside = dir.join("other");
        std::fs::create_dir_all(&inner).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        assert!(inside(&root, &inner));
        assert!(!inside(&root, &outside));
        assert!(!inside(&root, &inner.join("../../other")));
        assert!(!inside(&root, &root.join("missing")));
        let _ = std::fs::remove_dir_all(dir);
    }
}
