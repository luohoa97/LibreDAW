// SPDX-License-Identifier: GPL-3.0-or-later
//! Test support: a tiny stand-in for the `ui` side of the control socket.
//!
//! It runs the same loop `ui` runs on its 10 ms tick (`ControlServer::poll`)
//! and answers every request with a canned action, so the MCP <-> control
//! path can be tested without GTK. Not used by the DAW itself.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use protocol::control::{Outcome, Request};

use crate::{ControlServer, Incoming, UiEvent};

/// What the fake UI does with one request.
pub enum Action {
    /// Answer at once.
    Reply(Outcome),
    /// Treat the request as PRIVILEGED: raise the approval banner, let the
    /// "human" click `allow`, and on Allow answer with `then`.
    Approve {
        summary: String,
        allow: bool,
        then: Outcome,
    },
    /// Raise the banner and never click (the 60 s deadline expires).
    LeaveApprovalOpen { summary: String },
    /// A gesture is open: defer and never answer (the Busy deadline expires).
    Defer,
}

pub struct FakeUi {
    server: Arc<ControlServer>,
    stop: Arc<AtomicBool>,
    requests: Arc<Mutex<Vec<Request>>>,
    events: Arc<Mutex<Vec<UiEvent>>>,
    thread: Option<JoinHandle<()>>,
}

impl FakeUi {
    /// Starts polling `server` every few milliseconds.
    pub fn start(
        server: ControlServer,
        mut handler: impl FnMut(&Incoming) -> Action + Send + 'static,
    ) -> FakeUi {
        let server = Arc::new(server);
        let stop = Arc::new(AtomicBool::new(false));
        let requests: Arc<Mutex<Vec<Request>>> = Arc::default();
        let events: Arc<Mutex<Vec<UiEvent>>> = Arc::default();
        let (s, st, rq, ev) = (
            Arc::clone(&server),
            Arc::clone(&stop),
            Arc::clone(&requests),
            Arc::clone(&events),
        );
        let thread = thread::spawn(move || {
            while !st.load(Ordering::SeqCst) {
                let polled = s.poll();
                ev.lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .extend(polled.events);
                for inc in polled.requests {
                    rq.lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push(inc.request.clone());
                    match handler(&inc) {
                        Action::Reply(o) => {
                            s.reply(inc.ticket, o);
                        }
                        Action::Approve {
                            summary,
                            allow,
                            then,
                        } => {
                            s.require_approval(inc.ticket, summary);
                            if s.approval(inc.ticket, allow) {
                                s.reply(inc.ticket, then);
                            }
                        }
                        Action::LeaveApprovalOpen { summary } => {
                            s.require_approval(inc.ticket, summary);
                        }
                        Action::Defer => {
                            s.defer(inc.ticket);
                        }
                    }
                }
                thread::sleep(Duration::from_millis(5));
            }
        });
        FakeUi {
            server,
            stop,
            requests,
            events,
            thread: Some(thread),
        }
    }

    pub fn server(&self) -> &ControlServer {
        &self.server
    }

    /// Every request that reached the UI so far, in order.
    pub fn requests(&self) -> Vec<Request> {
        self.requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Every `UiEvent` seen so far.
    pub fn events(&self) -> Vec<UiEvent> {
        self.events
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl Drop for FakeUi {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        // The last Arc drops here and shuts the server down.
    }
}
