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

use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};
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
    /// Branch the entry was made on (15.12). The base of a branch belongs
    /// to the branch it was taken from.
    branch: String,
    /// `next_id` of the document at this entry (stored in the commit).
    next_id: u32,
    /// Commit hash once the entry is on disk (15.11).
    hash: Option<String>,
}

/// A named line of work (15.12).
#[derive(Clone, Debug)]
struct Branch {
    name: String,
    /// The newest entry made on the branch.
    head: EntryId,
    /// The entry the branch started from.
    base: EntryId,
    author: String,
    archived: bool,
}

/// The branch every project starts on.
pub const MAIN_BRANCH: &str = "main";

/// Why a branch or version operation failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BranchError {
    /// A gesture is open; the document cannot be replaced now.
    GestureOpen,
    UnknownBranch(String),
    UnknownCommit(String),
    /// Empty, too long, or with control characters.
    BadName,
    /// The branch is the current one.
    IsCurrent,
}

impl std::fmt::Display for BranchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BranchError::GestureOpen => write!(f, "a gesture is in progress"),
            BranchError::UnknownBranch(b) => write!(f, "no branch {b}"),
            BranchError::UnknownCommit(c) => write!(f, "no commit {c}"),
            BranchError::BadName => write!(f, "invalid name"),
            BranchError::IsCurrent => write!(f, "the branch is the current one"),
        }
    }
}

impl std::error::Error for BranchError {}

fn check_label(name: &str) -> Result<String, BranchError> {
    let n = name.trim();
    if n.is_empty()
        || n.chars().count() > protocol::consts::MAX_NAME_CHARS
        || n.chars().any(char::is_control)
    {
        return Err(BranchError::BadName);
    }
    Ok(n.to_string())
}

