// SPDX-License-Identifier: GPL-3.0-or-later
//! Engine glue (SPEC 4.2, 4.3, 4.4, 7.5, 9.1, 17.1): everything that has to
//! happen on the GTK thread when the document is replaced, and the 10 ms
//! housekeeping tick.
//!
//! After every document replacement (edit, undo, redo, load, script batch)
//! `Session`:
//! 1. syncs slot allocation,
//! 2. rewrites every control and parameter value into the tables,
//! 3. queues parameter events for plugins whose recorded values changed
//!    (except when the plugin itself made the change),
//! 4. reconciles the plugin registry (create, attach, detach, drop),
//! 5. asks the compiler thread for a new `Compiled` unless only control
//!    values changed.
//!
//! Before an edit or undo removes a plugin, `Session` captures its state so
//! undoing the removal brings the patch back (7.5 (a)).
//!
//! No GTK types here: the app calls `tick()` from a
//! `glib::timeout_add_local(10 ms)` source.

use std::collections::{HashSet, VecDeque};
use std::sync::atomic::Ordering;
use std::time::Instant;

use protocol::edit::Edit;
use protocol::engine::{EngineEvent, PluginEvent, PluginSlot};
use protocol::ids::InstanceId;
use protocol::model::{Insert, Instrument, Project};

use crate::bundle::CaptureClock;
use crate::change::{needs_compile, param_diffs, removed_instances};
use crate::compiler::{CompileJob, Compiler};
use crate::document::{Document, commit_plugin_state};
use crate::engine_adapter::{Compiled, EngineLink, compile, write_controls};
use crate::history::{Applied, Author, Done, EditFailure, Editor, HistoryError, Scope, Submitted};
use crate::plugin_adapter::PluginOut;
use crate::registry::{Notice, Registry, clap_ref};
use crate::slots::SlotAllocator;

/// Who caused a document change, for parameter replay.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    /// The user, a script, an agent, undo, redo, load.
    External,
    /// A plugin reported its own parameter change: do not echo it back.
    Plugin,
}

/// What a `tick` did, for the caller to react to.
#[derive(Debug, Default)]
pub struct TickReport {
    /// Queued script or agent batches that ran because a gesture closed.
    pub done: Vec<Done>,
    /// The document changed (plugin parameter, queued batch, sync).
    pub changed: bool,
}

pub struct Session {
    pub editor: Editor,
    pub slots: SlotAllocator,
    pub link: EngineLink,
    pub registry: Registry,
    compiler: Compiler<Box<Compiled>>,
    out_events: VecDeque<PluginEvent>,
    seen_overflows: u64,
    plugin_gesture: bool,
    gesture_just_ended: bool,
    capture_clock: CaptureClock,
    /// Messages for the user (toasts), drained with `take_messages`.
    messages: Vec<String>,
}

fn instance_ids(p: &Project) -> HashSet<InstanceId> {
    let mut s = HashSet::new();
    for c in &p.channels {
        if let Instrument::Clap(r) = &c.instrument {
            s.insert(r.instance);
        }
    }
    for t in &p.tracks {
        for Insert::Clap(r) in &t.inserts {
            s.insert(r.instance);
        }
    }
    s
}

impl Session {
    pub fn new(doc: Document, clean: bool, link: EngineLink, registry: Registry) -> Session {
        let mut s = Session {
            editor: Editor::new(doc, clean),
            slots: SlotAllocator::new(),
            link,
            registry,
            compiler: Compiler::spawn(|j: &CompileJob| compile(j)),
            out_events: VecDeque::new(),
            seen_overflows: 0,
            plugin_gesture: false,
            gesture_just_ended: false,
            capture_clock: CaptureClock::standard(Instant::now()),
            messages: Vec::new(),
        };
        s.seen_overflows = s.link.status.event_overflows.load(Ordering::Relaxed);
        s.after_change(&Project::empty(), Origin::External);
        // The engine needs a first `Compiled` even for an empty project.
        s.request_compile();
        s
    }

