// SPDX-License-Identifier: GPL-3.0-or-later
//! Server state shared by the listener, the client threads, and the GTK
//! thread: clients, tickets, deadlines, and the queues behind `poll`.

use std::collections::HashMap;
use std::fs::File;
use std::io;
use std::net::Shutdown;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use protocol::control::{ControlError, Outcome, Reply, ReplyBody, Request, Transport};

use crate::client;
use crate::mcp::Session;
use crate::socket;
use crate::suggest::{PendingSuggestion, SuggestionEvent};

/// Approval wait for a PRIVILEGED request (17.1).
pub const APPROVAL_TIMEOUT: Duration = Duration::from_secs(60);
/// How long a request deferred behind a gesture may wait (17.1).
pub const BUSY_TIMEOUT: Duration = Duration::from_secs(10);
/// A client that has not sent its hello after this long is dropped.
pub const HELLO_TIMEOUT: Duration = Duration::from_secs(5);
/// Requests one client may have in flight before it gets `Busy`.
pub const MAX_IN_FLIGHT: usize = 64;
/// Simultaneous connections; further ones are closed at once.
pub const MAX_CLIENTS: usize = 16;

#[derive(Clone, Debug)]
pub struct ControlConfig {
    /// `$XDG_RUNTIME_DIR/libredaw` (created 0700, holds the lock file).
    pub socket_dir: PathBuf,
    /// Per session; off by default.
    pub agents_enabled: bool,
    /// Started with `--agent-request` (the UI shows the enable banner).
    pub agent_request: bool,
    /// Whether the `script` transport may connect at all.
    pub allow_scripts: bool,
    pub approval_timeout: Duration,
    pub busy_timeout: Duration,
    /// How long an MCP `initialize` waits for the user to enable agent
    /// control before it gets a clear error (the user may be about to click
    /// the banner after `--agent-request`).
    pub agents_wait: Duration,
    /// How long an export or analysis tool call waits for its job.
    pub job_wait: Duration,
}

impl ControlConfig {
    pub fn new(socket_dir: PathBuf) -> ControlConfig {
        ControlConfig {
            socket_dir,
            agents_enabled: false,
            agent_request: false,
            allow_scripts: true,
            approval_timeout: APPROVAL_TIMEOUT,
            busy_timeout: BUSY_TIMEOUT,
            agents_wait: Duration::from_secs(25),
            job_wait: Duration::from_secs(120),
        }
    }

    /// `$XDG_RUNTIME_DIR/libredaw`; `None` if the variable is unset (never
    /// `/tmp`, 17.1).
    ///
    /// Inside a Flatpak the directory is `$XDG_RUNTIME_DIR/app/$FLATPAK_ID/libredaw`:
    /// each `flatpak run` gets its own private runtime dir, except that
    /// `app/<id>` is shared by every instance of the app, so the DAW and a
    /// `libredaw-mcp` started separately find the same socket.
    pub fn default_dir() -> Option<PathBuf> {
        let base = PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR").filter(|d| !d.is_empty())?);
        Some(dir_in(&base, std::env::var("FLATPAK_ID").ok().as_deref()))
    }
}

/// The socket directory under runtime dir `base` (see `default_dir`).
fn dir_in(base: &Path, flatpak_id: Option<&str>) -> PathBuf {
    if let Some(id) =
        flatpak_id.filter(|id| !id.is_empty() && !id.contains('/') && !id.starts_with('.'))
    {
        let shared = base.join("app").join(id);
        if shared.is_dir() {
            return shared.join("libredaw");
        }
    }
    base.join("libredaw")
}

#[cfg(test)]
mod dir_tests {
    use super::dir_in;

    #[test]
    fn flatpak_uses_the_shared_app_dir_when_it_exists() {
        let base = std::env::temp_dir().join(format!("ldaw-dir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("app/org.example.App")).unwrap();
        assert_eq!(
            dir_in(&base, Some("org.example.App")),
            base.join("app/org.example.App/libredaw")
        );
        // Not in a Flatpak, no such app dir, or a hostile id: the plain path.
        assert_eq!(dir_in(&base, None), base.join("libredaw"));
        assert_eq!(dir_in(&base, Some("other.App")), base.join("libredaw"));
        assert_eq!(dir_in(&base, Some("../x")), base.join("libredaw"));
        assert_eq!(dir_in(&base, Some("")), base.join("libredaw"));
        let _ = std::fs::remove_dir_all(&base);
    }
}

/// The control socket of the running DAW, or `None` if `XDG_RUNTIME_DIR`
/// is unset.
pub fn default_socket_path() -> Option<PathBuf> {
    ControlConfig::default_dir().map(|d| d.join(crate::socket::SOCKET_NAME))
}

#[derive(Debug)]
pub enum ControlStartError {
    /// Another DAW holds the lock.
    AlreadyRunning,
    Io(io::Error),
}

impl std::fmt::Display for ControlStartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ControlStartError::AlreadyRunning => {
                write!(f, "another LibreDAW already owns the control socket")
            }
            ControlStartError::Io(e) => write!(f, "cannot start the control socket: {e}"),
        }
    }
}

