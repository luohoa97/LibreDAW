// SPDX-License-Identifier: GPL-3.0-or-later
//! The persisted change tree (SPEC 15.11, 15.12): a content-addressed,
//! append-only store inside the bundle, written like git.
//!
//! ```text
//! Name.ldaw/history/
//!   objects/<hash>.toml   one node of the project tree (root, channel,
//!                         pattern, track), canonical text, named by the
//!                         hash of that text
//!   commits/<hash>.toml   parent, root object, branch, author, time,
//!                         next_id, description; named by its own hash
//!   branches/<id>.toml    name, head, base, author, archived
//!   refs/<slug>           named version: commit hash, then display name
//!   HEAD                  current branch and commit
//! ```
//!
//! Every file is written with the 7.4 procedure (temp file, fsync, rename,
//! directory fsync) and never changed once it is a commit or an object, so
//! a crash leaves complete old state plus possibly orphan files. A batch is
//! written in dependency order: objects, commits, branches, versions, HEAD.
//! A reader therefore never meets a reference to something not on disk.
//!
//! Choices where the spec leaves room:
//! - The hash is SHA-256 (already in the crate) instead of blake3, so no new
//!   dependency has to pass `cargo deny`. Only identity matters (15.11); the
//!   hash function can change with a new format version.
//! - `next_id` is stored in every commit (17.1), so a restored document
//!   never reuses an id.
//! - Plugin state blobs and samples are not stored here. Nodes name them,
//!   and `keep()` lists them so the 7.4 garbage collection leaves them.
//! - A commit whose parent is missing (after `compact`) is a root.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use protocol::control::{BranchInfo, HistoryNode};
use protocol::model::{Channel, Clip, LoopRegion, Metronome, Pattern, Project, SampleRef, Track};
use serde::{Deserialize, Serialize};

use crate::bundle::Keep;
use crate::sha256::sha256_hex;

pub const HISTORY_DIR: &str = "history";
const OBJECTS: &str = "objects";
const COMMITS: &str = "commits";
const BRANCHES: &str = "branches";
const REFS: &str = "refs";
const HEAD_FILE: &str = "HEAD";

/// Largest history file the loader reads.
const MAX_FILE_BYTES: u64 = 64 << 20;
/// Write caches are cleared when they grow past this many nodes.
const CACHE_LIMIT: usize = 8192;

#[derive(Debug)]
pub enum StoreError {
    Io {
        path: PathBuf,
        error: io::Error,
    },
    Corrupt(String),
    /// Test hook: the write stopped here, as a crash would.
    Crashed(StoreStep),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::Io { path, error } => write!(f, "{}: {error}", path.display()),
            StoreError::Corrupt(s) => write!(f, "corrupt history: {s}"),
            StoreError::Crashed(s) => write!(f, "simulated crash after {s:?}"),
        }
    }
}

impl std::error::Error for StoreError {}

fn io_err(path: &Path) -> impl FnOnce(io::Error) -> StoreError + '_ {
    move |error| StoreError::Io {
        path: path.to_path_buf(),
        error,
    }
}

/// The points of a batch write, in order, for crash tests.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StoreStep {
    ObjectWritten(String),
    CommitWritten(String),
    BranchWritten(String),
    VersionWritten(String),
    HeadWritten,
}

/// What a commit file says.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommitMeta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    pub root: String,
    pub branch: String,
    /// `user`, `script`, or `agent:<session>`.
    pub author: String,
    pub unix_ms: u64,
    pub next_id: u32,
    pub description: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BranchFile {
    name: String,
    head: String,
    base: String,
    author: String,
    #[serde(default)]
    archived: bool,
}

/// A branch: a named ref with a base commit (15.12).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BranchMeta {
    /// Stable id, also the file name.
    pub id: String,
    pub name: String,
    pub head: String,
    pub base: String,
    pub author: String,
    pub archived: bool,
}

