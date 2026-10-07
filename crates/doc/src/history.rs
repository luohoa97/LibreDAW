// SPDX-License-Identifier: GPL-3.0-or-later
//! Undo and redo (SPEC 6, 17.1): a tree of project snapshots in memory.
//!
//! Every entry holds an `Arc<Project>` root, so snapshots share whatever an
//! edit did not touch. An edit made after an undo starts a new branch; no
//! branch is thrown away except by the memory limit. Undo moves to the
//! parent. Redo moves to the child most recently visited.
//!
//! `Editor` owns the current `Document`, the tree, the open gesture, and
//! the queue of script and agent batches that wait for the gesture to
//! close. All of it runs on the GTK thread; nothing here blocks.
//!
//! Writing the tree to disk is section 15.11 and comes later.

use std::collections::{BTreeMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use protocol::control::HistoryEntry;
use protocol::edit::{Edit, EditError};
use protocol::model::{Insert, Instrument, Pattern, Project};

use crate::document::{Document, apply_batch, apply_batch_indexed};

/// Who made a change (15.11, 17.1).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Author {
    User,
    Script,
    /// An agent session, named by the id the control server gave it.
    Agent(String),
}

impl Author {
    /// `user`, `script`, or `agent:<session>`.
    pub fn tag(&self) -> String {
        match self {
            Author::User => "user".into(),
            Author::Script => "script".into(),
            Author::Agent(s) => format!("agent:{s}"),
        }
    }

    pub fn from_tag(t: &str) -> Option<Author> {
        match t {
            "user" => Some(Author::User),
            "script" => Some(Author::Script),
            _ => t
                .strip_prefix("agent:")
                .filter(|s| !s.is_empty())
                .map(|s| Author::Agent(s.to_string())),
        }
    }
}

/// Which entries an undo or redo may cross (17.1). Agents only move over
/// their own session's commits; the user can undo anything.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Scope {
    Any,
    Only(Author),
}

impl Scope {
    fn allows(&self, a: &Author) -> bool {
        match self {
            Scope::Any => true,
            Scope::Only(x) => x == a,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HistoryError {
    /// A gesture is open; undo and redo are disabled (6).
    GestureOpen,
    NothingToUndo,
    NothingToRedo,
    /// The entry belongs to another author and the scope forbids it.
    NotYours,
}

impl std::fmt::Display for HistoryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HistoryError::GestureOpen => write!(f, "a gesture is in progress"),
            HistoryError::NothingToUndo => write!(f, "nothing to undo"),
            HistoryError::NothingToRedo => write!(f, "nothing to redo"),
            HistoryError::NotYours => write!(f, "the next step was made by someone else"),
        }
    }
}

impl std::error::Error for HistoryError {}

/// Memory limit of section 6: 200 entries or 256 MiB, whichever is first.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max_entries: usize,
    pub max_bytes: usize,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits {
            max_entries: 200,
            max_bytes: 256 << 20,
        }
    }
}

pub type EntryId = u64;

struct Entry {
    parent: Option<EntryId>,
    children: Vec<EntryId>,
    /// Child that redo goes to: the one visited or created last.
    redo: Option<EntryId>,
    project: Arc<Project>,
    author: Author,
    description: String,
    unix_ms: u64,
}