    pub fn document(&self) -> &Document {
        self.editor.document()
    }

    pub fn take_messages(&mut self) -> Vec<String> {
        std::mem::take(&mut self.messages)
    }

    pub fn jobs_compiled(&self) -> u64 {
        self.compiler.jobs_compiled()
    }

    pub fn jobs_requested(&self) -> u64 {
        self.compiler.jobs_received()
    }

    pub fn compiler_idle(&self) -> bool {
        self.compiler.is_idle()
    }

    // ---- editing ----

    /// Applies a batch as one undo group (or queues it behind a gesture).
    /// Plugins the batch removes have their state captured first.
    pub fn submit(
        &mut self,
        author: Author,
        description: Option<&str>,
        edits: Vec<Edit>,
        token: u64,
    ) -> Result<Submitted, EditFailure> {
        let gone = removed_instances(&self.editor.document().project, &edits);
        self.capture(&gone);
        let old = self.editor.document().project.clone();
        let r = self.editor.submit(author, description, edits, token)?;
        if matches!(r, Submitted::Applied(_)) {
            self.after_change(&old, Origin::External);
        }
        Ok(r)
    }

    pub fn begin_gesture(&mut self, author: Author, description: &str) -> bool {
        self.editor.begin_gesture(author, description)
    }

    pub fn gesture_edit(&mut self, edits: &[Edit]) -> Result<Applied, EditFailure> {
        let old = self.editor.document().project.clone();
        let r = self.editor.gesture_edit(edits)?;
        self.after_change(&old, Origin::External);
        Ok(r)
    }

    /// Closes the gesture and runs the queued batches.
    pub fn end_gesture(&mut self) -> Vec<Done> {
        let old = self.editor.document().project.clone();
        let done = self.editor.end_gesture();
        if !done.is_empty() {
            self.after_change(&old, Origin::External);
        }
        done
    }

    pub fn undo(&mut self, scope: &Scope) -> Result<(), HistoryError> {
        self.step(true, scope)
    }

    pub fn redo(&mut self, scope: &Scope) -> Result<(), HistoryError> {
        self.step(false, scope)
    }

    fn step(&mut self, undo: bool, scope: &Scope) -> Result<(), HistoryError> {
        if self.editor.gesture_open() {
            return Err(HistoryError::GestureOpen);
        }
        if let Some(target) = self.editor.peek(undo, scope) {
            let cur = instance_ids(&self.editor.document().project);
            let keep = instance_ids(&target);
            let gone: Vec<_> = cur.difference(&keep).copied().collect();
            self.capture(&gone);
        }
        let old = self.editor.document().project.clone();
        if undo {
            self.editor.undo(scope)?;
        } else {
            self.editor.redo(scope)?;
        }
        self.after_change(&old, Origin::External);
        Ok(())
    }

    /// Replaces the whole document (new, open, recover).
    pub fn replace_document(&mut self, doc: Document, clean: bool) {
        let old = self.editor.document().project.clone();
        // Keep ids and revisions moving forward across projects so stale
        // control requests cannot match.
        let mut doc = doc;
        doc.revision = doc.revision.max(self.editor.document().revision + 1);
        self.editor = Editor::new(doc, clean);
        self.plugin_gesture = false;
        self.out_events.clear();
        // Old instances are not wanted by the new document.
        self.after_change(&old, Origin::External);
        let p = self.editor.document().project.clone();
        self.slots.sync(&p).ok();
        self.request_compile();
    }

    /// Applies autosaved work on top of the open project as one undoable
    /// step (Amendment 10).
    pub fn apply_recovered(&mut self, recovered: &Document) {
        let old = self.editor.document().project.clone();
        self.editor
            .push_state(Author::User, "Recovered unsaved work", recovered);
        self.after_change(&old, Origin::External);
    }

    // ---- plugin state (7.5) ----