/// A named version (15.11): `refs/<slug>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VersionMeta {
    pub slug: String,
    pub name: String,
    pub commit: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct HeadFile {
    branch: String,
    commit: String,
}

/// The root object: project scalars plus the hashes of the nodes.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RootObject {
    tempo_bpm: f64,
    time_sig_num: u8,
    metronome: Metronome,
    loop_region: LoopRegion,
    channels: Vec<String>,
    patterns: Vec<String>,
    tracks: Vec<String>,
    #[serde(default)]
    samples: Vec<SampleRef>,
    #[serde(default)]
    clips: Vec<Clip>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    groups: Vec<protocol::model::PatternGroup>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    shapes: Vec<protocol::model::Shape>,
}

type NodeCache<T> = HashMap<usize, (Arc<T>, String)>;
type Hook = Box<dyn FnMut(&StoreStep) -> bool + Send>;

/// Summary of `compact`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CompactReport {
    pub commits_removed: usize,
    pub objects_removed: usize,
}

pub struct HistoryStore {
    dir: PathBuf,
    commits: BTreeMap<String, CommitMeta>,
    branches: BTreeMap<String, BranchMeta>,
    versions: BTreeMap<String, VersionMeta>,
    head: Option<(String, String)>,
    objects: HashSet<String>,
    keep: Keep,
    /// Directories that got a new file and still need an fsync.
    unsynced: HashSet<PathBuf>,
    hook: Option<Hook>,
    // Write side: node pointer -> hash. The `Arc` is kept so the address
    // cannot be reused by another node while the entry exists.
    w_channels: HashMap<usize, (Arc<Channel>, String)>,
    w_patterns: HashMap<usize, (Arc<Pattern>, String)>,
    w_tracks: HashMap<usize, (Arc<Track>, String)>,
    // Read side: hash -> node, so loaded commits share what they share.
    r_channels: HashMap<String, Arc<Channel>>,
    r_patterns: HashMap<String, Arc<Pattern>>,
    r_tracks: HashMap<String, Arc<Track>>,
}

fn read_text(path: &Path) -> Result<String, StoreError> {
    let len = fs::metadata(path).map_err(io_err(path))?.len();
    if len > MAX_FILE_BYTES {
        return Err(StoreError::Corrupt(format!(
            "{} is too large",
            path.display()
        )));
    }
    fs::read_to_string(path).map_err(io_err(path))
}

