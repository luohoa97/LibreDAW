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
    Analysis, BufferSize, ControlError, Focus, JobState, MAX_ACTIVITY_CHARS, Outcome, PluginInfo,
    ProjectInfo, ReplyBody, Request, RequestBody, Setting, Settings, Theme, Transport, WavFormat,
    agent_string,
};
use protocol::edit::{Applied, Edit, NewInstrument};

use crate::app::{App, UiCommand};
use crate::engine_adapter::{self, RenderJob};
use crate::files;
use crate::presence;
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
    /// What the entry changed; hovering it glows these (18.1).
    pub focus: Vec<Focus>,
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
    /// What the agent says it is doing now (18.2), if anything.
    pub activity: Option<AgentActivity>,
}

/// An agent's declared activity (SPEC 18.2).
#[derive(Clone, Debug, PartialEq)]
pub struct AgentActivity {
    /// Untrusted text, control characters removed, at most
    /// `MAX_ACTIVITY_CHARS` characters.
    pub text: String,
    pub focus: Option<Focus>,
    /// The author tag of the agent that set it.
    pub author: String,
    pub client: u64,
}

/// Cleans an activity text: no control characters, at most
/// `MAX_ACTIVITY_CHARS` characters, trimmed. `None` when nothing is left.
pub fn activity_text(s: &str) -> Option<String> {
    let t: String = s
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_ACTIVITY_CHARS)
        .collect();
    let t = t.trim().to_string();
    (!t.is_empty()).then_some(t)
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
    /// The last analysis, for the revision, range and tail it was made for
    /// (SPEC 18.7: cached per revision).
    analysis_cache: Rc<RefCell<Option<(AnalysisKey, Analysis)>>>,
    pub ui: AgentUi,
}

/// Revision, range and tail (as bits) of an analysis.
type AnalysisKey = (u64, Option<(u32, u32)>, u64);

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

/// What a script hears in the Flatpak build.
pub const SCRIPTS_OFF_IN_FLATPAK: &str =
    "Scripting is not available in the Flatpak version of Oto. Agents still work.";

/// Whether LibreDAW runs inside a Flatpak sandbox.
pub fn in_flatpak() -> bool {
    std::path::Path::new("/.flatpak-info").exists()
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
                instrument: NewInstrument::Clap { plugin_id, .. },
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
                analysis_cache: Rc::new(RefCell::new(None)),
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
        presence::on_enabled(on);
        self.ui.enabled = on;
        if on {
            self.ui.wants_control = false;
        }
    }

    /// The hard stop (Escape, the pill): no more agent requests, nothing
    /// held or queued for them.
    pub fn stop_agents(&mut self) {
        self.set_enabled(false);
        self.held.clear();
        self.queued.clear();
        self.ui.pending.clear();
        self.ui.activity = None;
    }

    pub fn server_clients(&self) -> Vec<ClientInfo> {
        self.server.clients()
    }

    pub fn enabled(&self) -> bool {
        self.server.agents_enabled()
    }

    pub fn socket_path(&self) -> &std::path::Path {
        self.server.socket_path()
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

/// Stops every agent (the pill's Stop, Escape): see `Bridge::stop_agents`.
pub fn stop_agents(app: &App) {
    if let Some(b) = app.bridge.borrow_mut().as_mut() {
        b.stop_agents();
    }
    changed(app);
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
    if presence::refuse(&inc.client) {
        reply(app, inc.ticket, bad(presence::STOPPED));
        return;
    }
    // Scripts do not run in the Flatpak build (SPEC 19.3); they get a
    // clear answer instead of a silent failure.
    if inc.client.transport == Transport::Script && in_flatpak() {
        reply(app, inc.ticket, bad(SCRIPTS_OFF_IN_FLATPAK));
        return;
    }
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

/// The projects Home shows, as the bridge reports them: unsaved work is
/// not a project yet, so only saved ones are listed (newest first).
fn list_projects(app: &App) -> Vec<ProjectInfo> {
    let open = app.ui.borrow().path.clone();
    let dirty = app.is_dirty();
    crate::home_logic::scan(&app.dirs, &[])
        .recent
        .into_iter()
        .map(|it| {
            let tempo = std::fs::read_to_string(it.path.join(doc::bundle::PROJECT_FILE))
                .ok()
                .and_then(|t| tempo_from_text(&t))
                .unwrap_or(0.0);
            ProjectInfo {
                name: agent_string(&it.name),
                dirty: dirty && open.as_ref() == Some(&it.path),
                path: it.path.to_string_lossy().to_string(),
                tempo_bpm: tempo,
                modified_unix_s: it.modified,
            }
        })
        .collect()
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

fn note_activity(app: &App, author: &Author, edits: &[Edit], created: &[u32]) {
    let text = match edits {
        [] => return,
        [one] => describe_edit(one),
        [first, rest @ ..] => format!("{} and {} more", describe_edit(first), rest.len()),
    };
    let focus = presence::foci_of_edits(edits, created, &app.session.borrow().document().project);
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
                focus,
            },
        );
        b.ui.recent.truncate(30);
    }
}