impl std::error::Error for ControlStartError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Ticket(pub u64);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientInfo {
    pub id: u64,
    pub transport: Transport,
    /// Untrusted (17.1): capped and stripped of control characters.
    pub name: String,
    /// Not filled in yet (needs SO_PEERCRED, which std does not expose).
    pub pid: Option<u32>,
}

#[derive(Clone, Debug)]
pub struct Incoming {
    pub ticket: Ticket,
    pub client: ClientInfo,
    pub request: Request,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UiEvent {
    /// An agent tried to connect while agent control is off.
    AgentRequestedControl {
        client: ClientInfo,
    },
    ApprovalNeeded {
        ticket: Ticket,
        summary: String,
    },
    /// The 60 s approval deadline passed; the client got `needs_user_approval`.
    ApprovalTimedOut {
        ticket: Ticket,
    },
    /// A deferred request waited 10 s; the client got `Busy`. Drop it.
    DeferTimedOut {
        ticket: Ticket,
    },
    /// The client disconnected while the UI held this ticket. Drop it.
    TicketCancelled {
        ticket: Ticket,
    },
    ClientConnected(ClientInfo),
    ClientGone {
        id: u64,
    },
}

/// What one `poll` call returns.
#[derive(Debug, Default)]
pub struct Polled {
    pub requests: Vec<Incoming>,
    pub events: Vec<UiEvent>,
    /// Suggestion lifecycle (18.5). Kept apart from `events` so existing
    /// exhaustive matches on `UiEvent` keep compiling.
    pub suggestions: Vec<SuggestionEvent>,
}

/// Lines for a client's writer thread.
pub enum Out {
    Line(String),
    Close,
}

enum Phase {
    /// In the queue, not yet returned by `poll`.
    Queued,
    /// The UI has it.
    Held,
    Approval {
        deadline: Instant,
    },
    Deferred {
        deadline: Instant,
    },
}

struct TicketState {
    client: u64,
    request_id: u64,
    phase: Phase,
}

struct ClientSlot {
    info: ClientInfo,
    out: Sender<Out>,
    stream: UnixStream,
    /// Set for MCP clients: replies go to the session, not to a line.
    mcp: Option<Arc<Session>>,
}

pub enum HelloErr {
    AgentsDisabled,
    NotAllowed,
    BusyOwner,
}

pub(crate) struct State {
    agents_enabled: bool,
    requested_notified: bool,
    next_client: u64,
    next_ticket: u64,
    clients: HashMap<u64, ClientSlot>,
    tickets: HashMap<u64, TicketState>,
    queue: Vec<Incoming>,
    events: Vec<UiEvent>,
    shutting_down: bool,
    /// Latest document revision seen in any reply or `notify_revision`.
    revision: Option<u64>,
    pub(crate) next_suggestion: u64,
    pub(crate) pending: Vec<PendingSuggestion>,
    pub(crate) suggestion_events: Vec<SuggestionEvent>,
}

pub struct Shared {
    state: Mutex<State>,
    allow_scripts: bool,
    pub(crate) approval_timeout: Duration,
    busy_timeout: Duration,
    pub(crate) agents_wait: Duration,
    pub(crate) job_wait: Duration,
}

fn error_outcome(error: ControlError) -> Outcome {
    Outcome::Err { error }
}

fn reply_line(id: u64, outcome: Outcome) -> String {
    let fallback = id;
    serde_json::to_string(&Reply { id, outcome }).unwrap_or_else(|e| {
        serde_json::to_string(&Reply {
            id: fallback,
            outcome: error_outcome(ControlError::Internal {
                reason: e.to_string(),
            }),
        })
        .unwrap_or_default()
    })
}

/// Sends `outcome` to the client behind `slot`: a reply line for scripts, a
/// tool-call result for MCP sessions.
fn deliver(slot: &ClientSlot, request_id: u64, outcome: Outcome) -> bool {
    match &slot.mcp {
        Some(session) => session.deliver(request_id, outcome),
        None => slot
            .out
            .send(Out::Line(reply_line(request_id, outcome)))
            .is_ok(),
    }
}

/// The document revision a reply carries, if any.
fn outcome_revision(outcome: &Outcome) -> Option<u64> {
    match outcome {
        Outcome::Ok { body } => match body {
            ReplyBody::Project { revision, .. } | ReplyBody::Job { revision, .. } => {
                Some(*revision)
            }
            ReplyBody::Applied(a) => Some(a.revision),
            ReplyBody::Analysis(a) => Some(a.revision),
            _ => None,
        },
        Outcome::Err { .. } => None,
    }
}

impl Shared {
    pub(crate) fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn is_shutting_down(&self) -> bool {
        self.lock().shutting_down
    }

