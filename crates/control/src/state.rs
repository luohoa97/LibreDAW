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

use protocol::control::{ControlError, Outcome, Reply, Request, Transport};

use crate::client;
use crate::socket;

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
        }
    }

    /// `$XDG_RUNTIME_DIR/libredaw`; `None` if the variable is unset (never
    /// `/tmp`, 17.1).
    pub fn default_dir() -> Option<PathBuf> {
        let d = std::env::var_os("XDG_RUNTIME_DIR").filter(|d| !d.is_empty())?;
        Some(PathBuf::from(d).join("libredaw"))
    }
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
}

pub enum HelloErr {
    AgentsDisabled,
    NotAllowed,
    BusyOwner,
}

struct State {
    agents_enabled: bool,
    requested_notified: bool,
    next_client: u64,
    next_ticket: u64,
    clients: HashMap<u64, ClientSlot>,
    tickets: HashMap<u64, TicketState>,
    queue: Vec<Incoming>,
    events: Vec<UiEvent>,
    shutting_down: bool,
}

pub struct Shared {
    state: Mutex<State>,
    allow_scripts: bool,
    approval_timeout: Duration,
    busy_timeout: Duration,
}

fn error_line(id: u64, error: ControlError) -> String {
    let reply = Reply {
        id,
        outcome: Outcome::Err { error },
    };
    serde_json::to_string(&reply).unwrap_or_default()
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn is_shutting_down(&self) -> bool {
        self.lock().shutting_down
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
    /// enabled flag so a banner click cannot race the check.
    pub fn register(
        &self,
        id: u64,
        transport: Transport,
        name: String,
        out: Sender<Out>,
        stream: UnixStream,
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
            },
        );
        s.events.push(UiEvent::ClientConnected(info.clone()));
        Ok(info)
    }

    /// A validated request from client `id`. `Err` carries the line to send
    /// back at once.
    pub fn submit(&self, client: u64, request: Request) -> Result<(), String> {
        let mut s = self.lock();
        let Some(slot) = s.clients.get(&client) else {
            return Ok(());
        };
        let info = slot.info.clone();
        let in_flight = s.tickets.values().filter(|t| t.client == client).count();
        if in_flight >= MAX_IN_FLIGHT {
            return Err(error_line(request.id, ControlError::Busy));
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
        if s.clients.remove(&id).is_none() {
            return;
        }
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
    }
}

/// The server handle. Dropping it shuts it down.
pub struct ControlServer {
    shared: Arc<Shared>,
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
            }),
            allow_scripts: cfg.allow_scripts,
            approval_timeout: cfg.approval_timeout,
            busy_timeout: cfg.busy_timeout,
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
                let _ = c.out.send(Out::Line(error_line(t.request_id, error)));
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
        Polled { requests, events }
    }

    /// Answers one request. False if the ticket is unknown: expired,
    /// already answered, or its client left.
    pub fn reply(&self, ticket: Ticket, outcome: Outcome) -> bool {
        let mut s = self.shared.lock();
        let Some(t) = s.tickets.remove(&ticket.0) else {
            return false;
        };
        let line = serde_json::to_string(&Reply {
            id: t.request_id,
            outcome,
        })
        .unwrap_or_else(|e| {
            error_line(
                t.request_id,
                ControlError::Internal {
                    reason: e.to_string(),
                },
            )
        });
        match s.clients.get(&t.client) {
            Some(c) => c.out.send(Out::Line(line)).is_ok(),
            None => false,
        }
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
            let _ = c
                .out
                .send(Out::Line(error_line(t.request_id, ControlError::Denied)));
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