/// An agent declares (or ends, with `None` text) what it is doing (18.2).
pub fn set_activity(app: &App, author: &Author, text: Option<&str>, focus: Option<Focus>) {
    presence::on_activity(author, text.and_then(activity_text), focus);
    let client = match author {
        Author::Agent(tag) => tag
            .rsplit('-')
            .next()
            .and_then(|n| n.parse().ok())
            .unwrap_or(0),
        _ => 0,
    };
    let new = text.and_then(activity_text).map(|text| AgentActivity {
        text,
        focus,
        author: author.tag(),
        client,
    });
    let mut log = None;
    if let Some(b) = app.bridge.borrow_mut().as_mut() {
        if let Some(a) = &new
            && b.ui.activity.as_ref().map(|o| &o.text) != Some(&a.text)
        {
            log = Some(a.text.clone());
        }
        b.ui.activity = new;
    }
    if let Some(text) = log {
        push_activity(app, author, text, focus.into_iter().collect());
    }
}

fn push_activity(app: &App, author: &Author, text: String, focus: Vec<Focus>) {
    if let Some(b) = app.bridge.borrow_mut().as_mut() {
        b.ui.recent.insert(
            0,
            Activity {
                text,
                author: author.tag(),
                unix_s: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
                focus,
            },
        );
        b.ui.recent.truncate(30);
    }
}

/// The sound library (installed packs and the user's folders).
fn library(app: &App) -> Vec<crate::soundlib::Kit> {
    crate::samples_ui::library(app)
}

/// `KitAdd`: imports the kit's files off the GTK thread, then adds one
/// sampler channel per piece in one undo group by the agent.
fn kit_add(
    app: &Rc<App>,
    ticket: Ticket,
    author: &Author,
    pack: &str,
    kit: &str,
    track: Option<protocol::ids::TrackId>,
) -> Option<Outcome> {
    let kits = library(app);
    let Some(kit) = crate::sound_search::find_kit(&kits, pack, kit).cloned() else {
        return Some(err(ControlError::NotFound { what: "kit".into() }));
    };
    if let Some(t) = track
        && app.session.borrow().document().project.track(t).is_none()
    {
        return Some(err(ControlError::NotFound {
            what: "track".into(),
        }));
    }
    let items = kit
        .pieces
        .iter()
        .map(|p| crate::samples_ui::ImportItem::piece(p, kit.source))
        .collect();
    let target = match track {
        Some(t) => crate::channels::KitTrack::Existing(t),
        None => crate::channels::KitTrack::New(format!("{} Kit", kit.title)),
    };
    let done = format!("Added the {} kit", kit.title);
    let pieces = kit.pieces;
    import_and_add(
        app,
        ticket,
        author,
        items,
        move |i, s| crate::samples_ui::piece_setup(&pieces[i], s),
        target,
        done,
        "Adding a drum kit",
    )
}