    pub fn agents_enabled(&self) -> bool {
        self.lock().agents_enabled
    }

    pub fn next_client_id(&self) -> Option<u64> {
        let mut s = self.lock();
        if s.shutting_down || s.clients.len() >= MAX_CLIENTS {
            return None;
        }
        s.next_client += 1;
        Some(s.next_client)
    }

    /// Hello succeeded: adds the client or says why not. Atomic with the
    /// enabled flag so a banner click cannot race the check. `mcp` is the
    /// session of an MCP client.
    pub fn register(
        &self,
        id: u64,
        transport: Transport,
        name: String,
        out: Sender<Out>,
        stream: UnixStream,
        mcp: Option<Arc<Session>>,
    ) -> Result<ClientInfo, HelloErr> {
        let mut s = self.lock();
        let info = ClientInfo {
            id,
            transport,
            name,
            pid: None,
        };
        match transport {
            Transport::Script if !self.allow_scripts => return Err(HelloErr::NotAllowed),
            Transport::Script => {}
            Transport::Agent => {
                if !s.agents_enabled {
                    if !s.requested_notified {
                        s.requested_notified = true;
                        s.events
                            .push(UiEvent::AgentRequestedControl { client: info });
                    }
                    return Err(HelloErr::AgentsDisabled);
                }
                if s.clients
                    .values()
                    .any(|c| c.info.transport == Transport::Agent)
                {
                    return Err(HelloErr::BusyOwner);
                }
            }
        }
        s.clients.insert(
            id,
            ClientSlot {
                info: info.clone(),
                out,
                stream,
                mcp,
            },
        );
        s.events.push(UiEvent::ClientConnected(info.clone()));
        Ok(info)
    }

    /// A validated request from client `id`. `Err` carries the outcome to
    /// answer with at once.
    #[allow(clippy::result_large_err)]
    pub fn submit(&self, client: u64, request: Request) -> Result<(), Outcome> {
        let mut s = self.lock();
        let Some(slot) = s.clients.get(&client) else {
            return Ok(());
        };
        let info = slot.info.clone();
        let in_flight = s.tickets.values().filter(|t| t.client == client).count();
        if in_flight >= MAX_IN_FLIGHT {
            return Err(error_outcome(ControlError::Busy));
        }
        s.next_ticket += 1;
        let ticket = Ticket(s.next_ticket);
        s.tickets.insert(
            ticket.0,
            TicketState {
                client,
                request_id: request.id,
                phase: Phase::Queued,
            },
        );
        s.queue.push(Incoming {
            ticket,
            client: info,
            request,
        });
        Ok(())
    }

    /// Client thread ended. Idempotent.
    pub fn unregister(&self, id: u64) {
        let mut s = self.lock();
        let Some(slot) = s.clients.remove(&id) else {
            return;
        };
        s.queue.retain(|i| i.client.id != id);
        let gone: Vec<u64> = s
            .tickets
            .iter()
            .filter(|(_, t)| t.client == id)
            .map(|(k, _)| *k)
            .collect();
        for k in gone {
            if let Some(t) = s.tickets.remove(&k)
                && !matches!(t.phase, Phase::Queued)
            {
                s.events
                    .push(UiEvent::TicketCancelled { ticket: Ticket(k) });
            }
        }
        s.events.push(UiEvent::ClientGone { id });
        if slot.info.transport == Transport::Agent {
            // Pending suggestions can no longer be answered (18.5).
            let dropped: Vec<PendingSuggestion> = std::mem::take(&mut s.pending);
            for p in dropped {
                s.suggestion_events.push(SuggestionEvent::Failed {
                    id: p.id,
                    reason: "the agent disconnected".into(),
                });
            }
        }
        drop(s);
        if let Some(session) = slot.mcp {
            session.close();
        }
    }

