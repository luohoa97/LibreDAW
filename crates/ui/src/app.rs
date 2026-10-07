// SPDX-License-Identifier: GPL-3.0-or-later
//! Shared application state on the GTK thread: the `Session` (document,
//! undo, engine glue) plus what the user has selected. Every widget holds an
//! `Rc<App>`; every user action goes through `App::edit` (which is
//! `Session::submit`, which is `apply()`), so the keyboard, the mouse, the
//! control socket, and scripts all take the same path.
//!
//! Widgets register a refresh callback with `on_change`; `App` calls them
//! after every change. Callbacks only read state and queue redraws, they never
//! call back into `App::edit`.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::Ordering;

use protocol::edit::Edit;
use protocol::engine::EngineCommand;
use protocol::ids::{ChannelId, PatternId, TrackId};

use crate::history::{Applied, Author, HistoryError, Scope, Submitted};
use crate::persist::ViewState;
use crate::session::Session;

/// What the user picked. Not part of the document and not undoable (6).
pub struct UiState {
    /// Where the project is saved; `None` until the first save.
    pub path: Option<PathBuf>,
    pub pattern: Option<PatternId>,
    pub channel: Option<ChannelId>,
    pub track: TrackId,
    /// Transport as the user asked for it.
    pub playing: bool,
    /// Why audio is off, if it is.
    pub audio_error: Option<String>,
}

/// How the window reads and restores its view (`.view.toml`).
struct ViewHooks {
    collect: Box<dyn Fn() -> ViewState>,
    apply: Box<dyn Fn(&ViewState)>,
}

type Toaster = Rc<dyn Fn(&str)>;

pub struct App {
    pub session: RefCell<Session>,
    pub ui: RefCell<UiState>,
    pub tasks: crate::tasks::Tasks,
    pub dirs: crate::persist::Dirs,
    /// Names this run's recovery bundle.
    pub session_id: String,
    /// A crashed session's recovery bundle to delete once ours exists.
    pub stale_recovery: RefCell<Option<PathBuf>>,
    view_hooks: RefCell<Option<ViewHooks>>,
    listeners: RefCell<Vec<Rc<dyn Fn()>>>,
    toaster: RefCell<Option<Toaster>>,
    notifying: Cell<bool>,
}

impl App {
    pub fn new(session: Session) -> Rc<App> {
        let app = Rc::new(App {
            session: RefCell::new(session),
            tasks: crate::tasks::Tasks::new(),
            dirs: crate::files::real_dirs(),
            session_id: crate::persist::session_id(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
                std::process::id(),
            ),
            stale_recovery: RefCell::new(None),
            view_hooks: RefCell::new(None),
            ui: RefCell::new(UiState {
                path: None,
                pattern: None,
                channel: None,
                track: TrackId::MASTER,
                playing: false,
                audio_error: None,
            }),
            listeners: RefCell::new(Vec::new()),
            toaster: RefCell::new(None),
            notifying: Cell::new(false),
        });
        app.fix_selection();
        app
    }

    /// The window registers how to read and restore its view.
    pub fn set_view_hooks(
        &self,
        collect: impl Fn() -> ViewState + 'static,
        apply: impl Fn(&ViewState) + 'static,
    ) {
        *self.view_hooks.borrow_mut() = Some(ViewHooks {
            collect: Box::new(collect),
            apply: Box::new(apply),
        });
    }

    pub fn collect_view(&self) -> Option<ViewState> {
        let h = self.view_hooks.borrow();
        h.as_ref().map(|h| {
            let mut v = (h.collect)();
            let ui = self.ui.borrow();
            v.pattern = ui.pattern.map(|p| p.0);
            v.channel = ui.channel.map(|c| c.0);
            v.track = Some(ui.track.0);
            v
        })
    }

    /// Restores the selection and the window view from a `.view.toml`.
    pub fn apply_view(&self, v: &ViewState) {
        {
            let s = self.session.borrow();
            let p = &s.document().project;
            let mut ui = self.ui.borrow_mut();
            if let Some(id) = v.pattern.map(PatternId)
                && p.pattern(id).is_some()
            {
                ui.pattern = Some(id);
            }
            if let Some(id) = v.channel.map(ChannelId)
                && p.channel(id).is_some()
            {
                ui.channel = Some(id);
            }
        }
        if let Some(h) = self.view_hooks.borrow().as_ref() {
            (h.apply)(v);
        }
    }

    pub fn on_change(&self, f: impl Fn() + 'static) {
        self.listeners.borrow_mut().push(Rc::new(f));
    }

    pub fn set_toaster(&self, f: impl Fn(&str) + 'static) {
        *self.toaster.borrow_mut() = Some(Rc::new(f));
    }

    pub fn toast(&self, msg: &str) {
        let t = self.toaster.borrow().clone();
        match t {
            Some(t) => t(msg),
            None => eprintln!("libredaw: {msg}"),
        }
    }

    /// Keeps the selection pointing at things that exist.
    pub fn fix_selection(&self) {
        let s = self.session.borrow();
        let p = &s.document().project;
        let mut ui = self.ui.borrow_mut();
        if ui.pattern.is_none_or(|id| p.pattern(id).is_none()) {
            ui.pattern = p.patterns.first().map(|x| x.id);
        }
        if ui.channel.is_none_or(|id| p.channel(id).is_none()) {
            ui.channel = p.channels.first().map(|x| x.id);
        }
        if p.track(ui.track).is_none() {
            ui.track = TrackId::MASTER;
        }
    }