fn is_hash(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Lowercase letters, digits, and single dashes; at most 40 characters.
pub fn slugify(name: &str) -> String {
    let mut s = String::new();
    for c in name.chars() {
        if c.is_alphanumeric() && c.is_ascii() {
            s.push(c.to_ascii_lowercase());
        } else if !s.ends_with('-') && !s.is_empty() {
            s.push('-');
        }
    }
    let s: String = s.trim_end_matches('-').chars().take(40).collect();
    let s = s.trim_end_matches('-').to_string();
    if s.is_empty() { "version".into() } else { s }
}

fn clean_line(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

impl HistoryStore {
    /// An empty store that has written nothing. `open` loads what is there.
    pub fn new(bundle_dir: &Path) -> HistoryStore {
        HistoryStore {
            dir: bundle_dir.join(HISTORY_DIR),
            commits: BTreeMap::new(),
            branches: BTreeMap::new(),
            versions: BTreeMap::new(),
            head: None,
            objects: HashSet::new(),
            keep: Keep::default(),
            unsynced: HashSet::new(),
            hook: None,
            w_channels: HashMap::new(),
            w_patterns: HashMap::new(),
            w_tracks: HashMap::new(),
            r_channels: HashMap::new(),
            r_patterns: HashMap::new(),
            r_tracks: HashMap::new(),
        }
    }

    /// Opens the store of a bundle. A bundle without `history/` gives an
    /// empty store. Files that fail their checks (name is not the hash of
    /// the text, root object missing, unreadable) are skipped, never
    /// trusted: they can only be left over from a crash or damage.
    pub fn open(bundle_dir: &Path) -> Result<HistoryStore, StoreError> {
        let mut s = HistoryStore::new(bundle_dir);
        let objects = s.dir.join(OBJECTS);
        if let Ok(rd) = fs::read_dir(&objects) {
            for e in rd.flatten() {
                if let Some(n) = e.file_name().to_str().and_then(|n| n.strip_suffix(".toml"))
                    && is_hash(n)
                {
                    s.objects.insert(n.to_string());
                }
            }
        }
        let commits = s.dir.join(COMMITS);
        if let Ok(rd) = fs::read_dir(&commits) {
            for e in rd.flatten() {
                let Some(name) = e
                    .file_name()
                    .to_str()
                    .and_then(|n| n.strip_suffix(".toml"))
                    .map(str::to_string)
                else {
                    continue;
                };
                if !is_hash(&name) {
                    continue;
                }
                let Ok(text) = read_text(&e.path()) else {
                    continue;
                };
                if sha256_hex(text.as_bytes()) != name {
                    continue;
                }
                let Ok(meta) = toml::from_str::<CommitMeta>(&text) else {
                    continue;
                };
                if s.objects.contains(&meta.root) {
                    s.commits.insert(name, meta);
                }
            }
        }
        if let Ok(rd) = fs::read_dir(s.dir.join(BRANCHES)) {
            for e in rd.flatten() {
                let Some(id) = e
                    .file_name()
                    .to_str()
                    .and_then(|n| n.strip_suffix(".toml"))
                    .map(str::to_string)
                else {
                    continue;
                };
                let Ok(text) = read_text(&e.path()) else {
                    continue;
                };
                let Ok(f) = toml::from_str::<BranchFile>(&text) else {
                    continue;
                };
                if s.commits.contains_key(&f.head) {
                    let base = if s.commits.contains_key(&f.base) {
                        f.base
                    } else {
                        f.head.clone()
                    };
                    s.branches.insert(
                        id.clone(),
                        BranchMeta {
                            id,
                            name: f.name,
                            head: f.head,
                            base,
                            author: f.author,
                            archived: f.archived,
                        },
                    );
                }
            }
        }
        if let Ok(rd) = fs::read_dir(s.dir.join(REFS)) {
            for e in rd.flatten() {
                let Some(slug) = e.file_name().to_str().map(str::to_string) else {
                    continue;
                };
                let Ok(text) = read_text(&e.path()) else {
                    continue;
                };
                let mut lines = text.lines();
                let (Some(commit), name) =
                    (lines.next(), lines.next().unwrap_or(&slug).to_string())
                else {
                    continue;
                };
                if s.commits.contains_key(commit) {
                    s.versions.insert(
                        slug.clone(),
                        VersionMeta {
                            slug,
                            name,
                            commit: commit.to_string(),
                        },
                    );
                }
            }
        }
        if let Ok(text) = read_text(&s.dir.join(HEAD_FILE))
            && let Ok(h) = toml::from_str::<HeadFile>(&text)
            && s.commits.contains_key(&h.commit)
        {
            s.head = Some((h.branch, h.commit));
        }
        s.scan_keep();
        Ok(s)
    }

    pub fn set_hook(&mut self, hook: Option<Hook>) {
        self.hook = hook;
    }

    fn step(&mut self, s: StoreStep) -> Result<(), StoreError> {
        if let Some(h) = &mut self.hook
            && !h(&s)
        {
            return Err(StoreError::Crashed(s));
        }
        Ok(())
    }

    pub fn is_empty(&self) -> bool {
        self.commits.is_empty()
    }

    pub fn commit_count(&self) -> usize {
        self.commits.len()
    }

    pub fn commit(&self, hash: &str) -> Option<&CommitMeta> {
        self.commits.get(hash)
    }

    pub fn commits(&self) -> impl Iterator<Item = (&String, &CommitMeta)> {
        self.commits.iter()
    }

    pub fn branches(&self) -> impl Iterator<Item = &BranchMeta> {
        self.branches.values()
    }

    pub fn versions(&self) -> impl Iterator<Item = &VersionMeta> {
        self.versions.values()
    }

    /// Current branch and commit, if HEAD was written.
    pub fn head(&self) -> Option<(&str, &str)> {
        self.head.as_ref().map(|(b, c)| (b.as_str(), c.as_str()))
    }

    /// Plugin blobs and samples that any commit references. Merge it into
    /// the `Keep` passed to `bundle::save_keeping` so the save's garbage
    /// collection never drops what an old version needs (15.11).
    pub fn keep(&self) -> &Keep {
        &self.keep
    }

    // ----- writing ---------------------------------------------------------

    fn put(
        &mut self,
        sub: &str,
        name: &str,
        bytes: &[u8],
        replace: bool,
    ) -> Result<bool, StoreError> {
        let dir = self.dir.join(sub);
        fs::create_dir_all(&dir).map_err(io_err(&dir))?;
        let fin = dir.join(name);
        if !replace && fin.exists() {
            return Ok(false);
        }
        let tmp = dir.join(format!("{name}.tmp"));
        let mut f = File::create(&tmp).map_err(io_err(&tmp))?;
        f.write_all(bytes).map_err(io_err(&tmp))?;
        f.sync_all().map_err(io_err(&tmp))?;
        drop(f);
        fs::rename(&tmp, &fin).map_err(io_err(&fin))?;
        self.unsynced.insert(dir);
        Ok(true)
    }

    /// fsyncs every directory that got a file since the last call.
    pub fn sync(&mut self) -> Result<(), StoreError> {
        let dirs: Vec<PathBuf> = self.unsynced.drain().collect();
        for d in dirs {
            File::open(&d)
                .and_then(|f| f.sync_all())
                .map_err(io_err(&d))?;
        }
        if self.dir.is_dir() {
            File::open(&self.dir)
                .and_then(|f| f.sync_all())
                .map_err(io_err(&self.dir))?;
        }
        Ok(())
    }

    fn put_object(&mut self, text: String) -> Result<String, StoreError> {
        let hash = sha256_hex(text.as_bytes());
        if !self.objects.contains(&hash) {
            self.put(OBJECTS, &format!("{hash}.toml"), text.as_bytes(), false)?;
            self.objects.insert(hash.clone());
            self.step(StoreStep::ObjectWritten(hash.clone()))?;
        }
        Ok(hash)
    }

    fn write_nodes<T: Serialize>(
        &mut self,
        items: &[Arc<T>],
        pick: fn(&mut HistoryStore) -> &mut NodeCache<T>,
    ) -> Result<Vec<String>, StoreError> {
        let mut out = Vec::with_capacity(items.len());
        for it in items {
            let key = Arc::as_ptr(it) as usize;
            if let Some((_, h)) = pick(self).get(&key) {
                out.push(h.clone());
                continue;
            }
            let text = toml::to_string(&**it)
                .map_err(|e| StoreError::Corrupt(format!("cannot write node: {e}")))?;
            let h = self.put_object(text)?;
            let cache = pick(self);
            if cache.len() > CACHE_LIMIT {
                cache.clear();
            }
            cache.insert(key, (it.clone(), h.clone()));
            out.push(h);
        }
        Ok(out)
    }

    /// Writes the nodes of a project as objects and returns the root hash.
    pub fn write_project(&mut self, p: &Project) -> Result<String, StoreError> {
        let channels = self.write_nodes(&p.channels, |s| &mut s.w_channels)?;
        let patterns = self.write_nodes(&p.patterns, |s| &mut s.w_patterns)?;
        let tracks = self.write_nodes(&p.tracks, |s| &mut s.w_tracks)?;
        let root = RootObject {
            tempo_bpm: p.tempo_bpm,
            time_sig_num: p.time_sig_num,
            metronome: p.metronome.clone(),
            loop_region: p.loop_region,
            channels,
            patterns,
            tracks,
            samples: p.samples.clone(),
            clips: p.clips.clone(),
            groups: p.groups.clone(),
            shapes: p.shapes.clone(),
        };
        let text = toml::to_string(&root)
            .map_err(|e| StoreError::Corrupt(format!("cannot write root: {e}")))?;
        self.put_object(text)
    }

    /// Writes one commit (and the objects it needs) and returns its hash.
    /// Writing the same commit again returns the same hash.
    #[allow(clippy::too_many_arguments)]
    pub fn write_commit(
        &mut self,
        project: &Project,
        next_id: u32,
        parent: Option<&str>,
        branch: &str,
        author: &str,
        description: &str,
        unix_ms: u64,
    ) -> Result<String, StoreError> {
        let root = self.write_project(project)?;
        self.sync()?;
        let meta = CommitMeta {
            parent: parent.map(str::to_string),
            root,
            branch: branch.to_string(),
            author: author.to_string(),
            unix_ms,
            next_id,
            description: clean_line(description),
        };
        let text = toml::to_string(&meta)
            .map_err(|e| StoreError::Corrupt(format!("cannot write commit: {e}")))?;
        let hash = sha256_hex(text.as_bytes());
        if self.put(COMMITS, &format!("{hash}.toml"), text.as_bytes(), false)? {
            self.step(StoreStep::CommitWritten(hash.clone()))?;
        }
        self.sync()?;
        self.keep.add_project(project);
        self.commits.insert(hash.clone(), meta);
        Ok(hash)
    }

    /// Creates or updates a branch ref. Its head and base must be commits
    /// of this store.
    pub fn write_branch(&mut self, b: &BranchMeta) -> Result<(), StoreError> {
        for c in [&b.head, &b.base] {
            if !self.commits.contains_key(c) {
                return Err(StoreError::Corrupt(format!(
                    "branch names unknown commit {c}"
                )));
            }
        }
        if b.id.is_empty() || b.id != slugify(&b.id) {
            return Err(StoreError::Corrupt(format!("bad branch id {}", b.id)));
        }
        let text = toml::to_string(&BranchFile {
            name: clean_line(&b.name),
            head: b.head.clone(),
            base: b.base.clone(),
            author: b.author.clone(),
            archived: b.archived,
        })
        .map_err(|e| StoreError::Corrupt(format!("cannot write branch: {e}")))?;
        self.put(BRANCHES, &format!("{}.toml", b.id), text.as_bytes(), true)?;
        self.sync()?;
        self.branches.insert(b.id.clone(), b.clone());
        self.step(StoreStep::BranchWritten(b.id.clone()))
    }

    /// Names a commit (`refs/<slug>`). Naming an existing slug moves it.
    pub fn write_version(&mut self, name: &str, commit: &str) -> Result<String, StoreError> {
        if !self.commits.contains_key(commit) {
            return Err(StoreError::Corrupt(format!("unknown commit {commit}")));
        }
        let slug = slugify(name);
        let name = clean_line(name);
        let text = format!("{commit}\n{name}\n");
        self.put(REFS, &slug, text.as_bytes(), true)?;
        self.sync()?;
        self.versions.insert(
            slug.clone(),
            VersionMeta {
                slug: slug.clone(),
                name,
                commit: commit.to_string(),
            },
        );
        self.step(StoreStep::VersionWritten(slug.clone()))?;
        Ok(slug)
    }

    pub fn write_head(&mut self, branch: &str, commit: &str) -> Result<(), StoreError> {
        if !self.commits.contains_key(commit) {
            return Err(StoreError::Corrupt(format!("unknown commit {commit}")));
        }
        let text = toml::to_string(&HeadFile {
            branch: branch.to_string(),
            commit: commit.to_string(),
        })
        .map_err(|e| StoreError::Corrupt(format!("cannot write HEAD: {e}")))?;
        self.put("", HEAD_FILE, text.as_bytes(), true)?;
        self.sync()?;
        self.head = Some((branch.to_string(), commit.to_string()));
        self.step(StoreStep::HeadWritten)
    }

    // ----- reading ---------------------------------------------------------

    fn object_text(&self, hash: &str) -> Result<String, StoreError> {
        if !is_hash(hash) {
            return Err(StoreError::Corrupt(format!("bad object name {hash}")));
        }
        let text = read_text(&self.dir.join(OBJECTS).join(format!("{hash}.toml")))?;
        if sha256_hex(text.as_bytes()) != hash {
            return Err(StoreError::Corrupt(format!(
                "object {hash} does not match its name"
            )));
        }
        Ok(text)
    }

    fn read_nodes<T: for<'de> Deserialize<'de>>(
        &mut self,
        hashes: &[String],
        pick: fn(&mut HistoryStore) -> &mut HashMap<String, Arc<T>>,
    ) -> Result<Vec<Arc<T>>, StoreError> {
        let mut out = Vec::with_capacity(hashes.len());
        for h in hashes {
            if let Some(n) = pick(self).get(h) {
                out.push(n.clone());
                continue;
            }
            let text = self.object_text(h)?;
            let node: T = toml::from_str(&text)
                .map_err(|e| StoreError::Corrupt(format!("object {h}: {e}")))?;
            let node = Arc::new(node);
            pick(self).insert(h.clone(), node.clone());
            out.push(node);
        }
        Ok(out)
    }

    /// The project and `next_id` of a commit. Plugin state bytes are not
    /// loaded (`state_bytes` is `None`); the editor fills them from
    /// `plugin-state/` when it moves there.
    pub fn load_project(&mut self, commit: &str) -> Result<(Project, u32), StoreError> {
        let meta = self
            .commits
            .get(commit)
            .cloned()
            .ok_or_else(|| StoreError::Corrupt(format!("unknown commit {commit}")))?;
        let text = self.object_text(&meta.root)?;
        let root: RootObject = toml::from_str(&text)
            .map_err(|e| StoreError::Corrupt(format!("root {}: {e}", meta.root)))?;
        let channels = self.read_nodes(&root.channels, |s| &mut s.r_channels)?;
        let patterns = self.read_nodes(&root.patterns, |s| &mut s.r_patterns)?;
        let tracks = self.read_nodes(&root.tracks, |s| &mut s.r_tracks)?;
        let p = Project {
            tempo_bpm: root.tempo_bpm,
            time_sig_num: root.time_sig_num,
            metronome: root.metronome,
            channels,
            patterns,
            tracks,
            samples: root.samples,
            clips: root.clips,
            loop_region: root.loop_region,
            groups: root.groups,
            shapes: root.shapes,
        };
        protocol::validate::validate(&p)
            .map_err(|e| StoreError::Corrupt(format!("commit {commit}: {e}")))?;
        Ok((p, meta.next_id))
    }

    fn scan_keep(&mut self) {
        self.keep = Keep::default();
        let mut seen: HashSet<String> = HashSet::new();
        let roots: Vec<String> = self.commits.values().map(|c| c.root.clone()).collect();
        for r in roots {
            if !seen.insert(r.clone()) {
                continue;
            }
            let Ok(text) = self.object_text(&r) else {
                continue;
            };
            let Ok(root) = toml::from_str::<RootObject>(&text) else {
                continue;
            };
            self.keep
                .samples
                .extend(root.samples.iter().map(|s| s.hash.clone()));
            for h in root.channels.iter().chain(&root.tracks) {
                if !seen.insert(h.clone()) {
                    continue;
                }
                let Ok(t) = self.object_text(h) else {
                    continue;
                };
                for line in t.lines() {
                    if let Some(n) = line
                        .trim()
                        .strip_prefix("state_file = \"")
                        .and_then(|r| r.strip_suffix('"'))
                    {
                        self.keep.blobs.insert(n.to_string());
                    }
                }
            }
        }
    }

    // ----- the tree for the control API ---------------------------------------

    fn version_name(&self, commit: &str) -> Option<String> {
        self.versions
            .values()
            .find(|v| v.commit == commit)
            .map(|v| v.name.clone())
    }

    /// Commits newest first. `since` lists only commits after that one
    /// (those with a later time, ties broken by hash), `limit` caps the
    /// count (0 = all).
    pub fn nodes(&self, since: Option<&str>, limit: usize) -> Vec<HistoryNode> {
        let floor = since
            .and_then(|s| self.commits.get(s))
            .map(|c| (c.unix_ms, since.unwrap_or("").to_string()));
        let mut all: Vec<(&String, &CommitMeta)> = self
            .commits
            .iter()
            .filter(|(h, c)| {
                floor
                    .as_ref()
                    .is_none_or(|(ms, fh)| (c.unix_ms, h.as_str()) > (*ms, fh.as_str()))
            })
            .collect();
        all.sort_by(|a, b| (b.1.unix_ms, b.0).cmp(&(a.1.unix_ms, a.0)));
        if limit > 0 {
            all.truncate(limit);
        }
        all.into_iter()
            .map(|(h, c)| HistoryNode {
                commit: h.clone(),
                parent: c.parent.clone().filter(|p| self.commits.contains_key(p)),
                branch: c.branch.clone(),
                author: c.author.clone(),
                description: c.description.clone(),
                unix_ms: c.unix_ms,
                name: self.version_name(h),
            })
            .collect()
    }

    pub fn branch_infos(&self) -> Vec<BranchInfo> {
        self.branches
            .values()
            .map(|b| BranchInfo {
                branch: b.id.clone(),
                name: b.name.clone(),
                head: b.head.clone(),
                base: b.base.clone(),
                author: b.author.clone(),
                archived: b.archived,
            })
            .collect()
    }

    // ----- compaction -------------------------------------------------------

    /// "Compact history" (15.11): drops commits older than `keep_since_ms`
    /// unless they are named versions, branch heads or bases, or HEAD, then
    /// removes objects nothing references. Commits are removed before
    /// objects, so a crash never leaves a commit without its objects. A
    /// kept commit whose parent was dropped becomes a root.
    pub fn compact(&mut self, keep_since_ms: u64) -> Result<CompactReport, StoreError> {
        let mut keep: HashSet<String> = HashSet::new();
        keep.extend(self.versions.values().map(|v| v.commit.clone()));
        for b in self.branches.values() {
            keep.insert(b.head.clone());
            keep.insert(b.base.clone());
        }
        if let Some((_, c)) = &self.head {
            keep.insert(c.clone());
        }
        keep.extend(
            self.commits
                .iter()
                .filter(|(_, c)| c.unix_ms >= keep_since_ms)
                .map(|(h, _)| h.clone()),
        );
        let drop: Vec<String> = self
            .commits
            .keys()
            .filter(|h| !keep.contains(*h))
            .cloned()
            .collect();
        let mut report = CompactReport::default();
        for h in &drop {
            let p = self.dir.join(COMMITS).join(format!("{h}.toml"));
            fs::remove_file(&p).map_err(io_err(&p))?;
            self.commits.remove(h);
            report.commits_removed += 1;
        }
        // Objects: everything reachable from the commits that remain.
        let mut live: HashSet<String> = HashSet::new();
        let roots: Vec<String> = self.commits.values().map(|c| c.root.clone()).collect();
        for r in roots {
            if !live.insert(r.clone()) {
                continue;
            }
            if let Ok(text) = self.object_text(&r)
                && let Ok(root) = toml::from_str::<RootObject>(&text)
            {
                live.extend(root.channels);
                live.extend(root.patterns);
                live.extend(root.tracks);
            }
        }
        let objects = self.dir.join(OBJECTS);
        if let Ok(rd) = fs::read_dir(&objects) {
            for e in rd.flatten() {
                let Some(n) = e.file_name().to_str().map(str::to_string) else {
                    continue;
                };
                let stem = n.strip_suffix(".toml");
                let stale = match stem {
                    Some(h) => is_hash(h) && !live.contains(h),
                    None => n.ends_with(".tmp"),
                };
                if stale {
                    let p = e.path();
                    fs::remove_file(&p).map_err(io_err(&p))?;
                    if let Some(h) = stem {
                        self.objects.remove(h);
                    }
                    report.objects_removed += 1;
                }
            }
        }
        self.w_channels.clear();
        self.w_patterns.clear();
        self.w_tracks.clear();
        self.r_channels.retain(|h, _| live.contains(h));
        self.r_patterns.retain(|h, _| live.contains(h));
        self.r_tracks.retain(|h, _| live.contains(h));
        self.scan_keep();
        Ok(report)
    }
}

/// Everything the writer needs for one commit, with no document types
/// behind it: cheap to build on the GTK thread, written on the worker.
#[derive(Clone, Debug)]
pub struct PendingCommit {
    /// The editor's id of the entry (`History` entry id).
    pub entry: u64,
    pub project: Arc<Project>,
    pub next_id: u32,
    pub parent: Option<u64>,
    pub branch: String,
    pub author: String,
    pub description: String,
    pub unix_ms: u64,
}

#[derive(Clone, Debug)]
pub struct PendingBranch {
    pub id: String,
    pub name: String,
    pub head: u64,
    pub base: u64,
    pub author: String,
    pub archived: bool,
}

/// One batch for the worker: new commits plus the refs after them.
#[derive(Clone, Debug, Default)]
pub struct PendingBatch {
    pub commits: Vec<PendingCommit>,
    pub branches: Vec<PendingBranch>,
    /// `(display name, entry)`
    pub versions: Vec<(String, u64)>,
    /// `(branch id, entry)`
    pub head: Option<(String, u64)>,
}

impl PendingBatch {
    pub fn is_empty(&self) -> bool {
        self.commits.is_empty()
            && self.branches.is_empty()
            && self.versions.is_empty()
            && self.head.is_none()
    }
}

/// The save worker's side: a store plus the map from editor entries to the
/// commit hashes written for them.
pub struct Persister {
    pub store: HistoryStore,
    hashes: HashMap<u64, String>,
}

impl Persister {
    pub fn new(store: HistoryStore, known: HashMap<u64, String>) -> Persister {
        Persister {
            store,
            hashes: known,
        }
    }

    pub fn hash_of(&self, entry: u64) -> Option<&str> {
        self.hashes.get(&entry).map(String::as_str)
    }

    /// Writes a batch in dependency order and returns `(entry, hash)` for
    /// every commit written. On error nothing is half-referenced: refs are
    /// only written after the commits they name. Entries whose commit is
    /// not known yet are skipped in refs; the next batch fixes them.
    pub fn write(&mut self, batch: &PendingBatch) -> Result<Vec<(u64, String)>, StoreError> {
        let mut done = Vec::new();
        for c in &batch.commits {
            let parent = c.parent.and_then(|p| self.hashes.get(&p)).cloned();
            let h = self.store.write_commit(
                &c.project,
                c.next_id,
                parent.as_deref(),
                &c.branch,
                &c.author,
                &c.description,
                c.unix_ms,
            )?;
            self.hashes.insert(c.entry, h.clone());
            done.push((c.entry, h));
        }
        for b in &batch.branches {
            let (Some(head), Some(base)) = (self.hashes.get(&b.head), self.hashes.get(&b.base))
            else {
                continue;
            };
            self.store.write_branch(&BranchMeta {
                id: b.id.clone(),
                name: b.name.clone(),
                head: head.clone(),
                base: base.clone(),
                author: b.author.clone(),
                archived: b.archived,
            })?;
        }
        for (name, entry) in &batch.versions {
            if let Some(h) = self.hashes.get(entry).cloned() {
                self.store.write_version(name, &h)?;
            }
        }
        if let Some((branch, entry)) = &batch.head
            && let Some(h) = self.hashes.get(entry).cloned()
        {
            self.store.write_head(branch, &h)?;
        }
        Ok(done)
    }
}

#[cfg(test)]
pub(crate) mod tests;