/// `SoundSearch`: the catalogue (SPEC 15.3, 18.4), paged. A drum kit's
/// pieces are the sounds whose `kit` is its id, so a search with `kit` set
/// to a kit id lists the kit and its pieces. Messages for the agent (FL
/// Studio off, still being read) go in `notes`.
fn sound_search(app: &Rc<App>, q: crate::sound_catalog::Query) -> ReplyBody {
    use crate::sound_catalog as cat;
    let fl = fl_state(app);
    let entries = catalogue(app, &fl);
    let (page, total) = cat::search(&entries, &q);
    let sounds = page
        .into_iter()
        .map(|e| protocol::control::SoundInfo {
            id: agent_string(&e.id),
            name: agent_string(&e.name),
            role: agent_string(&e.role),
            genres: if e.family.is_empty() {
                Vec::new()
            } else {
                vec![agent_string(&e.family)]
            },
            tags: e.tags.iter().map(|t| agent_string(t)).collect(),
            pack: e.source.to_string(),
            kit: e.kit.as_ref().map(|(id, _)| agent_string(id)),
            source: e.source.to_string(),
            kind: e.kind.to_string(),
            kit_name: e.kit.as_ref().map(|(_, name)| agent_string(name)),
        })
        .collect();
    let asks_fl = q.source.trim().is_empty() || q.source.to_lowercase().contains("fl");
    let notes = if asks_fl {
        cat::fl_note(&fl.status, fl.remembered)
            .map(str::to_string)
            .into_iter()
            .collect()
    } else {
        Vec::new()
    };
    ReplyBody::Sounds {
        sounds,
        total: total as u32,
        notes,
    }
}

struct FlState {
    status: crate::fl_library::Status,
    remembered: bool,
}

/// The FL library as the Sounds pane has it. The choice saved earlier is
/// picked up (and read) the first time an agent asks, without the pane
/// having been opened. An agent never turns the library on.
fn fl_state(app: &Rc<App>) -> FlState {
    use crate::fl_library as fl;
    let remembered = fl::load(&app.dirs.config).is_some();
    if matches!(fl::status(), fl::Status::Off) && remembered {
        crate::fl_browser::resume(app);
    }
    FlState {
        status: fl::status(),
        remembered,
    }
}

fn loaded_of(fl: &FlState) -> Option<std::sync::Arc<crate::fl_library::Loaded>> {
    match &fl.status {
        crate::fl_library::Status::Ready(l) => Some(l.clone()),
        _ => None,
    }
}

fn catalogue(app: &Rc<App>, fl: &FlState) -> Vec<crate::sound_catalog::Entry> {
    let kits = library(app);
    let loaded = loaded_of(fl);
    let a = app.clone();
    crate::sound_catalog::build(
        &kits,
        &move |s| crate::sound_picker::available(&a, s),
        loaded.as_deref(),
    )
}

/// Imports `items` off the GTK thread, then adds one sampler channel per
/// file (`make` builds each setup) in one undo group by the agent.
#[allow(clippy::too_many_arguments)]
fn import_and_add(
    app: &Rc<App>,
    ticket: Ticket,
    author: &Author,
    items: Vec<crate::samples_ui::ImportItem>,
    make: impl Fn(usize, protocol::model::SampleRef) -> crate::channels::SamplerSetup + 'static,
    track: crate::channels::KitTrack,
    done: String,
    working: &'static str,
) -> Option<Outcome> {
    let server = app.bridge.borrow().as_ref()?.server.clone();
    let (a, author) = (app.clone(), author.clone());
    crate::samples_ui::import(app, items, move |results| {
        let mut setups = Vec::new();
        for (i, r) in results.into_iter().enumerate() {
            match r {
                Ok(s) => setups.push(make(i, s)),
                Err(reason) => {
                    server.reply(ticket, err(ControlError::Internal { reason }));
                    return;
                }
            }
        }
        let o = match crate::channels::add_kit_as(&a, author.clone(), track, setups) {
            Some(ids) => {
                push_activity(&a, &author, agent_string(&done), Vec::new());
                let created: Vec<u32> = ids.iter().map(|c| c.0).collect();
                presence::on_applied(&a, &author, &created, working);
                ok(ReplyBody::Applied(protocol::edit::Applied {
                    revision: revision(&a),
                    created,
                }))
            }
            None => err(ControlError::Busy),
        };
        server.reply(ticket, o);
        changed(&a);
    });
    None
}