    fn capture(&mut self, ids: &[InstanceId]) {
        for &id in ids {
            let Some(r) = clap_ref(&self.editor.document().project, id).cloned() else {
                continue;
            };
            match self.registry.capture(id, &r) {
                Ok(Some((name, bytes))) => {
                    let res = self
                        .editor
                        .merge_with(|d| commit_plugin_state(d, id, &name, bytes.clone(), None));
                    if let Err(e) = res {
                        self.messages
                            .push(format!("Could not record plugin state: {e}"));
                    }
                }
                Ok(None) => {}
                Err(e) => self
                    .messages
                    .push(format!("Could not save state of {}: {e}", r.plugin_id)),
            }
        }
    }

    /// Captures state of every live instance and returns the document to
    /// write: before an explicit save, and before handing a clone to the
    /// autosave worker (7.5 (b), (c)).
    pub fn snapshot_for_save(&mut self) -> Document {
        let ids: Vec<_> = instance_ids(&self.editor.document().project)
            .into_iter()
            .collect();
        self.capture(&ids);
        self.editor.document().clone()
    }

    // ---- the pipeline after a replacement ----

    fn request_compile(&mut self) {
        let d = self.editor.document();
        self.compiler.request(CompileJob {
            revision: d.revision,
            project: d.project.clone(),
            slots: self.slots.clone(),
            sample_rate: self.link.sample_rate(),
        });
    }

    fn after_change(&mut self, old: &Project, origin: Origin) {
        let new = self.editor.document().project.clone();
        if let Err(e) = self.slots.sync(&new) {
            self.messages.push(format!("Engine slots: {e}"));
        }
        write_controls(&new, &self.slots, &self.link);
        if origin == Origin::External {
            self.out_events.extend(param_diffs(old, &new, &self.slots));
        }
        for n in self.registry.reconcile(&new, &self.slots, &mut self.link) {
            let Notice::Unavailable {
                plugin_id, reason, ..
            } = n;
            self.messages
                .push(format!("Plugin {plugin_id} is unavailable: {reason}"));
        }
        if needs_compile(old, &new) {
            self.request_compile();
        }
    }

    // ---- the 10 ms tick (4.4) ----

    pub fn tick(&mut self) -> TickReport {
        self.tick_at(Instant::now())
    }

    /// `tick` with the time passed in, for tests.
    pub fn tick_at(&mut self, now: Instant) -> TickReport {
        let mut report = TickReport::default();

        // Discrete events from the audio thread.
        let mut events = Vec::new();
        self.link.drain_events(|e| events.push(e));
        let mut reconcile = false;
        for e in events {
            match e {
                EngineEvent::DetachAck { slot } => {
                    reconcile |= self.registry.on_detach_ack(slot);
                }
                EngineEvent::PluginParamChanged {
                    slot,
                    param_id,
                    value,
                } => self.plugin_param(slot, param_id, value, &mut report),
                EngineEvent::PluginGestureBegin { .. } => self.plugin_gesture_begin(),
                EngineEvent::PluginGestureEnd { .. } => self.plugin_gesture_end(&mut report),
                EngineEvent::Stopped { .. } => {}
            }
        }

        // Event ring overflow: re-read every parameter (4.4).
        let ov = self.link.status.event_overflows.load(Ordering::Relaxed);
        if ov != self.seen_overflows {
            self.seen_overflows = ov;
            self.resync_params(&mut report);
        }

        // Plugin housekeeping and what plugins did outside processing.
        let polled = self.registry.poll();
        for (id, out) in polled.changes {
            match out {
                PluginOut::Param { id: pid, value } => {
                    self.plugin_param_of(id, pid, value, &mut report)
                }
                PluginOut::GestureBegin { .. } => self.plugin_gesture_begin(),
                PluginOut::GestureEnd { .. } => self.plugin_gesture_end(&mut report),
            }
        }
        if !polled.dirty.is_empty() {
            self.editor.touch();
            self.capture_clock.plugin_dirty();
            report.changed = true;
        }
        // Plugin state: every 60 s if a plugin reported changes, and right
        // after a plugin gesture (7.5 (c), Amendment 10).
        if self.gesture_just_ended {
            self.gesture_just_ended = false;
            self.capture_clock.after_gesture(now);
        }
        if self.capture_clock.due(now) {
            let ids: Vec<_> = instance_ids(&self.editor.document().project)
                .into_iter()
                .collect();
            self.capture(&ids);
            report.changed = true;
        }

        if reconcile {
            let p = self.editor.document().project.clone();
            for n in self.registry.reconcile(&p, &self.slots, &mut self.link) {
                let Notice::Unavailable {
                    plugin_id, reason, ..
                } = n;
                self.messages
                    .push(format!("Plugin {plugin_id} is unavailable: {reason}"));
            }
        } else {
            // Retry attaches and detaches the command ring refused.
            let p = self.editor.document().project.clone();
            self.registry.reconcile(&p, &self.slots, &mut self.link);
        }

        // Rings: parameter events, then the newest compiled state.
        while let Some(e) = self.out_events.front().copied() {
            if self.link.plugin_event(e).is_ok() {
                self.out_events.pop_front();
            } else {
                break;
            }
        }
        if let Some((rev, c)) = self.compiler.take_ready()
            && let Err(c) = self.link.submit(c)
        {
            self.compiler.put_back(rev, c);
        }
        report
    }