/// The snapshot tree.
pub struct History {
    entries: BTreeMap<EntryId, Entry>,
    current: EntryId,
    next_entry: EntryId,
    limits: Limits,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl History {
    pub fn new(root: Arc<Project>, limits: Limits) -> History {
        let mut entries = BTreeMap::new();
        entries.insert(
            0,
            Entry {
                parent: None,
                children: Vec::new(),
                redo: None,
                project: root,
                author: Author::User,
                description: "Initial state".into(),
                unix_ms: now_ms(),
            },
        );
        History {
            entries,
            current: 0,
            next_entry: 1,
            limits,
        }
    }

    pub fn current(&self) -> EntryId {
        self.current
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn contains(&self, id: EntryId) -> bool {
        self.entries.contains_key(&id)
    }

    pub fn project(&self) -> &Arc<Project> {
        &self.entries[&self.current].project
    }

    pub fn author_of_current(&self) -> &Author {
        &self.entries[&self.current].author
    }

    /// Adds a child of the current entry and moves to it.
    fn push(&mut self, project: Arc<Project>, author: Author, description: String) {
        let id = self.next_entry;
        self.next_entry += 1;
        let cur = self.current;
        self.entries.insert(
            id,
            Entry {
                parent: Some(cur),
                children: Vec::new(),
                redo: None,
                project,
                author,
                description,
                unix_ms: now_ms(),
            },
        );
        let parent = self.entries.get_mut(&cur).expect("current exists");
        parent.children.push(id);
        parent.redo = Some(id);
        self.current = id;
        self.enforce_limits();
    }

    /// Replaces the project of the current entry (gesture steps, merges).
    fn replace_current(&mut self, project: Arc<Project>) {
        self.entries
            .get_mut(&self.current)
            .expect("current exists")
            .project = project;
        self.enforce_limits();
    }

    fn can_undo(&self, scope: &Scope) -> Result<EntryId, HistoryError> {
        let e = &self.entries[&self.current];
        let parent = e.parent.ok_or(HistoryError::NothingToUndo)?;
        if !scope.allows(&e.author) {
            return Err(HistoryError::NotYours);
        }
        Ok(parent)
    }

    fn can_redo(&self, scope: &Scope) -> Result<EntryId, HistoryError> {
        let e = &self.entries[&self.current];
        let child = e
            .redo
            .or_else(|| e.children.last().copied())
            .ok_or(HistoryError::NothingToRedo)?;
        if !scope.allows(&self.entries[&child].author) {
            return Err(HistoryError::NotYours);
        }
        Ok(child)
    }

    fn peek(&self, undo: bool, scope: &Scope) -> Option<Arc<Project>> {
        let to = if undo {
            self.can_undo(scope)
        } else {
            self.can_redo(scope)
        }
        .ok()?;
        Some(self.entries[&to].project.clone())
    }

    fn goto(&mut self, to: EntryId) {
        let from = self.current;
        // Moving up leaves the parent's redo pointer on the child we came
        // from, so redo returns there.
        if self.entries[&from].parent == Some(to) {
            self.entries.get_mut(&to).expect("target").redo = Some(from);
        }
        self.current = to;
    }

    /// Estimated bytes held by all snapshots. Each distinct pattern,
    /// channel, track and state blob counts once, however many snapshots
    /// share it (6).
    pub fn size_bytes(&self) -> usize {
        let mut pats: HashSet<*const Pattern> = HashSet::new();
        let mut others: HashSet<*const u8> = HashSet::new();
        let mut blobs: HashSet<*const u8> = HashSet::new();
        let mut total = 0usize;
        let mut blob = |r: &protocol::model::ClapRef, total: &mut usize| {
            if let Some(b) = &r.state_bytes
                && blobs.insert(b.as_ptr())
            {
                *total += b.len();
            }
        };
        for e in self.entries.values() {
            total += 256 + e.description.len();
            for p in &e.project.patterns {
                if pats.insert(Arc::as_ptr(p)) {
                    total += std::mem::size_of::<Pattern>() + p.name.len();
                    for cn in &p.notes {
                        total += std::mem::size_of::<protocol::model::ChannelNotes>()
                            + cn.notes.len() * std::mem::size_of::<protocol::model::Note>();
                    }
                }
            }
            for c in &e.project.channels {
                if others.insert(Arc::as_ptr(c) as *const u8) {
                    total += std::mem::size_of::<protocol::model::Channel>() + c.name.len();
                    if let Instrument::Clap(r) = &c.instrument {
                        total += r.params.len() * 16;
                        blob(r, &mut total);
                    }
                }
            }
            for t in &e.project.tracks {
                if others.insert(Arc::as_ptr(t) as *const u8) {
                    total += std::mem::size_of::<protocol::model::Track>() + t.name.len();
                    for Insert::Clap(r) in &t.inserts {
                        total += r.params.len() * 16;
                        blob(r, &mut total);
                    }
                }
            }
        }
        total
    }

    fn enforce_limits(&mut self) {
        while self.entries.len() > 1
            && (self.entries.len() > self.limits.max_entries
                || self.size_bytes() > self.limits.max_bytes)
        {
            if !self.evict_one() {
                break;
            }
        }
    }

    /// Drops the oldest leaf that is not the current entry. If the tree is
    /// a single chain ending at the current entry, drops its root.
    fn evict_one(&mut self) -> bool {
        let leaf = self
            .entries
            .iter()
            .find(|(id, e)| e.children.is_empty() && **id != self.current)
            .map(|(id, _)| *id);
        if let Some(id) = leaf {
            self.remove_leaf(id);
            return true;
        }
        // Chain ending at current: remove the root; its only child becomes
        // the root.
        let root = self
            .entries
            .iter()
            .find(|(_, e)| e.parent.is_none())
            .map(|(id, _)| *id)
            .expect("a root exists");
        if root == self.current {
            return false;
        }
        let e = self.entries.remove(&root).expect("root");
        for c in e.children {
            self.entries.get_mut(&c).expect("child").parent = None;
        }
        true
    }

    fn remove_leaf(&mut self, id: EntryId) {
        let e = self.entries.remove(&id).expect("leaf");
        if let Some(p) = e.parent
            && let Some(pe) = self.entries.get_mut(&p)
        {
            pe.children.retain(|c| *c != id);
            if pe.redo == Some(id) {
                pe.redo = pe.children.last().copied();
            }
        }
    }

    /// Entries for the control API's `History` reply, oldest first.
    pub fn infos(&self) -> Vec<HistoryEntry> {
        self.entries
            .iter()
            .map(|(id, e)| HistoryEntry {
                commit: commit_name(*id),
                author: e.author.tag(),
                description: e.description.clone(),
                unix_ms: e.unix_ms,
                current: *id == self.current,
            })
            .collect()
    }

    /// Checks the tree's structure. Used by tests.
    pub fn check_integrity(&self) -> Result<(), String> {
        if !self.entries.contains_key(&self.current) {
            return Err("current entry missing".into());
        }
        let mut roots = 0;
        for (id, e) in &self.entries {
            match e.parent {
                None => roots += 1,
                Some(p) => match self.entries.get(&p) {
                    Some(pe) if pe.children.contains(id) => {}
                    _ => return Err(format!("entry {id}: parent link broken")),
                },
            }
            for c in &e.children {
                match self.entries.get(c) {
                    Some(ce) if ce.parent == Some(*id) => {}
                    _ => return Err(format!("entry {id}: child {c} link broken")),
                }
            }
            if let Some(r) = e.redo
                && !e.children.contains(&r)
            {
                return Err(format!("entry {id}: redo points outside children"));
            }
        }
        if roots == 0 {
            return Err("no root".into());
        }
        // The current entry must reach a root.
        let mut at = self.current;
        let mut hops = 0;
        while let Some(p) = self.entries[&at].parent {
            at = p;
            hops += 1;
            if hops > self.entries.len() {
                return Err("parent cycle".into());
            }
        }
        Ok(())
    }
}

/// Commit name used in the control API (15.11 uses content hashes later).
pub fn commit_name(id: EntryId) -> String {
    format!("{id:016x}")
}

/// Short name of an edit for history rows: its variant name.
pub fn describe_edit(e: &Edit) -> String {
    let s = format!("{e:?}");
    let end = s.find(|c: char| !c.is_alphanumeric()).unwrap_or(s.len());
    s[..end].to_string()
}

fn describe_batch(edits: &[Edit]) -> String {
    match edits {
        [] => String::new(),
        [one] => describe_edit(one),
        [first, rest @ ..] => format!("{} and {} more", describe_edit(first), rest.len()),
    }
}

// ---------------------------------------------------------------------------

struct Gesture {
    author: Author,
    description: String,
    /// True once the gesture's first edit made its entry.
    started: bool,
}

/// A script or agent batch waiting for the gesture to close.
#[derive(Clone, Debug)]
pub struct Queued {
    /// Chosen by the submitter; echoed in `Done`.
    pub token: u64,
    pub author: Author,
    pub description: String,
    pub edits: Vec<Edit>,
}

/// Outcome of a queued batch once it ran.
#[derive(Debug)]
pub struct Done {
    pub token: u64,
    pub result: Result<Applied, EditFailure>,
}

/// A rejected batch: the error and the position of the edit that caused it
/// (`None` if it cannot be pinned to one edit).
#[derive(Clone, Debug, PartialEq)]
pub struct EditFailure {
    pub index: Option<u32>,
    pub error: EditError,
}

impl From<(Option<u32>, EditError)> for EditFailure {
    fn from((index, error): (Option<u32>, EditError)) -> EditFailure {
        EditFailure { index, error }
    }
}

impl std::fmt::Display for EditFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.index {
            Some(i) => write!(f, "edit {i}: {}", self.error),
            None => write!(f, "{}", self.error),
        }
    }
}

