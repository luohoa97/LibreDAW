// SPDX-License-Identifier: GPL-3.0-or-later
//! Project bundles on disk (SPEC 7.1, 7.4, 7.5, 7.6, 17.1).
//!
//! ```text
//! Name.ldaw/
//!   project.toml
//!   plugin-state/<instance>-<generation>.bin   immutable blobs
//!   .autosave/                                 same layout, written by autosave
//! ```
//!
//! `save` follows 7.4 exactly: new blobs first (tmp, fsync, rename), fsync
//! the blob directory, then `project.toml` (tmp, fsync, rename, fsync the
//! bundle directory), then delete blobs the new file does not reference. A
//! crash at any point leaves the complete old project or the complete new
//! one, plus possibly orphan blobs that the next save removes.
//!
//! These functions block on disk I/O. Callers run them on a worker thread
//! (`AutosaveWorker`, the save job), never on the GTK thread.

use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use protocol::format::{self, FormatError};
use protocol::model::{Insert, Instrument, Project};
use protocol::validate::ValidationError;

use crate::document::{Document, parse_state_file_name};

pub const PROJECT_FILE: &str = "project.toml";
pub const STATE_DIR: &str = "plugin-state";
pub const AUTOSAVE_DIR: &str = ".autosave";
/// Autosave timing (7.6, Amendment 10) and plugin state capture period.
pub const AUTOSAVE_QUIET: Duration = Duration::from_secs(3);
pub const AUTOSAVE_MAX: Duration = Duration::from_secs(15);
pub const CAPTURE_PERIOD: Duration = Duration::from_secs(60);

/// Largest `project.toml` and largest blob the loader reads.
const MAX_PROJECT_FILE_BYTES: u64 = 256 << 20;
const MAX_BLOB_BYTES: u64 = 256 << 20;

#[derive(Debug)]
pub enum BundleError {
    Io {
        path: PathBuf,
        error: io::Error,
    },
    Format(FormatError),
    Invalid(ValidationError),
    /// A blob with this name exists with different content. Blobs are
    /// immutable (7.1), so this means two captures got the same name.
    BlobConflict(String),
    /// Test hook: the save stopped here, as a crash would.
    Crashed(SaveStep),
    TooLarge(PathBuf),
}

impl std::fmt::Display for BundleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BundleError::Io { path, error } => write!(f, "{}: {error}", path.display()),
            BundleError::Format(e) => write!(f, "{e}"),
            BundleError::Invalid(e) => write!(f, "invalid project: {e}"),
            BundleError::BlobConflict(n) => write!(f, "plugin state file {n} already exists"),
            BundleError::Crashed(s) => write!(f, "simulated crash after {s:?}"),
            BundleError::TooLarge(p) => write!(f, "{} is too large", p.display()),
        }
    }
}

impl std::error::Error for BundleError {}

fn io_err(path: &Path) -> impl FnOnce(io::Error) -> BundleError + '_ {
    move |error| BundleError::Io {
        path: path.to_path_buf(),
        error,
    }
}

/// The points of the save procedure, in order. The hook passed to
/// `save_with_hook` sees each one after it happened and may stop the save
/// there to simulate a crash.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SaveStep {
    /// Step 1: `<name>.tmp` written and fsynced.
    BlobTmpWritten(String),
    /// Step 1: renamed to its final name.
    BlobRenamed(String),
    /// Step 2.
    StateDirSynced,
    /// Step 3: `project.toml.tmp` written and fsynced.
    ProjectTmpWritten,
    /// Step 3: renamed over `project.toml`.
    ProjectRenamed,
    /// Step 3: bundle directory fsynced.
    BundleDirSynced,
    /// Step 4: one unreferenced blob (or leftover tmp file) deleted.
    BlobDeleted(String),
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct SaveReport {
    pub blobs_written: Vec<String>,
    pub blobs_deleted: Vec<String>,
}

fn fsync_dir(dir: &Path) -> Result<(), BundleError> {
    File::open(dir)
        .and_then(|f| f.sync_all())
        .map_err(io_err(dir))
}

/// Writes `bytes` to `tmp`, fsyncs it, and leaves it there.
fn write_durable(tmp: &Path, bytes: &[u8]) -> Result<(), BundleError> {
    let mut f = File::create(tmp).map_err(io_err(tmp))?;
    f.write_all(bytes).map_err(io_err(tmp))?;
    f.sync_all().map_err(io_err(tmp))
}

fn is_blob_name(n: &str) -> bool {
    parse_state_file_name(n).is_some()
}

fn is_ours_to_delete(n: &str) -> bool {
    is_blob_name(n) || n.ends_with(".tmp")
}