    /// The agent session, if an MCP agent is connected.
    pub(crate) fn agent_session(&self) -> Option<Arc<Session>> {
        self.lock()
            .clients
            .values()
            .find(|c| c.info.transport == Transport::Agent)
            .and_then(|c| c.mcp.clone())
    }

    /// The document changed to `revision`: tells MCP clients that subscribed
    /// to resources.
    pub fn note_revision(&self, revision: u64) {
        let sessions = {
            let mut s = self.lock();
            if s.revision == Some(revision) {
                return;
            }
            s.revision = Some(revision);
            s.clients
                .values()
                .filter_map(|c| c.mcp.clone())
                .collect::<Vec<_>>()
        };
        for session in sessions {
            session.on_revision(revision);
        }
    }

    pub(crate) fn push_suggestion_event(&self, e: SuggestionEvent) {
        self.lock().suggestion_events.push(e);
    }
}

/// The server handle. Dropping it shuts it down.
pub struct ControlServer {
    pub(crate) shared: Arc<Shared>,
    socket_path: PathBuf,
    listener_thread: Option<JoinHandle<()>>,
    lock: Option<File>,
    agent_request: bool,
}

impl ControlServer {
    pub fn start(cfg: ControlConfig) -> Result<ControlServer, ControlStartError> {
        let bound = socket::bind(&cfg.socket_dir)?;
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                agents_enabled: cfg.agents_enabled,
                requested_notified: false,
                next_client: 0,
                next_ticket: 0,
                clients: HashMap::new(),
                tickets: HashMap::new(),
                queue: Vec::new(),
                events: Vec::new(),
                shutting_down: false,
                revision: None,
                next_suggestion: 0,
                pending: Vec::new(),
                suggestion_events: Vec::new(),
            }),
            allow_scripts: cfg.allow_scripts,
            approval_timeout: cfg.approval_timeout,
            busy_timeout: cfg.busy_timeout,
            agents_wait: cfg.agents_wait,
            job_wait: cfg.job_wait,
        });
        let socket_path = bound.socket_path.clone();
        let listener_shared = Arc::clone(&shared);
        let listener: UnixListener = bound.listener;
        let listener_thread = thread::Builder::new()
            .name("control-listener".into())
            .spawn(move || client::accept_loop(listener, listener_shared))
            .map_err(ControlStartError::Io)?;
        Ok(ControlServer {
            shared,
            socket_path,
            listener_thread: Some(listener_thread),
            lock: Some(bound.lock),
            agent_request: cfg.agent_request,
        })
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// Started with `--agent-request`.
    pub fn agent_request(&self) -> bool {
        self.agent_request
    }

    /// From Preferences or the banner. Turning it off disconnects agents.
    pub fn set_agents_enabled(&self, on: bool) {
        let mut s = self.shared.lock();
        s.agents_enabled = on;
        s.requested_notified = false;
        if !on {
            for c in s.clients.values() {
                if c.info.transport == Transport::Agent {
                    let _ = c.out.send(Out::Close);
                    let _ = c.stream.shutdown(Shutdown::Both);
                }
            }
        }
    }

    pub fn agents_enabled(&self) -> bool {
        self.shared.lock().agents_enabled
    }

    /// The document is now at `revision` (call after every change, from any
    /// author, so MCP clients subscribed to `libredaw://` resources hear
    /// about changes the user makes). Replies that carry a revision are
    /// observed automatically; this covers the user's own edits.
    pub fn notify_revision(&self, revision: u64) {
        self.shared.note_revision(revision);
    }

    /// GTK thread, every 10 ms. Also expires approval and Busy deadlines.
    pub fn poll(&self) -> Polled {
        let now = Instant::now();
        let mut s = self.shared.lock();
        let expired: Vec<(u64, bool)> = s
            .tickets
            .iter()
            .filter_map(|(k, t)| match t.phase {
                Phase::Approval { deadline } if deadline <= now => Some((*k, true)),
                Phase::Deferred { deadline } if deadline <= now => Some((*k, false)),
                _ => None,
            })
            .collect();
        for (k, approval) in expired {
            let Some(t) = s.tickets.remove(&k) else {
                continue;
            };
            let (error, event) = if approval {
                (
                    ControlError::NeedsUserApproval,
                    UiEvent::ApprovalTimedOut { ticket: Ticket(k) },
                )
            } else {
                (
                    ControlError::Busy,
                    UiEvent::DeferTimedOut { ticket: Ticket(k) },
                )
            };
            if let Some(c) = s.clients.get(&t.client) {
                deliver(c, t.request_id, error_outcome(error));
            }
            s.events.push(event);
        }
        let requests = std::mem::take(&mut s.queue);
        for r in &requests {
            if let Some(t) = s.tickets.get_mut(&r.ticket.0) {
                t.phase = Phase::Held;
            }
        }
        let events = std::mem::take(&mut s.events);
        let suggestions = std::mem::take(&mut s.suggestion_events);
        Polled {
            requests,
            events,
            suggestions,
        }
    }

    /// Answers one request. False if the ticket is unknown: expired,
    /// already answered, or its client left.
    pub fn reply(&self, ticket: Ticket, outcome: Outcome) -> bool {
        let rev = outcome_revision(&outcome);
        let delivered = {
            let mut s = self.shared.lock();
            let Some(t) = s.tickets.remove(&ticket.0) else {
                return false;
            };
            match s.clients.get(&t.client) {
                Some(c) => deliver(c, t.request_id, outcome),
                None => false,
            }
        };
        if let Some(rev) = rev {
            self.shared.note_revision(rev);
        }
        delivered
    }

    /// The UI decided this request is PRIVILEGED: show the banner and wait
    /// up to 60 s for `approval`. False if the ticket is not live.
    pub fn require_approval(&self, ticket: Ticket, summary: String) -> bool {
        let deadline = Instant::now() + self.shared.approval_timeout;
        let mut s = self.shared.lock();
        let Some(t) = s.tickets.get_mut(&ticket.0) else {
            return false;
        };
        t.phase = Phase::Approval { deadline };
        s.events.push(UiEvent::ApprovalNeeded { ticket, summary });
        true
    }

    /// The human clicked Allow or Deny. Deny replies `Denied` here and
    /// returns false. On Allow returns true: the UI now executes the
    /// request it holds and calls `reply`. False also if the ticket timed
    /// out meanwhile (the UI drops it).
    pub fn approval(&self, ticket: Ticket, allowed: bool) -> bool {
        let mut s = self.shared.lock();
        let Some(t) = s.tickets.get_mut(&ticket.0) else {
            return false;
        };
        if !matches!(t.phase, Phase::Approval { .. }) {
            return false;
        }
        if allowed {
            t.phase = Phase::Held;
            return true;
        }
        let t = s.tickets.remove(&ticket.0).expect("present");
        if let Some(c) = s.clients.get(&t.client) {
            deliver(c, t.request_id, error_outcome(ControlError::Denied));
        }
        false
    }

    /// The UI cannot run this request now (a gesture is open). If it does
    /// not `reply` within 10 s the client gets `Busy`.
    pub fn defer(&self, ticket: Ticket) -> bool {
        let deadline = Instant::now() + self.shared.busy_timeout;
        let mut s = self.shared.lock();
        let Some(t) = s.tickets.get_mut(&ticket.0) else {
            return false;
        };
        t.phase = Phase::Deferred { deadline };
        true
    }

    pub fn clients(&self) -> Vec<ClientInfo> {
        let s = self.shared.lock();
        let mut v: Vec<ClientInfo> = s.clients.values().map(|c| c.info.clone()).collect();
        v.sort_by_key(|c| c.id);
        v
    }

    pub fn shutdown(mut self) {
        self.stop();
    }

    fn stop(&mut self) {
        {
            let mut s = self.shared.lock();
            if s.shutting_down {
                return;
            }
            s.shutting_down = true;
            for c in s.clients.values() {
                let _ = c.out.send(Out::Close);
                let _ = c.stream.shutdown(Shutdown::Both);
            }
        }
        // Wake the blocking accept.
        let _ = UnixStream::connect(&self.socket_path);
        if let Some(h) = self.listener_thread.take() {
            let _ = h.join();
        }
        let _ = std::fs::remove_file(&self.socket_path);
        self.lock = None;
    }
}

impl Drop for ControlServer {
    fn drop(&mut self) {
        self.stop();
    }
}
