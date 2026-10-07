// SPDX-License-Identifier: GPL-3.0-or-later
//! State of one MCP client connection: replies the tool workers wait for,
//! the document revision this agent last saw, resource subscriptions, and
//! requests the server sent to the client (sampling).

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use protocol::control::{Outcome, ReplyBody};
use serde_json::{Value, json};

use crate::state::Out;

pub const PROJECT_URI: &str = "libredaw://project";
pub const MIXER_URI: &str = "libredaw://mixer";
pub const SONG_URI: &str = "libredaw://song";
pub const SUGGESTIONS_URI: &str = "libredaw://suggestions_pending";
pub const PATTERN_URI_PREFIX: &str = "libredaw://pattern/";
pub const HISTORY_URI: &str = "libredaw://history";

#[derive(Default)]
struct Inner {
    initialized: bool,
    sampling: bool,
    revision: Option<u64>,
    /// Lowest id the document counter can be at: above every id this
    /// agent saw created (`ids.rs`).
    id_floor: Option<u32>,
    last_notified: Option<u64>,
    subs: HashSet<String>,
    /// MCP logging level rank (debug 0 .. emergency 7); `None` = no
    /// `logging/setLevel` yet, so no log messages are sent.
    log_level: Option<u8>,
}

pub struct Session {
    client_id: u64,
    tx: Sender<Out>,
    waiting: Mutex<HashMap<u64, Sender<Outcome>>>,
    next_req: AtomicU64,
    server_calls: Mutex<HashMap<String, Sender<Result<Value, Value>>>>,
    next_srv: AtomicU64,
    inner: Mutex<Inner>,
    pub(crate) in_flight: AtomicUsize,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

const LEVELS: [&str; 8] = [
    "debug",
    "info",
    "notice",
    "warning",
    "error",
    "critical",
    "alert",
    "emergency",
];

impl Session {
    pub fn new(client_id: u64, tx: Sender<Out>) -> Session {
        Session {
            client_id,
            tx,
            waiting: Mutex::default(),
            next_req: AtomicU64::new(1),
            server_calls: Mutex::default(),
            next_srv: AtomicU64::new(1),
            inner: Mutex::default(),
            in_flight: AtomicUsize::new(0),
        }
    }

    pub fn sender(&self) -> Sender<Out> {
        self.tx.clone()
    }

    pub fn client_id(&self) -> u64 {
        self.client_id
    }

    /// One JSON-RPC message to the client.
    pub fn send(&self, v: &Value) {
        let _ = self.tx.send(Out::Line(v.to_string()));
    }