/// Calls `f` on every CLAP reference in the project, cloning only the
/// channels and tracks that hold one.
fn for_each_clap_mut(p: &mut Project, mut f: impl FnMut(&mut protocol::model::ClapRef)) {
    for c in &mut p.channels {
        if matches!(c.instrument, Instrument::Clap(_))
            && let Instrument::Clap(r) = &mut Arc::make_mut(c).instrument
        {
            f(r);
        }
    }
    for t in &mut p.tracks {
        if !t.inserts.is_empty() {
            for Insert::Clap(r) in &mut Arc::make_mut(t).inserts {
                f(r);
            }
        }
    }
}

fn for_each_clap(p: &Project, mut f: impl FnMut(&protocol::model::ClapRef)) {
    for c in &p.channels {
        if let Instrument::Clap(r) = &c.instrument {
            f(r);
        }
    }
    for t in &p.tracks {
        for Insert::Clap(r) in &t.inserts {
            f(r);
        }
    }
}

/// Saves the document into the bundle directory `dir` (7.4).
pub fn save(dir: &Path, doc: &Document) -> Result<SaveReport, BundleError> {
    save_with_hook(dir, doc, &mut |_| true)
}

/// `save` with a hook called after each step of 7.4. If the hook returns
/// false the save stops there with `Crashed`, leaving the disk as it is.
pub fn save_with_hook(
    dir: &Path,
    doc: &Document,
    hook: &mut dyn FnMut(&SaveStep) -> bool,
) -> Result<SaveReport, BundleError> {
    let mut step = |s: SaveStep| -> Result<(), BundleError> {
        if hook(&s) {
            Ok(())
        } else {
            Err(BundleError::Crashed(s))
        }
    };
    let state_dir = dir.join(STATE_DIR);
    fs::create_dir_all(&state_dir).map_err(io_err(&state_dir))?;

    // Decide what the new project.toml references. A ref whose blob is not
    // in memory and not on disk cannot be written; it is saved without a
    // state file (the plugin starts from defaults) rather than pointing at
    // nothing.
    let mut project = (*doc.project).clone();
    let mut to_write: Vec<(String, Arc<[u8]>)> = Vec::new();
    let mut err: Option<BundleError> = None;
    for_each_clap_mut(&mut project, |r| {
        let Some(name) = r.state_file.clone() else {
            return;
        };
        let path = state_dir.join(&name);
        match &r.state_bytes {
            Some(bytes) => match fs::metadata(&path) {
                Ok(m) if m.len() == bytes.len() as u64 => {}
                Ok(_) => err = Some(BundleError::BlobConflict(name)),
                Err(_) => {
                    if !to_write.iter().any(|(n, _)| *n == name) {
                        to_write.push((name, bytes.clone()));
                    }
                }
            },
            None => {
                if !path.is_file() {
                    r.state_file = None;
                }
            }
        }
    });
    if let Some(e) = err {
        return Err(e);
    }
    let text = format::emit(&project, doc.next_id).map_err(BundleError::Invalid)?;

    let mut report = SaveReport::default();

    // 1. New blobs: tmp, fsync, rename.
    for (name, bytes) in &to_write {
        let tmp = state_dir.join(format!("{name}.tmp"));
        let fin = state_dir.join(name);
        write_durable(&tmp, bytes)?;
        step(SaveStep::BlobTmpWritten(name.clone()))?;
        fs::rename(&tmp, &fin).map_err(io_err(&fin))?;
        step(SaveStep::BlobRenamed(name.clone()))?;
        report.blobs_written.push(name.clone());
    }
    // 2. fsync the blob directory.
    fsync_dir(&state_dir)?;
    step(SaveStep::StateDirSynced)?;

    // 3. project.toml.
    let tmp = dir.join(format!("{PROJECT_FILE}.tmp"));
    let fin = dir.join(PROJECT_FILE);
    write_durable(&tmp, text.as_bytes())?;
    step(SaveStep::ProjectTmpWritten)?;
    fs::rename(&tmp, &fin).map_err(io_err(&fin))?;
    step(SaveStep::ProjectRenamed)?;
    fsync_dir(dir)?;
    step(SaveStep::BundleDirSynced)?;

    // 4. Delete blobs the new project does not reference.
    let mut referenced: HashSet<String> = HashSet::new();
    for_each_clap(&project, |r| {
        if let Some(n) = &r.state_file {
            referenced.insert(n.clone());
        }
    });
    let mut stale: Vec<String> = Vec::new();
    for entry in fs::read_dir(&state_dir).map_err(io_err(&state_dir))? {
        let entry = entry.map_err(io_err(&state_dir))?;
        let Some(n) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if is_ours_to_delete(&n) && !referenced.contains(&n) {
            stale.push(n);
        }
    }
    stale.sort();
    for n in stale {
        let p = state_dir.join(&n);
        fs::remove_file(&p).map_err(io_err(&p))?;
        step(SaveStep::BlobDeleted(n.clone()))?;
        report.blobs_deleted.push(n);
    }
    Ok(report)
}