impl std::error::Error for EditFailure {}

#[derive(Clone, Debug, PartialEq)]
pub struct Applied {
    pub revision: u64,
    pub created: Vec<u32>,
}

#[derive(Debug)]
pub enum Submitted {
    Applied(Applied),
    /// A gesture is open; the batch runs after it closes.
    Queued,
}

/// The document, its history, the open gesture, and the waiting batches.
pub struct Editor {
    doc: Document,
    hist: History,
    gesture: Option<Gesture>,
    queue: VecDeque<Queued>,
    /// The entry that is on disk. `None` for a never-saved document.
    saved: Option<EntryId>,
    /// Non-undoable merges since the last save.
    merged_since_save: bool,
}

impl Editor {
    /// A freshly opened or new document. It counts as saved if `clean`.
    pub fn new(doc: Document, clean: bool) -> Editor {
        Editor::with_limits(doc, clean, Limits::default())
    }

    pub fn with_limits(doc: Document, clean: bool, limits: Limits) -> Editor {
        let hist = History::new(doc.project.clone(), limits);
        let saved = clean.then(|| hist.current());
        Editor {
            doc,
            hist,
            gesture: None,
            queue: VecDeque::new(),
            saved,
            merged_since_save: false,
        }
    }

    pub fn document(&self) -> &Document {
        &self.doc
    }