/// The snapshot tree.
pub struct History {
    entries: BTreeMap<EntryId, Entry>,
    current: EntryId,
    next_entry: EntryId,
    limits: Limits,
    branches: BTreeMap<String, Branch>,
    current_branch: String,
    /// Named versions: slug -> (name, entry).
    versions: BTreeMap<String, (String, EntryId)>,
    /// Entries not yet written to disk (only with persistence on).
    dirty: BTreeSet<EntryId>,
    /// Branch, version, or HEAD changed since the last batch.
    refs_dirty: bool,
    persist: bool,
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
                branch: MAIN_BRANCH.into(),
                next_id: 0,
                hash: None,
            },
        );
        let mut branches = BTreeMap::new();
        branches.insert(
            MAIN_BRANCH.to_string(),
            Branch {
                name: "Main".into(),
                head: 0,
                base: 0,
                author: Author::User.tag(),
                archived: false,
            },
        );
        History {
            entries,
            current: 0,
            next_entry: 1,
            limits,
            branches,
            current_branch: MAIN_BRANCH.into(),
            versions: BTreeMap::new(),
            dirty: BTreeSet::new(),
            refs_dirty: false,
            persist: false,
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
    fn push(&mut self, project: Arc<Project>, next_id: u32, author: Author, description: String) {
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
                author: author.clone(),
                description,
                unix_ms: now_ms(),
                branch: self.current_branch.clone(),
                next_id,
                hash: None,
            },
        );
        let parent = self.entries.get_mut(&cur).expect("current exists");
        parent.children.push(id);
        parent.redo = Some(id);
        self.current = id;
        if let Some(b) = self.branches.get_mut(&self.current_branch) {
            b.head = id;
        }
        self.mark_dirty(id);
        self.enforce_limits();
    }

    /// Replaces the project of the current entry (gesture steps, merges).
    fn replace_current(&mut self, project: Arc<Project>, next_id: u32) {
        let cur = self.current;
        let e = self.entries.get_mut(&cur).expect("current exists");
        e.project = project;
        e.next_id = next_id;
        self.mark_dirty(cur);
        self.enforce_limits();
    }

    fn mark_dirty(&mut self, id: EntryId) {
        if self.persist {
            self.dirty.insert(id);
            self.refs_dirty = true;
        }
    }

    fn can_undo(&self, scope: &Scope) -> Result<EntryId, HistoryError> {
        let e = &self.entries[&self.current];
        let parent = e.parent.ok_or(HistoryError::NothingToUndo)?;
        // The base of a branch belongs to the branch it was taken from:
        // undo never leaves the current branch (15.12).
        if e.branch != self.current_branch {
            return Err(HistoryError::NothingToUndo);
        }
        if !scope.allows(&e.author) {
            return Err(HistoryError::NotYours);
        }
        Ok(parent)
    }

    fn can_redo(&self, scope: &Scope) -> Result<EntryId, HistoryError> {
        let e = &self.entries[&self.current];
        let on_branch = |c: &EntryId| self.entries[c].branch == self.current_branch;
        let child = e
            .redo
            .filter(on_branch)
            .or_else(|| e.children.iter().rev().copied().find(on_branch))
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
        if self.persist {
            self.refs_dirty = true;
        }
    }

    /// Entries the memory limit must keep: the current one, branch heads
    /// and bases, named versions, and anything not yet on disk.
    fn is_protected(&self, id: EntryId) -> bool {
        id == self.current
            || self.dirty.contains(&id)
            || self.versions.values().any(|(_, e)| *e == id)
            || self
                .branches
                .iter()
                .any(|(n, b)| b.head == id && *n != self.current_branch)
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
                    total += p.notes.len() * std::mem::size_of::<protocol::model::Note>();
                }
            }
            total += e.project.clips.len() * std::mem::size_of::<protocol::model::Clip>();
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
                    for ins in &t.inserts {
                        let Insert::Clap(r) = ins else { continue };
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
            .find(|(id, e)| e.children.is_empty() && !self.is_protected(**id))
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
        if self.is_protected(root) {
            return false;
        }
        let e = self.entries.remove(&root).expect("root");
        let heir = e.children.first().copied();
        self.repoint_branches(root, heir);
        for c in e.children {
            self.entries.get_mut(&c).expect("child").parent = None;
        }
        true
    }

    /// A removed entry may be a branch's head or base: point those at the
    /// nearest entry that is left.
    fn repoint_branches(&mut self, gone: EntryId, to: Option<EntryId>) {
        for b in self.branches.values_mut() {
            let fallback = to.unwrap_or(b.head);
            if b.head == gone {
                b.head = fallback;
            }
            if b.base == gone {
                b.base = to.unwrap_or(b.head);
            }
        }
        if self.persist {
            self.refs_dirty = true;
        }
    }

    fn remove_leaf(&mut self, id: EntryId) {
        let e = self.entries.remove(&id).expect("leaf");
        self.repoint_branches(id, e.parent);
        if let Some(p) = e.parent
            && let Some(pe) = self.entries.get_mut(&p)
        {
            pe.children.retain(|c| *c != id);
            if pe.redo == Some(id) {
                pe.redo = pe.children.last().copied();
            }
        }
    }

    /// Plugin blobs and samples that any entry of the tree references, on
    /// any branch. Pass it to `bundle::save_keeping` so a save's garbage
    /// collection leaves what undo and redo can still reach (17.2).
    pub fn keep(&self) -> crate::bundle::Keep {
        let mut keep = crate::bundle::Keep::default();
        let mut seen: HashSet<*const Project> = HashSet::new();
        for e in self.entries.values() {
            if seen.insert(Arc::as_ptr(&e.project)) {
                keep.add_project(&e.project);
            }
        }
        keep
    }

    // ----- branches and versions (15.12) -----------------------------------

    pub fn current_branch(&self) -> &str {
        &self.current_branch
    }

    /// Name of an entry in the control API: its commit hash once it is on
    /// disk, else a provisional id.
    pub fn name_of(&self, id: EntryId) -> String {
        self.entries
            .get(&id)
            .and_then(|e| e.hash.clone())
            .unwrap_or_else(|| commit_name(id))
    }

    /// The entry a commit name stands for: a full hash, a hash prefix of at
    /// least 8 characters that is unique, or a provisional id.
    pub fn resolve(&self, name: &str) -> Option<EntryId> {
        if let Some((id, _)) = self
            .entries
            .iter()
            .find(|(_, e)| e.hash.as_deref() == Some(name))
        {
            return Some(*id);
        }
        if name.len() >= 8 {
            let mut hits = self
                .entries
                .iter()
                .filter(|(_, e)| e.hash.as_deref().is_some_and(|h| h.starts_with(name)));
            if let (Some((id, _)), None) = (hits.next(), hits.next()) {
                return Some(*id);
            }
        }
        let id = u64::from_str_radix(name, 16).ok()?;
        (name.len() == 16 && self.entries.contains_key(&id)).then_some(id)
    }

    pub fn project_of(&self, id: EntryId) -> Option<&Arc<Project>> {
        self.entries.get(&id).map(|e| &e.project)
    }

    /// A branch by id or, failing that, by exact display name.
    pub fn find_branch(&self, key: &str) -> Option<String> {
        if self.branches.contains_key(key) {
            return Some(key.to_string());
        }
        self.branches
            .iter()
            .find(|(_, b)| b.name == key)
            .map(|(id, _)| id.clone())
    }

    pub fn branch_infos(&self) -> Vec<protocol::control::BranchInfo> {
        self.branches
            .iter()
            .map(|(id, b)| protocol::control::BranchInfo {
                branch: id.clone(),
                name: b.name.clone(),
                head: self.name_of(b.head),
                base: self.name_of(b.base),
                author: b.author.clone(),
                archived: b.archived,
            })
            .collect()
    }

    /// Entries of the tree newest first, after `since` if given, at most
    /// `limit` (0 = all).
    pub fn nodes(
        &self,
        since: Option<EntryId>,
        limit: usize,
    ) -> Vec<protocol::control::HistoryNode> {
        let mut out: Vec<protocol::control::HistoryNode> = self
            .entries
            .iter()
            .rev()
            .filter(|(id, _)| since.is_none_or(|s| **id > s))
            .take(if limit == 0 { usize::MAX } else { limit })
            .map(|(id, e)| protocol::control::HistoryNode {
                commit: self.name_of(*id),
                parent: e.parent.map(|p| self.name_of(p)),
                branch: e.branch.clone(),
                author: e.author.tag(),
                description: e.description.clone(),
                unix_ms: e.unix_ms,
                name: self
                    .versions
                    .values()
                    .find(|(_, v)| v == id)
                    .map(|(n, _)| n.clone()),
            })
            .collect();
        out.shrink_to_fit();
        out
    }

    /// Starts a branch at `from` (default: the current entry) and makes it
    /// current. Edits now go onto it; undo stays on it (15.12).
    pub fn create_branch(
        &mut self,
        name: &str,
        author: &Author,
        from: Option<EntryId>,
    ) -> Result<String, BranchError> {
        let name = check_label(name)?;
        let from = from.unwrap_or(self.current);
        if !self.entries.contains_key(&from) {
            return Err(BranchError::UnknownCommit(commit_name(from)));
        }
        let base = crate::store::slugify(&name);
        let mut id = base.clone();
        let mut n = 2;
        while self.branches.contains_key(&id) {
            id = format!("{base}-{n}");
            n += 1;
        }
        self.branches.insert(
            id.clone(),
            Branch {
                name,
                head: from,
                base: from,
                author: author.tag(),
                archived: false,
            },
        );
        self.current_branch = id.clone();
        self.goto(from);
        self.refs_dirty |= self.persist;
        Ok(id)
    }

    /// Makes a branch current: its head becomes the current entry. An
    /// archived branch is brought back.
    pub fn switch_branch(&mut self, id: &str) -> Result<(), BranchError> {
        let head = self
            .branches
            .get_mut(id)
            .map(|b| {
                b.archived = false;
                b.head
            })
            .ok_or_else(|| BranchError::UnknownBranch(id.to_string()))?;
        self.current_branch = id.to_string();
        self.goto(head);
        self.refs_dirty |= self.persist;
        Ok(())
    }

    /// Puts the current branch and entry back (taking back a switch).
    /// False if either is gone.
    fn restore_position(&mut self, branch: &str, entry: EntryId) -> bool {
        if !self.branches.contains_key(branch) || !self.entries.contains_key(&entry) {
            return false;
        }
        self.current_branch = branch.to_string();
        self.goto(entry);
        self.refs_dirty |= self.persist;
        true
    }

    /// Replaces the current entry's project without making it a new
    /// version: same content with plugin state bytes filled in.
    fn set_project_quiet(&mut self, project: Arc<Project>) {
        let cur = self.current;
        if let Some(e) = self.entries.get_mut(&cur) {
            e.project = project;
        }
    }

    pub fn rename_branch(&mut self, id: &str, name: &str) -> Result<(), BranchError> {
        let name = check_label(name)?;
        self.branches
            .get_mut(id)
            .ok_or_else(|| BranchError::UnknownBranch(id.to_string()))?
            .name = name;
        self.refs_dirty |= self.persist;
        Ok(())
    }

    /// Hides a branch from the version list. Its entries stay.
    pub fn archive_branch(&mut self, id: &str) -> Result<(), BranchError> {
        if id == self.current_branch {
            return Err(BranchError::IsCurrent);
        }
        self.branches
            .get_mut(id)
            .ok_or_else(|| BranchError::UnknownBranch(id.to_string()))?
            .archived = true;
        self.refs_dirty |= self.persist;
        Ok(())
    }

    /// Names the current entry. Naming again moves the version.
    pub fn save_version(&mut self, name: &str) -> Result<String, BranchError> {
        let name = check_label(name)?;
        let slug = crate::store::slugify(&name);
        self.versions.insert(slug.clone(), (name, self.current));
        self.refs_dirty |= self.persist;
        Ok(slug)
    }

    pub fn versions(&self) -> Vec<(String, String, EntryId)> {
        self.versions
            .iter()
            .map(|(s, (n, e))| (s.clone(), n.clone(), *e))
            .collect()
    }

    /// The entry the current branch ends at.
    pub fn branch_head(&self) -> EntryId {
        self.branches
            .get(&self.current_branch)
            .map_or(self.current, |b| b.head)
    }

    // ----- persistence (15.11) ---------------------------------------------

    /// Starts tracking what is not on disk yet. Everything already in the
    /// tree counts as new.
    pub fn enable_persistence(&mut self) {
        self.persist = true;
        self.dirty = self.entries.keys().copied().collect();
        self.refs_dirty = true;
    }

    pub fn persistence_enabled(&self) -> bool {
        self.persist
    }

    /// True if a batch is waiting.
    pub fn has_pending(&self) -> bool {
        self.persist && (self.refs_dirty || !self.dirty.is_empty())
    }

    /// Takes the commits and refs that are not on disk, for the save
    /// worker. `skip` is the entry of an open gesture: it is still
    /// changing, so it waits.
    pub fn take_pending(&mut self, skip: Option<EntryId>) -> Option<crate::store::PendingBatch> {
        use crate::store::{PendingBatch, PendingBranch, PendingCommit};
        if !self.has_pending() {
            return None;
        }
        self.dirty.retain(|i| self.entries.contains_key(i));
        let ids: Vec<EntryId> = self
            .dirty
            .iter()
            .copied()
            .filter(|i| Some(*i) != skip)
            .collect();
        let commits = ids
            .iter()
            .map(|i| {
                let e = &self.entries[i];
                PendingCommit {
                    entry: *i,
                    project: e.project.clone(),
                    next_id: e.next_id,
                    parent: e.parent,
                    branch: e.branch.clone(),
                    author: e.author.tag(),
                    description: e.description.clone(),
                    unix_ms: e.unix_ms,
                }
            })
            .collect();
        for i in &ids {
            self.dirty.remove(i);
        }
        let batch = PendingBatch {
            commits,
            branches: self
                .branches
                .iter()
                .map(|(id, b)| PendingBranch {
                    id: id.clone(),
                    name: b.name.clone(),
                    head: b.head,
                    base: b.base,
                    author: b.author.clone(),
                    archived: b.archived,
                })
                .collect(),
            versions: self.versions.values().cloned().collect(),
            head: Some((self.current_branch.clone(), self.current)),
        };
        if skip.is_none() {
            self.refs_dirty = false;
        }
        Some(batch)
    }

    /// Records the hashes the worker wrote.
    pub fn mark_persisted(&mut self, written: &[(u64, String)]) {
        for (id, h) in written {
            if let Some(e) = self.entries.get_mut(id) {
                e.hash = Some(h.clone());
            }
        }
    }

    /// Entry to hash for every entry already on disk (seeds `Persister`).
    pub fn known_hashes(&self) -> std::collections::HashMap<u64, String> {
        self.entries
            .iter()
            .filter_map(|(id, e)| e.hash.clone().map(|h| (*id, h)))
            .collect()
    }

    /// Rebuilds the tree from a store. `None` if the store has no usable
    /// commit. A commit that cannot be loaded is left out together with
    /// its descendants. The current entry is HEAD, or the newest commit.
    pub fn restore(
        store: &mut crate::store::HistoryStore,
        limits: Limits,
    ) -> Result<Option<History>, crate::store::StoreError> {
        use std::collections::HashMap;
        let metas: Vec<(String, crate::store::CommitMeta)> = store
            .commits()
            .map(|(h, m)| (h.clone(), m.clone()))
            .collect();
        let mut children: HashMap<Option<String>, Vec<(u64, String)>> = HashMap::new();
        let known: HashSet<&str> = metas.iter().map(|(h, _)| h.as_str()).collect();
        for (h, m) in &metas {
            let parent = m.parent.clone().filter(|p| known.contains(p.as_str()));
            children
                .entry(parent)
                .or_default()
                .push((m.unix_ms, h.clone()));
        }
        for v in children.values_mut() {
            v.sort();
        }
        let mut entries: BTreeMap<EntryId, Entry> = BTreeMap::new();
        let mut ids: HashMap<String, EntryId> = HashMap::new();
        let mut queue: VecDeque<(Option<EntryId>, String)> = children
            .get(&None)
            .into_iter()
            .flatten()
            .map(|(_, h)| (None, h.clone()))
            .collect();
        let mut next: EntryId = 0;
        while let Some((parent, h)) = queue.pop_front() {
            let Ok((project, next_id)) = store.load_project(&h) else {
                continue;
            };
            let meta = store.commit(&h).expect("listed").clone();
            let id = next;
            next += 1;
            ids.insert(h.clone(), id);
            entries.insert(
                id,
                Entry {
                    parent,
                    children: Vec::new(),
                    redo: None,
                    project: Arc::new(project),
                    author: Author::from_tag(&meta.author).unwrap_or(Author::User),
                    description: meta.description,
                    unix_ms: meta.unix_ms,
                    branch: meta.branch,
                    next_id,
                    hash: Some(h.clone()),
                },
            );
            if let Some(p) = parent {
                let pe = entries.get_mut(&p).expect("parent loaded first");
                pe.children.push(id);
                pe.redo = Some(id);
            }
            for (_, c) in children.get(&Some(h)).into_iter().flatten() {
                queue.push_back((Some(id), c.clone()));
            }
        }
        if entries.is_empty() {
            return Ok(None);
        }
        let newest = *entries.keys().next_back().expect("not empty");
        let mut branches: BTreeMap<String, Branch> = BTreeMap::new();
        for b in store.branches() {
            if let (Some(head), Some(base)) = (ids.get(&b.head), ids.get(&b.base)) {
                branches.insert(
                    b.id.clone(),
                    Branch {
                        name: b.name.clone(),
                        head: *head,
                        base: *base,
                        author: b.author.clone(),
                        archived: b.archived,
                    },
                );
            }
        }
        // Branches that commits name but no ref file survived for.
        let labels: BTreeSet<String> = entries.values().map(|e| e.branch.clone()).collect();
        for l in labels {
            if branches.contains_key(&l) {
                continue;
            }
            let on: Vec<EntryId> = entries
                .iter()
                .filter(|(_, e)| e.branch == l)
                .map(|(i, _)| *i)
                .collect();
            let (first, head) = (on[0], *on.last().expect("not empty"));
            let base = entries[&first].parent.unwrap_or(first);
            branches.insert(
                l.clone(),
                Branch {
                    name: if l == MAIN_BRANCH {
                        "Main".into()
                    } else {
                        l.clone()
                    },
                    head,
                    base,
                    author: entries[&head].author.tag(),
                    archived: false,
                },
            );
        }
        let (mut current_branch, mut current) = (String::new(), newest);
        if let Some((b, c)) = store.head()
            && let Some(id) = ids.get(c)
        {
            current = *id;
            current_branch = b.to_string();
        }
        if !branches.contains_key(&current_branch) {
            current_branch = entries[&current].branch.clone();
        }
        let versions = store
            .versions()
            .filter_map(|v| {
                ids.get(&v.commit)
                    .map(|e| (v.slug.clone(), (v.name.clone(), *e)))
            })
            .collect();
        let mut h = History {
            entries,
            current,
            next_entry: next,
            limits,
            branches,
            current_branch,
            versions,
            dirty: BTreeSet::new(),
            refs_dirty: false,
            persist: true,
        };
        h.enforce_limits();
        Ok(Some(h))
    }

    /// Entries for the control API's `History` reply, oldest first.
    pub fn infos(&self) -> Vec<HistoryEntry> {
        self.entries
            .iter()
            .map(|(id, e)| HistoryEntry {
                commit: self.name_of(*id),
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
    /// Where branches were switched from, newest last (15.12).
    nav: Vec<(String, EntryId)>,
    /// Bundle directory to load plugin state bytes from after a move.
    blob_dir: Option<std::path::PathBuf>,
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
            nav: Vec::new(),
            blob_dir: None,
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
        self.hist.push(nd.project.clone(), nd.next_id, author, desc);
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
            self.hist.replace_current(nd.project.clone(), nd.next_id);
        } else {
            g.started = true;
            let (a, d) = (g.author.clone(), g.description.clone());
            self.hist.push(nd.project.clone(), nd.next_id, a, d);
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
        self.hist.push(
            nd.project.clone(),
            nd.next_id,
            author,
            description.to_string(),
        );
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
        self.sync_doc();
        Ok(())
    }

    pub fn redo(&mut self, scope: &Scope) -> Result<(), HistoryError> {
        if self.gesture.is_some() {
            return Err(HistoryError::GestureOpen);
        }
        let to = self.hist.can_redo(scope)?;
        self.hist.goto(to);
        self.sync_doc();
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
        self.hist.replace_current(nd.project.clone(), nd.next_id);
        self.doc = nd;
        self.merged_since_save = true;
        Ok(())
    }

    /// `merge_with` for plain edits.
    pub fn merge(&mut self, edits: &[Edit]) -> Result<(), EditError> {
        self.merge_with(|d| apply_batch(d, edits).map(|r| r.0))
    }

    /// Makes the document follow the history's current entry. `next_id`
    /// only grows, so ids made on another branch are never reused (17.1).
    fn sync_doc(&mut self) {
        if let Some(dir) = &self.blob_dir {
            let filled = crate::bundle::fill_state_bytes(self.hist.project(), dir);
            if !Arc::ptr_eq(&filled, self.hist.project()) {
                self.hist.set_project_quiet(filled);
            }
        }
        self.doc = self.doc.with_project(self.hist.project().clone());
    }

    // ----- persistence (15.11) ---------------------------------------------

    /// Turns on writing the tree to disk. Everything in the tree is new.
    /// `bundle` is where plugin state bytes are read from when an undo,
    /// redo, or branch switch lands on a snapshot loaded without them.
    pub fn enable_persistence(&mut self, bundle: Option<&std::path::Path>) {
        self.blob_dir = bundle.map(std::path::Path::to_path_buf);
        self.hist.enable_persistence();
    }

    pub fn has_pending_history(&self) -> bool {
        self.hist.has_pending()
    }

    /// The commits and refs not on disk yet, for the save worker (at most
    /// every 2 seconds, and on save and close). The entry of an open
    /// gesture waits until the gesture ends.
    pub fn take_pending_history(&mut self) -> Option<crate::store::PendingBatch> {
        let skip = (self.gesture.as_ref().is_some_and(|g| g.started)).then(|| self.hist.current());
        self.hist.take_pending(skip)
    }

    /// Records what the worker wrote.
    pub fn history_persisted(&mut self, written: &[(u64, String)]) {
        self.hist.mark_persisted(written);
    }

    /// Entry to hash for what is on disk, to seed the worker's `Persister`.
    pub fn known_hashes(&self) -> std::collections::HashMap<u64, String> {
        self.hist.known_hashes()
    }

    /// Rebuilds an editor from the persisted tree: the current document is
    /// HEAD. `saved` is the project in `project.toml`; if HEAD equals it,
    /// the editor starts clean. `None` if the store has no usable commit.
    pub fn restore(
        store: &mut crate::store::HistoryStore,
        limits: Limits,
        saved: Option<&Project>,
        bundle: Option<&std::path::Path>,
    ) -> Result<Option<Editor>, crate::store::StoreError> {
        let Some(hist) = History::restore(store, limits)? else {
            return Ok(None);
        };
        let blob_dir = bundle.map(std::path::Path::to_path_buf);
        let mut hist = hist;
        if let Some(dir) = &blob_dir {
            let filled = crate::bundle::fill_state_bytes(hist.project(), dir);
            hist.set_project_quiet(filled);
        }
        let project = hist.project().clone();
        let next_id = hist.entries[&hist.current].next_id;
        let clean = saved.is_some_and(|s| *project == *s);
        let doc = Document::from_project((*project).clone(), next_id);
        let doc = Document { project, ..doc };
        let saved = clean.then(|| hist.current());
        Ok(Some(Editor {
            doc,
            hist,
            gesture: None,
            queue: VecDeque::new(),
            saved,
            merged_since_save: false,
            nav: Vec::new(),
            blob_dir,
        }))
    }

    // ----- branches and versions (15.12) -----------------------------------

    pub fn current_branch(&self) -> &str {
        self.hist.current_branch()
    }

    pub fn branch_infos(&self) -> Vec<protocol::control::BranchInfo> {
        self.hist.branch_infos()
    }

    /// The tree newest first (`HistoryTree`), after commit `since`.
    pub fn history_nodes(
        &self,
        since: Option<&str>,
        limit: usize,
    ) -> Vec<protocol::control::HistoryNode> {
        let since = since.and_then(|s| self.hist.resolve(s));
        self.hist.nodes(since, limit)
    }

    /// Name of the current commit (`HistoryTree.head`).
    pub fn head_name(&self) -> String {
        self.hist.name_of(self.hist.current())
    }

    /// Starts a branch at `from` (default: now) and makes it current.
    /// Returns its id. Starting somewhere else replaces the document.
    pub fn create_branch(
        &mut self,
        author: &Author,
        name: &str,
        from: Option<&str>,
    ) -> Result<String, BranchError> {
        if self.gesture.is_some() {
            return Err(BranchError::GestureOpen);
        }
        let from = match from {
            Some(c) => Some(
                self.hist
                    .resolve(c)
                    .ok_or_else(|| BranchError::UnknownCommit(c.to_string()))?,
            ),
            None => None,
        };
        let moved = from.is_some_and(|f| f != self.hist.current());
        let before = (self.hist.current_branch().to_string(), self.hist.current());
        let id = self.hist.create_branch(name, author, from)?;
        self.nav.push(before);
        if moved {
            self.sync_doc();
        }
        Ok(id)
    }

    /// Makes a branch current (by id or name); its head becomes the project
    /// state. One step: `undo_switch` goes back.
    pub fn switch_branch(&mut self, key: &str) -> Result<(), BranchError> {
        if self.gesture.is_some() {
            return Err(BranchError::GestureOpen);
        }
        let id = self
            .hist
            .find_branch(key)
            .ok_or_else(|| BranchError::UnknownBranch(key.to_string()))?;
        let before = (self.hist.current_branch().to_string(), self.hist.current());
        self.hist.switch_branch(&id)?;
        self.nav.push(before);
        self.sync_doc();
        Ok(())
    }

    /// True if a branch switch or creation can be taken back.
    pub fn can_undo_switch(&self) -> bool {
        self.gesture.is_none() && !self.nav.is_empty()
    }

    /// Takes back the last branch switch or creation: the previous branch
    /// and entry are current again.
    pub fn undo_switch(&mut self) -> bool {
        if self.gesture.is_some() {
            return false;
        }
        let Some((branch, entry)) = self.nav.pop() else {
            return false;
        };
        if self.hist.restore_position(&branch, entry) {
            self.sync_doc();
            true
        } else {
            false
        }
    }

    pub fn rename_branch(&mut self, key: &str, name: &str) -> Result<(), BranchError> {
        let id = self
            .hist
            .find_branch(key)
            .ok_or_else(|| BranchError::UnknownBranch(key.to_string()))?;
        self.hist.rename_branch(&id, name)
    }

    pub fn archive_branch(&mut self, key: &str) -> Result<(), BranchError> {
        let id = self
            .hist
            .find_branch(key)
            .ok_or_else(|| BranchError::UnknownBranch(key.to_string()))?;
        self.hist.archive_branch(&id)
    }

    /// Names the current state (`VersionSave`).
    pub fn save_version(&mut self, name: &str) -> Result<String, BranchError> {
        self.hist.save_version(name)
    }

    /// Makes an older commit current again as a new commit on top of the
    /// current branch's head (`VersionRestore`). Nothing is lost.
    pub fn restore_version(
        &mut self,
        author: Author,
        commit: &str,
    ) -> Result<Applied, BranchError> {
        if self.gesture.is_some() {
            return Err(BranchError::GestureOpen);
        }
        let id = self
            .hist
            .resolve(commit)
            .ok_or_else(|| BranchError::UnknownCommit(commit.to_string()))?;
        let project = self.hist.project_of(id).cloned().expect("resolved");
        let label = self
            .hist
            .versions()
            .into_iter()
            .find(|(_, _, e)| *e == id)
            .map_or_else(|| self.hist.name_of(id), |(_, n, _)| n);
        let head = self.hist.branch_head();
        self.hist.goto(head);
        self.push_state(
            author,
            &format!("Restore {label}"),
            &Document {
                project,
                next_id: self.doc.next_id,
                revision: self.doc.revision,
            },
        );
        Ok(Applied {
            revision: self.doc.revision,
            created: Vec::new(),
        })
    }

    /// What changed between two commits, as short readable lines. Both must
    /// still be in memory; older ones come from `HistoryStore::load_project`
    /// and `diff::diff_projects`.
    pub fn diff(&self, from: &str, to: &str) -> Result<Vec<String>, BranchError> {
        let get = |c: &str| {
            self.hist
                .resolve(c)
                .and_then(|i| self.hist.project_of(i))
                .cloned()
                .ok_or_else(|| BranchError::UnknownCommit(c.to_string()))
        };
        Ok(crate::diff::diff_projects(&*get(from)?, &*get(to)?))
    }
}

#[cfg(test)]
mod branch_tests;
#[cfg(test)]
mod tests;