/// A project read from disk.
#[derive(Debug)]
pub struct Loaded {
    pub doc: Document,
    /// State files named by `project.toml` that are not on disk. Those
    /// plugins load without state (7.5).
    pub missing_blobs: Vec<String>,
}

fn read_limited(path: &Path, max: u64) -> Result<Vec<u8>, BundleError> {
    let f = File::open(path).map_err(io_err(path))?;
    let len = f.metadata().map_err(io_err(path))?.len();
    if len > max {
        return Err(BundleError::TooLarge(path.to_path_buf()));
    }
    let mut v = Vec::with_capacity(len as usize);
    f.take(max + 1).read_to_end(&mut v).map_err(io_err(path))?;
    Ok(v)
}

/// Loads a bundle directory. `next_id` is `max(file, max id + 1)` (17.1;
/// `format::parse` applies the rule).
pub fn load(dir: &Path) -> Result<Loaded, BundleError> {
    let file = dir.join(PROJECT_FILE);
    let bytes = read_limited(&file, MAX_PROJECT_FILE_BYTES)?;
    let text = String::from_utf8(bytes)
        .map_err(|e| BundleError::Format(FormatError::Syntax(format!("not UTF-8: {e}"))))?;
    let (mut project, next_id) = format::parse(&text).map_err(BundleError::Format)?;
    let state_dir = dir.join(STATE_DIR);
    let mut missing = Vec::new();
    let mut failure: Option<BundleError> = None;
    for_each_clap_mut(&mut project, |r| {
        let Some(name) = &r.state_file else {
            return;
        };
        match read_limited(&state_dir.join(name), MAX_BLOB_BYTES) {
            Ok(b) => r.state_bytes = Some(Arc::from(b)),
            Err(BundleError::Io { error, .. }) if error.kind() == io::ErrorKind::NotFound => {
                missing.push(name.clone());
            }
            Err(e) => failure = Some(e),
        }
    });
    if let Some(e) = failure {
        return Err(e);
    }
    Ok(Loaded {
        doc: Document::from_project(project, next_id),
        missing_blobs: missing,
    })
}

// ---------------------------------------------------------------------------
// Autosave (7.6)

pub fn autosave_path(bundle: &Path) -> PathBuf {
    bundle.join(AUTOSAVE_DIR)
}

/// Writes an autosave copy, with the same procedure as `save`.
pub fn save_autosave(bundle: &Path, doc: &Document) -> Result<SaveReport, BundleError> {
    save(&autosave_path(bundle), doc)
}

/// True if the bundle has an autosave newer than its `project.toml` (or a
/// bundle without a `project.toml` at all): the previous session did not
/// end with a save.
pub fn autosave_is_newer(bundle: &Path) -> bool {
    let auto = autosave_path(bundle).join(PROJECT_FILE);
    let Ok(a) = fs::metadata(&auto).and_then(|m| m.modified()) else {
        return false;
    };
    match fs::metadata(bundle.join(PROJECT_FILE)).and_then(|m| m.modified()) {
        Ok(p) => a > p,
        Err(_) => true,
    }
}

pub fn load_autosave(bundle: &Path) -> Result<Loaded, BundleError> {
    load(&autosave_path(bundle))
}

/// Removes the autosave copy (after a successful save or when the user
/// declines recovery).
pub fn clear_autosave(bundle: &Path) -> Result<(), BundleError> {
    let p = autosave_path(bundle);
    match fs::remove_dir_all(&p) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(BundleError::Io { path: p, error }),
    }
}

/// Explicit save: `save`, then drop the now stale autosave.
pub fn save_and_clear_autosave(dir: &Path, doc: &Document) -> Result<SaveReport, BundleError> {
    let r = save(dir, doc)?;
    clear_autosave(dir)?;
    Ok(r)
}

/// Decides when an autosave is due (7.6, Amendment 10): `QUIET` after the
/// last edit, and at least every `MAX` while edits keep coming. The caller
/// passes the time in, so tests need no real clock.
#[derive(Debug)]
pub struct AutosaveDebounce {
    quiet: Duration,
    max: Duration,
    /// When the oldest unsaved change happened.
    first: Option<Instant>,
    last: Option<Instant>,
}