    pub fn history(&self) -> &History {
        &self.hist
    }

    pub fn gesture_open(&self) -> bool {
        self.gesture.is_some()
    }

    pub fn queue_len(&self) -> usize {
        self.queue.len()
    }

    pub fn is_dirty(&self) -> bool {
        self.merged_since_save || self.saved != Some(self.hist.current())
    }

    /// Call after a successful save of the current document.
    pub fn mark_saved(&mut self) {
        self.saved = Some(self.hist.current());
        self.merged_since_save = false;
    }

    pub fn can_undo(&self, scope: &Scope) -> bool {
        self.gesture.is_none() && self.hist.can_undo(scope).is_ok()
    }

    pub fn can_redo(&self, scope: &Scope) -> bool {
        self.gesture.is_none() && self.hist.can_redo(scope).is_ok()
    }

    /// Applies a batch as one undo group, or queues it behind the open
    /// gesture. A batch from the gesture's own author joins the gesture.
    pub fn submit(
        &mut self,
        author: Author,
        description: Option<&str>,
        edits: Vec<Edit>,
        token: u64,
    ) -> Result<Submitted, EditFailure> {
        let desc = description
            .map(str::to_string)
            .unwrap_or_else(|| describe_batch(&edits));
        if let Some(g) = &self.gesture {
            if g.author != author {
                self.queue.push_back(Queued {
                    token,
                    author,
                    description: desc,
                    edits,
                });
                return Ok(Submitted::Queued);
            }
            return self.gesture_edit(&edits).map(Submitted::Applied);
        }
        self.apply_group(author, desc, &edits)
            .map(Submitted::Applied)
    }

    /// Applies a batch as a new undo group right now. Fails if a gesture is
    /// open (use `submit`).
    fn apply_group(
        &mut self,
        author: Author,
        desc: String,
        edits: &[Edit],
    ) -> Result<Applied, EditFailure> {
        if edits.is_empty() {
            return Ok(Applied {
                revision: self.doc.revision,
                created: Vec::new(),
            });
        }
        let (nd, created) = apply_batch_indexed(&self.doc, edits).map_err(EditFailure::from)?;
        self.hist.push(nd.project.clone(), author, desc);
        self.doc = nd;
        Ok(Applied {
            revision: self.doc.revision,
            created,
        })
    }

    /// Opens a gesture (6): a fader drag, a note drag, or a plugin's
    /// `begin_gesture`. Returns false if one is already open.
    pub fn begin_gesture(&mut self, author: Author, description: &str) -> bool {
        if self.gesture.is_some() {
            return false;
        }
        self.gesture = Some(Gesture {
            author,
            description: description.to_string(),
            started: false,
        });
        true
    }