/// `KitAdd` by catalogue id: the same action as "+" in the Sounds pane.
fn sound_add(
    app: &Rc<App>,
    ticket: Ticket,
    author: &Author,
    id: &str,
    track: Option<protocol::ids::TrackId>,
) -> Option<Outcome> {
    use crate::channels::KitTrack;
    use crate::sound_catalog::{self as cat, Target};
    use crate::{fl_library as fl, samples_ui::ImportItem};
    if let Some(t) = track
        && app.session.borrow().document().project.track(t).is_none()
    {
        return Some(err(ControlError::NotFound {
            what: "track".into(),
        }));
    }
    let kits = library(app);
    let state = fl_state(app);
    let loaded = loaded_of(&state);
    let Some(target) = cat::resolve(id, &kits, loaded.as_deref()) else {
        let why = if id.starts_with("fl:") {
            cat::fl_note(&state.status, state.remembered)
        } else {
            None
        };
        return Some(err(ControlError::NotFound {
            what: why
                .map(str::to_string)
                .unwrap_or_else(|| "sound (use an id from sound_search)".into()),
        }));
    };
    let target_track = |name: String| match track {
        Some(t) => KitTrack::Existing(t),
        None => KitTrack::New(name),
    };
    let fl_item = |e: &library::index::SoundEntry| ImportItem {
        path: e.path.clone(),
        local_only: true,
        expect_sha256: None,
    };
    match target {
        Target::OtoKit(k) => kit_add(
            app,
            ticket,
            author,
            &crate::sound_search::pack_of(k),
            &k.id,
            track,
        ),
        Target::Piece(k, p) => {
            let p = p.clone();
            let p2 = p.clone();
            import_and_add(
                app,
                ticket,
                author,
                vec![ImportItem::piece(&p, k.source)],
                move |_, s| crate::samples_ui::piece_setup(&p2, s),
                target_track(p.name.clone()),
                format!("Added {}", p.name),
                "Adding a sound",
            )
        }
        Target::FlSound(e) => {
            let e = e.clone();
            let e2 = e.clone();
            import_and_add(
                app,
                ticket,
                author,
                vec![fl_item(&e)],
                move |_, s| fl::sound_setup(&e2, s),
                target_track(e.name.clone()),
                format!("Added {}", e.name),
                "Adding a sound",
            )
        }
        Target::FlKit(k) => {
            let l = loaded.as_deref()?;
            let sounds: Vec<(&'static str, library::index::SoundEntry)> =
                fl::kit_sounds(k, &l.index)
                    .into_iter()
                    .map(|(slot, e)| (slot, e.clone()))
                    .collect();
            if sounds.is_empty() {
                return Some(err(ControlError::NotFound {
                    what: "sounds in that kit".into(),
                }));
            }
            let items = sounds.iter().map(|(_, e)| fl_item(e)).collect();
            let name = k.name.clone();
            import_and_add(
                app,
                ticket,
                author,
                items,
                move |i, s| fl::kit_piece_setup(sounds[i].0, &sounds[i].1, s),
                target_track(format!("{name} Kit")),
                format!("Added the {name} kit"),
                "Adding a drum kit",
            )
        }
        Target::FlInstrument(inst) => {
            let l = loaded.as_deref()?;
            let Some((root, e)) = fl::instrument_root(inst, &l.index) else {
                return Some(err(ControlError::NotFound {
                    what: "sounds in that instrument".into(),
                }));
            };
            let (item, name) = (fl_item(e), inst.name.clone());
            let n2 = name.clone();
            import_and_add(
                app,
                ticket,
                author,
                vec![item],
                move |_, s| fl::instrument_setup(root, &n2, s),
                target_track(name.clone()),
                format!("Added {name}"),
                "Adding an instrument",
            )
        }
        Target::Surge(_) => Some(bad(
            "add Surge XT sounds with instruments_add (kind plugin, plugin_id and preset)",
        )),
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
                next_id: s.document().next_id,
            })
        }
        RequestBody::ProjectInfo => ok(ReplyBody::ProjectInfo(project_info(app))),
        RequestBody::ProjectList => ok(ReplyBody::Projects {
            projects: list_projects(app),
        }),
        // A switch saves the open project first, exactly like the UI, and
        // answers only after the switch (18.6).
        RequestBody::ProjectNew { .. } => {
            let (a2, server) = (app.clone(), app.bridge.borrow().as_ref()?.server.clone());
            let (author, previous) = (author.clone(), app.ui.borrow().path.clone());
            files::save_before_switch(app, move |r| {
                let o = match r {
                    Ok(()) => {
                        files::fresh_project(&a2);
                        presence::on_project_changed(&a2, &author, "a new project", previous);
                        ok(ReplyBody::Done)
                    }
                    Err(e) => err(ControlError::Internal { reason: e }),
                };
                server.reply(ticket, o);
            });
            return None;
        }
        RequestBody::ProjectOpen { path } => {
            let p = PathBuf::from(&path);
            if !inside(&app.dirs.projects(), &p) {
                return Some(err(ControlError::NotFound {
                    what: "project in the projects folder".into(),
                }));
            }
            let (a2, server) = (app.clone(), app.bridge.borrow().as_ref()?.server.clone());
            let (author, previous) = (author.clone(), app.ui.borrow().path.clone());
            let name = p
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            files::save_before_switch(app, move |r| match r {
                Ok(()) => {
                    let (server, a3) = (server.clone(), a2.clone());
                    let (author, name, previous) = (author.clone(), name.clone(), previous.clone());
                    files::open_path_then(&a2, p.clone(), move |r| {
                        let o = match r {
                            Ok(()) => {
                                presence::on_project_changed(&a3, &author, &name, previous);
                                ok(ReplyBody::Done)
                            }
                            Err(e) => err(ControlError::Internal { reason: e }),
                        };
                        server.reply(ticket, o);
                    });
                }
                Err(e) => {
                    server.reply(ticket, err(ControlError::Internal { reason: e }));
                }
            });
            return None;
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
                    note_activity(app, author, &edits, &a.created);
                    presence::on_edit(app, author, &edits, &a.created);
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
        // Saves, then shows Home (18.6).
        RequestBody::ProjectClose => {
            let server = app.bridge.borrow().as_ref()?.server.clone();
            let (a2, author) = (app.clone(), author.clone());
            let previous = app.ui.borrow().path.clone();
            files::go_home(app, move |r| {
                let o = match r {
                    Ok(()) => {
                        presence::on_project_closed(&a2, &author, previous);
                        ok(ReplyBody::Done)
                    }
                    Err(e) => err(ControlError::Internal { reason: e }),
                };
                server.reply(ticket, o);
            });
            return None;
        }
        // The change tree and versions (15.11, 15.12) follow BRIDGE.md in
        // the next build; until then they answer plainly.
        RequestBody::HistoryTree { .. }
        | RequestBody::HistoryDiff { .. }
        | RequestBody::VersionSave { .. }
        | RequestBody::BranchCreate { .. }
        | RequestBody::BranchSwitch { .. }
        | RequestBody::BranchList
        | RequestBody::BranchRename { .. }
        | RequestBody::BranchArchive { .. }
        | RequestBody::VersionRestore { .. } => bad("versions are not available in this build yet"),
        RequestBody::NotesList { pattern } => {
            let s = app.session.borrow();
            match s.document().project.pattern(pattern) {
                Some(p) => ok(ReplyBody::Notes {
                    notes: p.notes.clone(),
                }),
                None => err(ControlError::NotFound {
                    what: "clip content".into(),
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
        RequestBody::TransportState => {
            let s = app.session.borrow();
            ok(ReplyBody::Transport {
                playing: app.ui.borrow().playing,
                tick: app.playhead_tick(),
                tempo_bpm: s.document().project.tempo_bpm,
                loop_region: s.document().project.loop_region,
            })
        }
        RequestBody::Undo => undo_redo(app, author, true),
        RequestBody::Redo => undo_redo(app, author, false),
        RequestBody::History => ok(ReplyBody::History {
            entries: app.session.borrow().editor.history().infos(),
        }),
        RequestBody::ExportWav {
            format,
            start,
            end,
            tail_seconds,
        } => {
            return Some(start_job(
                app,
                JobTarget {
                    range: range_of(start, end),
                    tail: tail_seconds,
                },
                Some(format),
            ));
        }
        RequestBody::Analyze { start, end } => {
            return Some(start_job(
                app,
                JobTarget {
                    range: range_of(start, end),
                    tail: 2.0,
                },
                None,
            ));
        }
        // The Hum sheet answers as a job of its own (control/src/hum.rs).
        RequestBody::JobStatus { job } if job == control::hum::JOB => match crate::hum::job() {
            Some((state, _, _)) => ok(ReplyBody::JobStatus {
                job,
                state,
                progress: 0.0,
            }),
            None => err(ControlError::NotFound { what: "job".into() }),
        },
        RequestBody::JobResult { job } if job == control::hum::JOB => match crate::hum::job() {
            Some((JobState::Done, clips, _)) => ok(ReplyBody::Applied(Applied {
                revision: revision(app),
                created: clips,
            })),
            Some(_) => bad("the hum has not finished"),
            None => err(ControlError::NotFound { what: "job".into() }),
        },
        RequestBody::JobCancel { job } if job == control::hum::JOB => {
            crate::hum::cancel_job();
            ok(ReplyBody::Done)
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
        RequestBody::SetActivity { text, focus } => {
            if let Some(p) = text.as_deref().and_then(control::hum::decode) {
                // An agent asks the user to hum; only the user starts it.
                return Some(if crate::hum::ask(app, author, p, focus) {
                    ok(ReplyBody::Done)
                } else {
                    err(ControlError::Busy)
                });
            }
            set_activity(app, author, text.as_deref(), focus);
            ok(ReplyBody::Done)
        }
        RequestBody::SoundSearch {
            role,
            genre,
            tags,
            limit,
            query,
            source,
            kit,
            offset,
        } => {
            let words: Vec<String> = query
                .iter()
                .map(String::as_str)
                .chain(tags.iter().map(String::as_str))
                .map(str::to_string)
                .collect();
            ok(sound_search(
                app,
                crate::sound_catalog::Query {
                    text: words.join(" "),
                    role: role.unwrap_or_default(),
                    source: source.unwrap_or_default(),
                    genre: genre.unwrap_or_default(),
                    kit: kit.unwrap_or_default(),
                    offset: offset as usize,
                    limit: limit.min(crate::sound_search::MAX_RESULTS) as usize,
                },
            ))
        }
        RequestBody::SoundAdd { id, track } => {
            let current = revision(app);
            if is_stale(base_revision, current) {
                return Some(err(ControlError::Stale { current }));
            }
            return sound_add(app, ticket, author, &id, track);
        }
        RequestBody::Seek { tick } => {
            app.seek(tick);
            ok(ReplyBody::Done)
        }
        RequestBody::KitAdd { pack, kit, track } => {
            let current = revision(app);
            if is_stale(base_revision, current) {
                return Some(err(ControlError::Stale { current }));
            }
            return kit_add(app, ticket, author, &pack, &kit, track);
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
                presence::on_applied(app, &author, &a.created, "Editing the project");
                ok(ReplyBody::Applied(wire(a)))
            }
            Err(f) => err(map_failure(f)),
        };
        reply(app, ticket, outcome);
    }
}

/// What an export or analysis job renders: a range of the timeline
/// (`None`: the loop region when it is on, else the whole arrangement)
/// plus a tail in seconds (SPEC 20).
#[derive(Clone, Copy)]
struct JobTarget {
    range: Option<(u32, u32)>,
    tail: f64,
}

/// A range from optional ends: both or neither.
fn range_of(start: Option<u32>, end: Option<u32>) -> Option<(u32, u32)> {
    match (start, end) {
        (Some(s), Some(e)) => Some((s, e)),
        (Some(s), None) => Some((s, u32::MAX)),
        (None, Some(e)) => Some((0, e)),
        (None, None) => None,
    }
}

fn start_job(app: &Rc<App>, target: JobTarget, fmt: Option<WavFormat>) -> Outcome {
    if !(0.0..=30.0).contains(&target.tail) {
        return bad("tail_seconds must be between 0 and 30");
    }
    let (project, slots, rate, rev, store, range) = {
        let s = app.session.borrow();
        let p = &s.document().project;
        let song_end = p.clips.iter().map(|c| c.end()).max().unwrap_or(0);
        let range = target.range.map(|(a, b)| (a, b.min(song_end.max(a + 1))));
        if let Some((a, b)) = range
            && a >= b
        {
            return bad("the range is empty");
        }
        if range.is_none()
            && !(p.loop_region.enabled && p.loop_region.end > p.loop_region.start)
            && song_end == 0
        {
            return bad("the timeline has no clips");
        }
        (
            s.document().project.clone(),
            s.slots.clone(),
            s.link.sample_rate().round() as u32,
            s.document().revision,
            s.store.clone(),
            range,
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
    let key: AnalysisKey = (rev, range, target.tail.to_bits());
    let cache = app
        .bridge
        .borrow()
        .as_ref()
        .map(|b| b.analysis_cache.clone())
        .unwrap_or_default();
    if fmt.is_none()
        && let Some((k, a)) = cache.borrow().as_ref()
        && *k == key
    {
        if let Some(j) = jobs.borrow_mut().get_mut(&id) {
            j.state = JobState::Done;
            j.result = Some(Ok(ReplyBody::Analysis(a.clone())));
            j.progress.store(100, Ordering::Relaxed);
        }
        return ok(ReplyBody::Job {
            job: id,
            revision: rev,
        });
    }
    let (start_tick, tempo_bpm) = {
        let p = &project;
        let loop_on = p.loop_region.enabled && p.loop_region.end > p.loop_region.start;
        let start = match range {
            Some((a, _)) => a,
            None if loop_on => p.loop_region.start,
            None => 0,
        };
        (start, p.tempo_bpm)
    };
    let song = project.clone();
    let (p2, c2) = (progress.clone(), cancel.clone());
    let jobs2 = jobs.clone();
    let cache2 = cache.clone();
    app.tasks.spawn(
        "control-job",
        move || -> Result<ReplyBody, String> {
            let frames = engine_adapter::render(
                RenderJob {
                    project,
                    range,
                    tail_seconds: target.tail,
                    sample_rate: rate,
                    store: Some(store),
                },
                &slots,
                &[],
                &p2,
                &c2,
            )
            .map_err(|e| e.to_string())?
            .audio;
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
                    let tpb = protocol::model::ticks_per_bar(song.time_sig_num);
                    let per_bar = f64::from(tpb) * 60.0 / (tempo_bpm * 960.0) * f64::from(rate);
                    a.bars =
                        control::song_map::bars(&frames, rate, per_bar, start_tick / tpb, &song);
                    a.sections = control::song_map::sections(&a.bars);
                    Ok(ReplyBody::Analysis(a))
                }
            }
        },
        move |r| {
            if let Some(j) = jobs2.borrow_mut().get_mut(&id) {
                match r {
                    Ok(body) => {
                        if let ReplyBody::Analysis(a) = &body {
                            *cache2.borrow_mut() = Some((key, a.clone()));
                        }
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
                    preset: None,
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
    fn an_agent_declares_and_ends_its_activity() {
        let rig = rig("activity");
        let long = "x".repeat(200);
        let out = talk(
            &rig,
            "agent",
            vec![
                r#"{"id":1,"body":{"op":"set_activity","text":"Writing the hats\u0007","focus":{"kind":"instrument","id":4}}}"#.into(),
            ],
        );
        assert!(out[1].contains("\"status\":\"ok\""), "{}", out[1]);
        {
            let b = rig.app.bridge.borrow();
            let a = b.as_ref().unwrap().ui.activity.clone().expect("activity");
            assert_eq!(a.text, "Writing the hats", "control characters removed");
            assert_eq!(a.focus, Some(Focus::Channel(protocol::ids::ChannelId(4))));
            assert!(a.author.starts_with("agent:test-"));
            assert_eq!(b.as_ref().unwrap().ui.recent[0].text, "Writing the hats");
        }
        let req = format!(r#"{{"id":2,"body":{{"op":"set_activity","text":"{long}"}}}}"#);
        talk(&rig, "agent", vec![req]);
        let len = rig
            .app
            .bridge
            .borrow()
            .as_ref()
            .unwrap()
            .ui
            .activity
            .as_ref()
            .map(|a| a.text.chars().count());
        assert_eq!(len, Some(MAX_ACTIVITY_CHARS));
        talk(
            &rig,
            "agent",
            vec![r#"{"id":3,"body":{"op":"set_activity","text":null}}"#.into()],
        );
        assert!(
            rig.app
                .bridge
                .borrow()
                .as_ref()
                .unwrap()
                .ui
                .activity
                .is_none()
        );
    }

    #[test]
    fn activity_text_is_cleaned() {
        assert_eq!(activity_text("  hi\n "), Some("hi".into()));
        assert_eq!(activity_text("\u{7}\u{8}"), None);
        assert_eq!(activity_text(&"é".repeat(99)).unwrap().chars().count(), 80);
    }

    #[test]
    fn an_unknown_kit_is_not_found() {
        let rig = rig("kit");
        let out = talk(
            &rig,
            "agent",
            vec![
                r#"{"id":1,"body":{"op":"kit_add","pack":"nope","kit":"nope","track":null}}"#
                    .into(),
                r#"{"id":2,"body":{"op":"sound_search","role":"kick","genre":null,"tags":[],"limit":5}}"#
                    .into(),
            ],
        );
        assert!(out[1].contains("not_found"), "{}", out[1]);
        assert!(out[2].contains("\"status\":\"ok\""), "{}", out[2]);
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