    pub fn notify(&self, method: &str, params: Value) {
        self.send(&json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }

    pub fn set_initialized(&self, sampling: bool) {
        let mut i = lock(&self.inner);
        i.initialized = true;
        i.sampling = sampling;
    }

    pub fn initialized(&self) -> bool {
        lock(&self.inner).initialized
    }

    /// The client declared the `sampling` capability.
    pub fn has_sampling(&self) -> bool {
        lock(&self.inner).sampling
    }

    // ---- control requests --------------------------------------------

    pub fn next_request_id(&self) -> u64 {
        self.next_req.fetch_add(1, Ordering::SeqCst)
    }

    pub fn expect_reply(&self, id: u64, tx: Sender<Outcome>) {
        lock(&self.waiting).insert(id, tx);
    }

    pub fn forget_reply(&self, id: u64) {
        lock(&self.waiting).remove(&id);
    }

    /// An answer from the UI (called on the UI side of the socket server).
    /// Records the revision it carries as the one this agent has seen.
    pub fn deliver(&self, request_id: u64, outcome: Outcome) -> bool {
        if let Outcome::Ok { body } = &outcome {
            let rev = match body {
                ReplyBody::Project { revision, .. } | ReplyBody::Job { revision, .. } => {
                    Some(*revision)
                }
                ReplyBody::Applied(a) => Some(a.revision),
                ReplyBody::Analysis(a) => Some(a.revision),
                _ => None,
            };
            let mut i = lock(&self.inner);
            if rev.is_some() {
                i.revision = rev;
            }
            if let ReplyBody::Applied(a) = body
                && let Some(max) = a.created.iter().max()
            {
                i.id_floor = Some(i.id_floor.unwrap_or(0).max(max.saturating_add(1)));
            }
        }
        match lock(&self.waiting).remove(&request_id) {
            Some(tx) => tx.send(outcome).is_ok(),
            None => false,
        }
    }

    /// The document revision this agent last saw in a reply.
    pub fn revision(&self) -> Option<u64> {
        lock(&self.inner).revision
    }

    /// A new document resets what was seen, numbering included.
    pub fn set_revision(&self, r: Option<u64>) {
        let mut i = lock(&self.inner);
        i.revision = r;
        if r.is_none() {
            i.id_floor = None;
        }
    }

    /// See `ids.rs`.
    pub fn id_floor(&self) -> Option<u32> {
        lock(&self.inner).id_floor
    }

    /// The connection ended: wakes every waiting worker.
    pub fn close(&self) {
        lock(&self.waiting).clear();
        lock(&self.server_calls).clear();
    }

    // ---- resources ------------------------------------------------------

    pub fn subscribe(&self, uri: &str) {
        lock(&self.inner).subs.insert(uri.to_string());
    }

    pub fn unsubscribe(&self, uri: &str) {
        lock(&self.inner).subs.remove(uri);
    }

    fn is_project_resource(uri: &str) -> bool {
        uri == PROJECT_URI
            || uri == MIXER_URI
            || uri == SONG_URI
            || uri == HISTORY_URI
            || uri.starts_with(PATTERN_URI_PREFIX)
    }

    /// The document is at `revision`. Subscribers hear about it unless it
    /// is the revision this agent itself just made.
    pub fn on_revision(&self, revision: u64) {
        let uris: Vec<String> = {
            let mut i = lock(&self.inner);
            if !i.initialized || i.revision == Some(revision) || i.last_notified == Some(revision) {
                return;
            }
            i.last_notified = Some(revision);
            i.subs
                .iter()
                .filter(|u| Self::is_project_resource(u))
                .cloned()
                .collect()
        };
        for uri in uris {
            self.notify("notifications/resources/updated", json!({"uri": uri}));
        }
        self.log(
            "info",
            json!(format!("the project changed (revision {revision})")),
        );
    }

    /// Every project resource changed (a new document was opened).
    pub fn notify_project_resources(&self) {
        let uris: Vec<String> = {
            let mut i = lock(&self.inner);
            i.last_notified = None;
            i.subs
                .iter()
                .filter(|u| Self::is_project_resource(u))
                .cloned()
                .collect()
        };
        for uri in uris {
            self.notify("notifications/resources/updated", json!({"uri": uri}));
        }
    }

    /// One resource changed.
    pub fn notify_resource(&self, uri: &str) {
        let subscribed = lock(&self.inner).subs.contains(uri);
        if subscribed {
            self.notify("notifications/resources/updated", json!({"uri": uri}));
        }
    }

    // ---- logging and progress -----------------------------------------

    pub fn set_log_level(&self, level: &str) -> bool {
        match LEVELS.iter().position(|l| *l == level) {
            Some(p) => {
                lock(&self.inner).log_level = Some(p as u8);
                true
            }
            None => false,
        }
    }

    /// `notifications/message`, if the client asked for logs at `level`.
    pub fn log(&self, level: &str, data: Value) {
        let rank = LEVELS.iter().position(|l| *l == level).unwrap_or(1) as u8;
        let wanted = lock(&self.inner).log_level;
        if wanted.is_some_and(|w| rank >= w) {
            self.notify(
                "notifications/message",
                json!({"level": level, "logger": "libredaw", "data": data}),
            );
        }
    }

    pub fn progress(&self, token: &Value, progress: f64, message: &str) {
        self.notify(
            "notifications/progress",
            json!({"progressToken": token, "progress": progress, "total": 100, "message": message}),
        );
    }

    // ---- requests to the client (sampling) --------------------------------

    /// Sends `method` to the client and waits for its answer.
    pub fn client_request(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, String> {
        let id = format!("srv-{}", self.next_srv.fetch_add(1, Ordering::SeqCst));
        let (tx, rx) = channel();
        lock(&self.server_calls).insert(id.clone(), tx);
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        match rx.recv_timeout(timeout) {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(e)) => Err(e["message"]
                .as_str()
                .unwrap_or("the client refused the request")
                .chars()
                .filter(|c| !c.is_control())
                .take(200)
                .collect()),
            Err(_) => {
                lock(&self.server_calls).remove(&id);
                Err("the client did not answer in time".into())
            }
        }
    }

    /// A response from the client to one of our requests.
    pub fn client_response(&self, id: &str, result: Result<Value, Value>) {
        if let Some(tx) = lock(&self.server_calls).remove(id) {
            let _ = tx.send(result);
        }
    }
}