impl AutosaveDebounce {
    pub fn new(quiet: Duration, max: Duration) -> AutosaveDebounce {
        AutosaveDebounce {
            quiet,
            max,
            first: None,
            last: None,
        }
    }

    /// The standard timing: 3 s after the last edit, 15 s at most.
    pub fn standard() -> AutosaveDebounce {
        AutosaveDebounce::new(AUTOSAVE_QUIET, AUTOSAVE_MAX)
    }

    /// Record that the document changed at `now`.
    pub fn changed(&mut self, now: Instant) {
        self.first.get_or_insert(now);
        self.last = Some(now);
    }

    pub fn pending(&self) -> bool {
        self.first.is_some()
    }

    /// True when a write should start now. Returns true once per batch of
    /// changes.
    pub fn due(&mut self, now: Instant) -> bool {
        let (Some(first), Some(last)) = (self.first, self.last) else {
            return false;
        };
        if now.duration_since(last) >= self.quiet || now.duration_since(first) >= self.max {
            self.first = None;
            self.last = None;
            true
        } else {
            false
        }
    }

    /// Forget pending changes (the document was saved).
    pub fn clear(&mut self) {
        self.first = None;
        self.last = None;
    }
}

/// Plugin state capture timing (7.5 (c), Amendment 10): every `PERIOD`
/// while some plugin reported a change, and right after a plugin gesture.
#[derive(Debug)]
pub struct CaptureClock {
    period: Duration,
    last: Instant,
    flagged: bool,
}

impl CaptureClock {
    pub fn new(now: Instant, period: Duration) -> CaptureClock {
        CaptureClock {
            period,
            last: now,
            flagged: false,
        }
    }

    pub fn standard(now: Instant) -> CaptureClock {
        CaptureClock::new(now, CAPTURE_PERIOD)
    }

    /// A plugin called `state.mark_dirty`.
    pub fn plugin_dirty(&mut self) {
        self.flagged = true;
    }

    /// A plugin gesture ended: capture on the next check.
    pub fn after_gesture(&mut self, now: Instant) {
        self.flagged = true;
        self.last = now.checked_sub(self.period).unwrap_or(now);
    }

    /// True when state should be captured now.
    pub fn due(&mut self, now: Instant) -> bool {
        if self.flagged && now.duration_since(self.last) >= self.period {
            self.flagged = false;
            self.last = now;
            true
        } else {
            false
        }
    }
}

enum Job {
    Write(PathBuf, Document),
    Stop,
}

/// Result of one autosave write.
#[derive(Debug)]
pub struct AutosaveResult {
    pub bundle: PathBuf,
    pub revision: u64,
    pub result: Result<SaveReport, BundleError>,
}

/// A thread that writes autosave copies. If several documents arrive while
/// one is being written, only the newest is written next.
pub struct AutosaveWorker {
    tx: Sender<Job>,
    pub results: Receiver<AutosaveResult>,
    handle: Option<JoinHandle<()>>,
}

impl AutosaveWorker {
    pub fn spawn() -> AutosaveWorker {
        let (tx, rx) = channel::<Job>();
        let (rtx, results) = channel::<AutosaveResult>();
        let handle = std::thread::Builder::new()
            .name("autosave".into())
            .spawn(move || {
                while let Ok(job) = rx.recv() {
                    let mut job = job;
                    // Coalesce: keep the newest of whatever is waiting.
                    let mut stop = false;
                    while let Ok(next) = rx.try_recv() {
                        match next {
                            Job::Stop => {
                                stop = true;
                                break;
                            }
                            w => job = w,
                        }
                    }
                    if let Job::Write(bundle, doc) = job {
                        let result = save(&bundle, &doc);
                        let _ = rtx.send(AutosaveResult {
                            bundle,
                            revision: doc.revision,
                            result,
                        });
                    } else {
                        break;
                    }
                    if stop {
                        break;
                    }
                }
            })
            .expect("spawn autosave thread");
        AutosaveWorker {
            tx,
            results,
            handle: Some(handle),
        }
    }

    /// Queues `doc` for writing into directory `bundle`: the `.autosave`
    /// folder of a project, or a recovery bundle of a never-saved one.
    pub fn submit(&self, bundle: PathBuf, doc: Document) {
        let _ = self.tx.send(Job::Write(bundle, doc));
    }

    /// Finishes pending work and joins the thread.
    pub fn shutdown(mut self) {
        let _ = self.tx.send(Job::Stop);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for AutosaveWorker {
    fn drop(&mut self) {
        let _ = self.tx.send(Job::Stop);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

#[cfg(test)]
mod tests;