    fn plugin_gesture_begin(&mut self) {
        if !self.plugin_gesture && self.editor.begin_gesture(Author::User, "Plugin parameter") {
            self.plugin_gesture = true;
        }
    }

    fn plugin_gesture_end(&mut self, report: &mut TickReport) {
        if self.plugin_gesture {
            self.plugin_gesture = false;
            self.gesture_just_ended = true;
            let done = self.end_gesture();
            report.changed = true;
            report.done.extend(done);
        }
    }

    fn plugin_param(&mut self, slot: PluginSlot, param: u32, value: f64, r: &mut TickReport) {
        if let Some(id) = self.registry.instance_at(slot) {
            self.plugin_param_of(id, param, value, r);
        }
    }

    /// A parameter the plugin changed itself. Inside a gesture it joins the
    /// gesture's undo step; outside one it is recorded without an undo step.
    fn plugin_param_of(&mut self, id: InstanceId, param: u32, value: f64, r: &mut TickReport) {
        let edit = Edit::SetPluginParam {
            instance: id,
            param_id: param,
            value,
        };
        let old = self.editor.document().project.clone();
        let ok = if self.plugin_gesture {
            self.editor
                .gesture_edit(std::slice::from_ref(&edit))
                .is_ok()
        } else {
            let ok = self.editor.merge(std::slice::from_ref(&edit)).is_ok();
            if ok {
                self.editor.touch();
            }
            ok
        };
        if ok {
            r.changed = true;
            self.after_change(&old, Origin::Plugin);
        }
    }

    fn resync_params(&mut self, r: &mut TickReport) {
        let mut edits = Vec::new();
        let p = self.editor.document().project.clone();
        for (id, pid, value, default) in self.registry.read_all_params() {
            let recorded = clap_ref(&p, id)
                .and_then(|c| c.params.iter().find(|v| v.id == pid))
                .map(|v| v.value);
            let differs = match recorded {
                Some(v) => v != value,
                None => value != default,
            };
            if differs {
                edits.push(Edit::SetPluginParam {
                    instance: id,
                    param_id: pid,
                    value,
                });
            }
        }
        if !edits.is_empty() && self.editor.merge(&edits).is_ok() {
            self.editor.touch();
            r.changed = true;
            self.after_change(&p, Origin::Plugin);
        }
    }

    /// Stops using plugins at exit. Call after the audio stream is stopped.
    pub fn shutdown(&mut self) {
        self.registry.drop_all();
    }
}

#[cfg(test)]
mod tests;