    /// An edit inside the open gesture. The first one makes the gesture's
    /// entry; later ones replace it, so the whole gesture is one step.
    pub fn gesture_edit(&mut self, edits: &[Edit]) -> Result<Applied, EditFailure> {
        let Some(g) = &mut self.gesture else {
            return Err(EditFailure {
                index: None,
                error: EditError::BadArgument {
                    what: "no gesture is open".into(),
                },
            });
        };
        if edits.is_empty() {
            return Ok(Applied {
                revision: self.doc.revision,
                created: Vec::new(),
            });
        }
        let (nd, created) = apply_batch_indexed(&self.doc, edits).map_err(EditFailure::from)?;
        if g.started {
            self.hist.replace_current(nd.project.clone());
        } else {
            g.started = true;
            let (a, d) = (g.author.clone(), g.description.clone());
            self.hist.push(nd.project.clone(), a, d);
        }
        self.doc = nd;
        Ok(Applied {
            revision: self.doc.revision,
            created,
        })
    }

    /// Closes the gesture and runs the queued batches in order, each as its
    /// own group. A batch that fails is reported and the rest still run.
    pub fn end_gesture(&mut self) -> Vec<Done> {
        if self.gesture.take().is_none() {
            return Vec::new();
        }
        let mut done = Vec::new();
        while let Some(q) = self.queue.pop_front() {
            let result = self.apply_group(q.author, q.description, &q.edits);
            done.push(Done {
                token: q.token,
                result,
            });
        }
        done
    }

    /// Removes a waiting batch (the control server's 10 s `Busy` timeout).
    /// Returns true if it was still queued.
    pub fn cancel_queued(&mut self, token: u64) -> bool {
        let before = self.queue.len();
        self.queue.retain(|q| q.token != token);
        self.queue.len() != before
    }

    /// Makes `other`'s project the current state as one undo step (recovery
    /// of autosaved work, Amendment 10). `next_id` only grows.
    pub fn push_state(&mut self, author: Author, description: &str, other: &Document) {
        let nd = Document {
            project: other.project.clone(),
            next_id: self
                .doc
                .next_id
                .max(other.next_id)
                .max(other.project.max_id().saturating_add(1)),
            revision: self.doc.revision + 1,
        };
        self.hist
            .push(nd.project.clone(), author, description.to_string());
        self.doc = nd;
    }

    /// The project an undo (or redo) would go to, without moving. Used to
    /// capture plugin state before a step that removes plugins (7.5).
    pub fn peek(&self, undo: bool, scope: &Scope) -> Option<Arc<Project>> {
        self.hist.peek(undo, scope)
    }

    /// Marks the document as having unsaved changes that are not edits
    /// (a plugin reported `state.mark_dirty`).
    pub fn touch(&mut self) {
        self.merged_since_save = true;
    }

    pub fn undo(&mut self, scope: &Scope) -> Result<(), HistoryError> {
        if self.gesture.is_some() {
            return Err(HistoryError::GestureOpen);
        }
        let to = self.hist.can_undo(scope)?;
        self.hist.goto(to);
        self.doc = self.doc.with_project(self.hist.project().clone());
        Ok(())
    }

    pub fn redo(&mut self, scope: &Scope) -> Result<(), HistoryError> {
        if self.gesture.is_some() {
            return Err(HistoryError::GestureOpen);
        }
        let to = self.hist.can_redo(scope)?;
        self.hist.goto(to);
        self.doc = self.doc.with_project(self.hist.project().clone());
        Ok(())
    }

    /// Changes the current document without making an undo step: captured
    /// plugin state (7.5), parameter sync after an event overflow (4.4).
    /// The result replaces the current entry's snapshot.
    pub fn merge_with(
        &mut self,
        f: impl FnOnce(&Document) -> Result<Document, EditError>,
    ) -> Result<(), EditError> {
        let nd = f(&self.doc)?;
        self.hist.replace_current(nd.project.clone());
        self.doc = nd;
        self.merged_since_save = true;
        Ok(())
    }

    /// `merge_with` for plain edits.
    pub fn merge(&mut self, edits: &[Edit]) -> Result<(), EditError> {
        self.merge_with(|d| apply_batch(d, edits).map(|r| r.0))
    }
}

#[cfg(test)]
mod tests;