    /// Runs after a document change: fixes the selection, shows messages
    /// from the session, and refreshes widgets.
    pub fn notify(&self) {
        if self.notifying.replace(true) {
            return; // a refresh callback asked for another refresh
        }
        self.fix_selection();
        let msgs = self.session.borrow_mut().take_messages();
        for m in msgs {
            self.toast(&m);
        }
        let ls: Vec<_> = self.listeners.borrow().clone();
        for l in ls {
            l();
        }
        self.notifying.set(false);
    }

    pub fn select_channel(&self, id: ChannelId) {
        let track = {
            let s = self.session.borrow();
            s.document().project.channel(id).map(|c| c.track)
        };
        if let Some(t) = track {
            let mut ui = self.ui.borrow_mut();
            ui.channel = Some(id);
            ui.track = t;
        }
        self.notify();
    }

    pub fn select_pattern(&self, id: PatternId) {
        self.ui.borrow_mut().pattern = Some(id);
        let playing = self.ui.borrow().playing;
        if playing {
            self.send_pattern();
        }
        self.notify();
    }

    pub fn select_track(&self, id: TrackId) {
        self.ui.borrow_mut().track = id;
        self.notify();
    }

    // ---- editing: the one path ----

    /// Applies edits as one undo group. Errors become a toast.
    pub fn edit(&self, edits: Vec<Edit>) -> Option<Applied> {
        self.edit_inner(edits, true)
    }

    /// As `edit`, for keyboard repeats that may legitimately hit a limit.
    pub fn edit_quiet(&self, edits: Vec<Edit>) -> Option<Applied> {
        self.edit_inner(edits, false)
    }

    fn edit_inner(&self, edits: Vec<Edit>, loud: bool) -> Option<Applied> {
        let r = self
            .session
            .borrow_mut()
            .submit(Author::User, None, edits, 0);
        match r {
            Ok(Submitted::Applied(a)) => {
                self.notify();
                Some(a)
            }
            Ok(Submitted::Queued) => None,
            Err(e) => {
                if loud {
                    self.toast(&e.to_string());
                }
                None
            }
        }
    }

    pub fn gesture_begin(&self, description: &str) -> bool {
        self.session
            .borrow_mut()
            .begin_gesture(Author::User, description)
    }

    pub fn gesture_edit(&self, edits: Vec<Edit>) -> Option<Applied> {
        let r = self.session.borrow_mut().gesture_edit(&edits);
        match r {
            Ok(a) => {
                self.notify();
                Some(a)
            }
            Err(e) => {
                self.toast(&e.to_string());
                None
            }
        }
    }

    pub fn gesture_end(&self) {
        let done = self.session.borrow_mut().end_gesture();
        for d in done {
            if let Err(e) = d.result {
                self.toast(&e.to_string());
            }
        }
        self.notify();
    }

    pub fn undo(&self) {
        let r = self.session.borrow_mut().undo(&Scope::Any);
        self.after_history(r);
    }

    pub fn redo(&self) {
        let r = self.session.borrow_mut().redo(&Scope::Any);
        self.after_history(r);
    }

    fn after_history(&self, r: Result<(), HistoryError>) {
        match r {
            Ok(()) => self.notify(),
            Err(HistoryError::GestureOpen) => {}
            Err(_) => {}
        }
    }

    pub fn can_undo(&self) -> bool {
        self.session.borrow().editor.can_undo(&Scope::Any)
    }

    pub fn can_redo(&self) -> bool {
        self.session.borrow().editor.can_redo(&Scope::Any)
    }

    pub fn is_dirty(&self) -> bool {
        self.session.borrow().editor.is_dirty()
    }

    // ---- transport ----

    fn send_pattern(&self) {
        let p = self.ui.borrow().pattern;
        if let Some(p) = p {
            let _ = self
                .session
                .borrow_mut()
                .link
                .command(EngineCommand::SetPlayingPattern { pattern: p });
        }
    }

    pub fn play(&self) {
        self.send_pattern();
        let r = self.session.borrow_mut().link.command(EngineCommand::Play);
        if r.is_ok() {
            self.ui.borrow_mut().playing = true;
        } else {
            self.toast("The audio engine is busy; try again");
        }
        self.notify();
    }

    pub fn stop(&self) {
        let _ = self.session.borrow_mut().link.command(EngineCommand::Stop);
        self.ui.borrow_mut().playing = false;
        self.notify();
    }

    pub fn toggle_play(&self) {
        let playing = self.ui.borrow().playing;
        if playing {
            self.stop();
        } else {
            self.play();
        }
    }

    pub fn playhead_tick(&self) -> u64 {
        self.session
            .borrow()
            .link
            .status
            .playhead_tick
            .load(Ordering::Relaxed)
    }

    /// The pattern the roll and step grid show.
    pub fn current_pattern(&self) -> Option<PatternId> {
        self.ui.borrow().pattern
    }

    pub fn current_channel(&self) -> Option<ChannelId> {
        self.ui.borrow().channel
    }
}
