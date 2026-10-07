// SPDX-License-Identifier: GPL-3.0-or-later
//! The library index: walk a sample folder, read headers only, classify,
//! and keep a canonical TOML cache so a rescan only re-reads files whose
//! size or modification time changed.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use protocol::control::SoundInfo;
use serde::{Deserialize, Serialize};

use crate::classify::classify;
use crate::header::{Format, read_header};

/// Bump when classification changes, so old caches are rebuilt.
const CACHE_VERSION: u32 = 3;

/// One sound found in a library. Always `local_only`: it stays where the
/// user has it (SPEC 15.3, 17.2 license-1).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SoundEntry {
    /// Stable: a hash of the source, relative path, size and mtime.
    pub id: String,
    pub name: String,
    pub role: String,
    pub tags: Vec<String>,
    pub pack: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kit: Option<String>,
    /// Instrument of a multisample, and the group its zones belong to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instrument: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// Number in the file name, for ordering a numbered multisample.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<u32>,
    pub rel: String,
    pub size: u64,
    /// Modification time, unix milliseconds.
    pub mtime_ms: i64,
    pub format: Format,
    pub sample_rate: u32,
    pub channels: u16,
    pub frames: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tempo_bpm: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// MIDI root note of a tonal one-note sample, and where it came from:
    /// "name", "keyboard-order", "pitch" or "unknown".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_note: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_source: Option<String>,
    /// Always true for entries from a user library.
    pub local_only: bool,
    /// Absolute path on this machine (filled on load, not cached).
    #[serde(skip)]
    pub path: PathBuf,
}

impl SoundEntry {
    pub fn duration_s(&self) -> f64 {
        if self.sample_rate == 0 {
            0.0
        } else {
            self.frames as f64 / f64::from(self.sample_rate)
        }
    }

    /// The control-API form (protocol `SoundInfo`).
    pub fn to_sound_info(&self) -> SoundInfo {
        SoundInfo {
            id: self.id.clone(),
            name: self.name.clone(),
            role: self.role.clone(),
            genres: Vec::new(),
            tags: self.tags.clone(),
            pack: self.pack.clone(),
            kit: self.kit.clone(),
            source: String::new(),
            kind: "single sound".into(),
            kit_name: None,
        }
    }
}

/// What to scan and how to label it.
#[derive(Clone, Debug)]
pub struct Source {
    pub root: PathBuf,
    /// Shown to the user, for example "FL Studio 2026".
    pub label: String,
    /// Prefix of entry ids, for example "fl".
    pub id_prefix: String,
    /// Pack names are `<pack_prefix>: <top folder>`.
    pub pack_prefix: String,
}

impl Source {
    /// An FL Studio install's `Packs` folder.
    pub fn fl_studio(install: &crate::detect::FlInstall) -> Source {
        Source {
            root: install.packs.clone(),
            label: install.name.clone(),
            id_prefix: "fl".into(),
            pack_prefix: "FL Studio".into(),
        }
    }

    /// Any folder of samples the user owns.
    pub fn user_folder(root: &Path) -> Source {
        let name = root
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("Samples")
            .to_string();
        Source {
            root: root.to_path_buf(),
            label: name.clone(),
            id_prefix: "user".into(),
            pack_prefix: name,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct LibraryIndex {
    pub root: PathBuf,
    pub label: String,
    /// Sorted by `rel`.
    pub entries: Vec<SoundEntry>,
}

impl LibraryIndex {
    pub fn sounds(&self) -> Vec<SoundInfo> {
        self.entries.iter().map(SoundEntry::to_sound_info).collect()
    }

    pub fn find(&self, id: &str) -> Option<&SoundEntry> {
        self.entries.iter().find(|e| e.id == id)
    }

    /// Entry counts per role, in the fixed role order, zero counts left out.
    pub fn counts_by_role(&self) -> Vec<(&'static str, usize)> {
        crate::classify::ROLES
            .iter()
            .map(|r| (*r, self.entries.iter().filter(|e| e.role == *r).count()))
            .filter(|(_, n)| *n > 0)
            .collect()
    }
}

#[derive(Clone, Debug, Default)]
pub struct ScanStats {
    pub files: usize,
    /// Entries taken from the cache untouched.
    pub reused: usize,
    /// Files whose header was read this time.
    pub parsed: usize,
    pub removed: usize,
    /// Audio files whose header could not be read.
    pub unreadable: usize,
    /// Root notes estimated from the audio.
    pub pitched: usize,
    pub cache_written: bool,
    pub elapsed: Duration,
}

pub struct ScanResult {
    pub index: LibraryIndex,
    pub stats: ScanStats,
}

#[derive(Serialize, Deserialize)]
struct CacheFile {
    version: u32,
    root: String,
    label: String,
    #[serde(default)]
    sound: Vec<SoundEntry>,
    #[serde(default)]
    unreadable: Vec<Stamp>,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
struct Stamp {
    rel: String,
    size: u64,
    mtime_ms: i64,
}

/// `$XDG_CACHE_HOME/libredaw/library` or `~/.cache/libredaw/library`.
pub fn default_cache_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    Some(base.join("libredaw").join("library"))
}

fn cache_path(cache_dir: &Path, src: &Source) -> PathBuf {
    let h = doc::sha256::sha256_hex(src.root.to_string_lossy().as_bytes());
    cache_dir.join(format!("{}-{}.toml", src.id_prefix, &h[..12]))
}

struct Found {
    rel: String,
    abs: PathBuf,
    size: u64,
    mtime_ms: i64,
}

fn walk(root: &Path) -> Vec<Found> {
    let mut out = Vec::new();
    let mut stack = vec![(root.to_path_buf(), String::new(), 0u32)];
    while let Some((dir, rel, depth)) = stack.pop() {
        let Ok(rd) = fs::read_dir(&dir) else { continue };
        for e in rd.filter_map(Result::ok) {
            let name = e.file_name();
            let Some(name) = name.to_str() else { continue };
            if name.starts_with('.') {
                continue;
            }
            let Ok(ft) = e.file_type() else { continue };
            let child_rel = if rel.is_empty() {
                name.to_string()
            } else {
                format!("{rel}/{name}")
            };
            if ft.is_dir() {
                if depth < 16 {
                    stack.push((e.path(), child_rel, depth + 1));
                }
            } else if (ft.is_file() || ft.is_symlink())
                && Format::of_path(Path::new(name)).is_some()
                && let Ok(md) = fs::metadata(e.path())
                && md.is_file()
            {
                let mtime_ms = md
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map_or(0, |d| d.as_millis() as i64);
                out.push(Found {
                    rel: child_rel,
                    abs: e.path(),
                    size: md.len(),
                    mtime_ms,
                });
            }
        }
    }
    out.sort_by(|a, b| a.rel.cmp(&b.rel));
    out
}

fn entry_id(src: &Source, f: &Found) -> String {
    let key = format!("{}\0{}\0{}\0{}", src.label, f.rel, f.size, f.mtime_ms);
    format!(
        "{}-{}",
        src.id_prefix,
        &doc::sha256::sha256_hex(key.as_bytes())[..16]
    )
}

fn make_entry(src: &Source, f: &Found) -> io::Result<SoundEntry> {
    let h = read_header(&f.abs)?;
    let c = classify(&f.rel);
    let top = f.rel.split_once('/').map(|(t, _)| t);
    let pack = match top {
        Some(t) => format!("{}: {t}", src.pack_prefix),
        None => src.pack_prefix.clone(),
    };
    let file = f.rel.rsplit('/').next().unwrap_or(&f.rel);
    let name = file
        .rsplit_once('.')
        .map_or(file, |(s, _)| s)
        .replace('_', " ");
    Ok(SoundEntry {
        id: entry_id(src, f),
        name,
        role: c.role.to_string(),
        tags: c.tags,
        pack,
        kit: c.kit,
        instrument: c.instrument,
        group: c.group,
        seq: c.seq,
        rel: f.rel.clone(),
        size: f.size,
        mtime_ms: f.mtime_ms,
        format: h.format,
        sample_rate: h.sample_rate,
        channels: h.channels,
        frames: h.frames,
        tempo_bpm: c.tempo_bpm,
        key: c.key,
        root_note: c.root_note,
        root_source: c.root_note.map(|_| "name".to_string()),
        local_only: true,
        path: f.abs.clone(),
    })
}

fn threads() -> usize {
    std::thread::available_parallelism().map_or(2, |n| n.get().clamp(1, 8))
}

fn load_cache(path: &Path, src: &Source) -> Option<CacheFile> {
    let text = fs::read_to_string(path).ok()?;
    let c: CacheFile = toml::from_str(&text).ok()?;
    (c.version == CACHE_VERSION && c.root == src.root.to_string_lossy()).then_some(c)
}

fn write_cache(path: &Path, c: &CacheFile) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let text = toml::to_string(c).map_err(io::Error::other)?;
    let tmp = path.with_extension("toml.tmp");
    fs::write(
        &tmp,
        format!(
            "# SPDX-License-Identifier: CC0-1.0\n# Oto library index cache, rebuilt automatically.\n{text}"
        ),
    )?;
    fs::rename(tmp, path)
}

/// Scans `src`, using and updating the cache in `cache_dir`. Read only on
/// the library itself.
pub fn scan(src: &Source, cache_dir: &Path) -> io::Result<ScanResult> {
    scan_with(src, cache_dir, true)
}

/// Like [`scan`]. With `estimate_pitch` false nothing is decoded, so the scan
/// stays fast (headers only) and numbered multisamples are left unplaced;
/// a later scan with `estimate_pitch` true places them (about 2 s once, then
/// cached).
pub fn scan_with(src: &Source, cache_dir: &Path, estimate_pitch: bool) -> io::Result<ScanResult> {
    let start = Instant::now();
    if !src.root.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "library folder not found",
        ));
    }
    let cpath = cache_path(cache_dir, src);
    let cache = load_cache(&cpath, src);
    let mut old: HashMap<String, SoundEntry> = HashMap::new();
    let mut old_bad: HashMap<String, Stamp> = HashMap::new();
    if let Some(c) = cache {
        old = c.sound.into_iter().map(|e| (e.rel.clone(), e)).collect();
        old_bad = c
            .unreadable
            .into_iter()
            .map(|s| (s.rel.clone(), s))
            .collect();
    }
    let old_total = old.len() + old_bad.len();

    let found = walk(&src.root);
    let present = |rel: &String| found.binary_search_by(|f| f.rel.cmp(rel)).is_ok();
    let gone = old
        .keys()
        .chain(old_bad.keys())
        .filter(|k| !present(k))
        .count();
    let mut stats = ScanStats {
        files: found.len(),
        ..ScanStats::default()
    };
    let mut entries: Vec<SoundEntry> = Vec::with_capacity(found.len());
    let mut bad: Vec<Stamp> = Vec::new();
    let mut todo: Vec<&Found> = Vec::new();
    for f in &found {
        if let Some(e) = old.remove(&f.rel)
            && e.size == f.size
            && e.mtime_ms == f.mtime_ms
        {
            let mut e = e;
            e.path = f.abs.clone();
            entries.push(e);
            stats.reused += 1;
        } else if old_bad
            .get(&f.rel)
            .is_some_and(|s| s.size == f.size && s.mtime_ms == f.mtime_ms)
        {
            bad.push(Stamp {
                rel: f.rel.clone(),
                size: f.size,
                mtime_ms: f.mtime_ms,
            });
        } else {
            todo.push(f);
        }
    }
    stats.parsed = todo.len();

    let next = AtomicUsize::new(0);
    let done: Mutex<Vec<(usize, io::Result<SoundEntry>)>> = Mutex::new(Vec::new());
    std::thread::scope(|s| {
        for _ in 0..threads().min(todo.len().max(1)) {
            s.spawn(|| {
                let mut local = Vec::new();
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(f) = todo.get(i) else { break };
                    local.push((i, make_entry(src, f)));
                }
                if let Ok(mut d) = done.lock() {
                    d.extend(local);
                }
            });
        }
    });
    for (i, r) in done.into_inner().unwrap_or_default() {
        match r {
            Ok(e) => entries.push(e),
            Err(_) => {
                stats.unreadable += 1;
                bad.push(Stamp {
                    rel: todo[i].rel.clone(),
                    size: todo[i].size,
                    mtime_ms: todo[i].mtime_ms,
                });
            }
        }
    }
    entries.sort_by(|a, b| a.rel.cmp(&b.rel));
    bad.sort_by(|a, b| a.rel.cmp(&b.rel));
    stats.unreadable = bad.len();
    stats.removed = gone;

    let pitched = crate::zones::assign_roots(&mut entries, estimate_pitch);
    stats.pitched = pitched;

    let changed = stats.parsed > 0 || pitched > 0 || old_total != entries.len() + bad.len();
    if changed {
        let cf = CacheFile {
            version: CACHE_VERSION,
            root: src.root.to_string_lossy().into_owned(),
            label: src.label.clone(),
            sound: entries.clone(),
            unreadable: bad,
        };
        stats.cache_written = write_cache(&cpath, &cf).is_ok();
    }
    stats.elapsed = start.elapsed();
    Ok(ScanResult {
        index: LibraryIndex {
            root: src.root.clone(),
            label: src.label.clone(),
            entries,
        },
        stats,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{TempDir, fl_like_tree, sine_wav, write};

    fn src(t: &TempDir) -> Source {
        Source {
            root: t.path().join("Packs"),
            label: "FL Studio 2026".into(),
            id_prefix: "fl".into(),
            pack_prefix: "FL Studio".into(),
        }
    }

    #[test]
    fn cold_then_warm() {
        let t = TempDir::new("idx");
        fl_like_tree(t.path());
        let cache = t.path().join("cache");
        let s = src(&t);
        let a = scan(&s, &cache).unwrap();
        assert_eq!(a.stats.reused, 0);
        assert_eq!(a.stats.parsed, a.stats.files);
        assert!(a.stats.cache_written);
        // .nfo and .fst are not audio.
        assert_eq!(a.index.entries.len(), 27);
        let b = scan(&s, &cache).unwrap();
        assert_eq!(b.stats.parsed, 0);
        assert_eq!(b.stats.reused, 27);
        assert!(!b.stats.cache_written);
        assert_eq!(a.index.entries, b.index.entries);
    }

    #[test]
    fn rescans_only_changed_files() {
        let t = TempDir::new("idx2");
        fl_like_tree(t.path());
        let cache = t.path().join("cache");
        let s = src(&t);
        let a = scan(&s, &cache).unwrap();
        write(
            t.path(),
            "Packs/Drums/Kicks/New Kick.wav",
            &sine_wav(80.0, 0.1, 44100),
        );
        write(
            t.path(),
            "Packs/Drums/Kicks/909 Kick.wav",
            &sine_wav(80.0, 0.3, 44100),
        );
        let b = scan(&s, &cache).unwrap();
        assert_eq!(b.stats.parsed, 2);
        assert_eq!(b.index.entries.len(), a.index.entries.len() + 1);
        let old = a
            .index
            .entries
            .iter()
            .find(|e| e.rel.ends_with("909 Kick.wav"))
            .unwrap();
        let new = b
            .index
            .entries
            .iter()
            .find(|e| e.rel.ends_with("909 Kick.wav"))
            .unwrap();
        assert_ne!(old.id, new.id, "id follows size and mtime");
        fs::remove_file(t.path().join("Packs/Drums/Kicks/New Kick.wav")).unwrap();
        assert_eq!(scan(&s, &cache).unwrap().stats.removed, 1);
    }

    #[test]
    fn entries_are_local_only_and_complete() {
        let t = TempDir::new("idx3");
        fl_like_tree(t.path());
        let r = scan(&src(&t), &t.path().join("cache")).unwrap();
        assert!(
            r.index
                .entries
                .iter()
                .all(|e| e.local_only && e.path.is_absolute())
        );
        let k = r
            .index
            .find(
                &r.index
                    .entries
                    .iter()
                    .find(|e| e.name == "909 Kick")
                    .unwrap()
                    .id,
            )
            .unwrap();
        assert_eq!(k.role, "kick");
        assert_eq!(k.pack, "FL Studio: Drums");
        assert_eq!(k.sample_rate, 44100);
        assert_eq!(k.channels, 1);
        assert!((k.duration_s() - 0.2).abs() < 0.001);
        let lp = r.index.entries.iter().find(|e| e.role == "loop").unwrap();
        assert_eq!((lp.tempo_bpm, lp.key.as_deref()), (Some(120), Some("Am")));
        let info = k.to_sound_info();
        assert_eq!(info.role, "kick");
    }

    #[test]
    fn pitch_is_optional_and_cached() {
        let t = TempDir::new("idx6");
        fl_like_tree(t.path());
        let cache = t.path().join("cache");
        let s = src(&t);
        let jazz = |r: &ScanResult| {
            r.index
                .entries
                .iter()
                .find(|e| e.rel.ends_with("Jazz Guitar (1).wav"))
                .unwrap()
                .root_note
        };
        let fast = scan_with(&s, &cache, false).unwrap();
        assert_eq!((fast.stats.pitched, jazz(&fast)), (0, None));
        let full = scan_with(&s, &cache, true).unwrap();
        assert_eq!(full.stats.parsed, 0);
        assert_eq!((full.stats.pitched, jazz(&full)), (3, Some(57)));
        assert!(full.stats.cache_written);
        let warm = scan(&s, &cache).unwrap();
        assert_eq!((warm.stats.pitched, jazz(&warm)), (0, Some(57)));
    }

    #[test]
    fn cache_is_canonical_text() {
        let t = TempDir::new("idx4");
        fl_like_tree(t.path());
        let cache = t.path().join("cache");
        let s = src(&t);
        scan(&s, &cache).unwrap();
        let f = fs::read_dir(&cache)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let a = fs::read_to_string(&f).unwrap();
        fs::remove_file(&f).unwrap();
        scan(&s, &cache).unwrap();
        assert_eq!(a, fs::read_to_string(&f).unwrap());
        assert!(a.contains("[[sound]]"));
    }

    #[test]
    fn unreadable_audio_is_skipped_and_remembered() {
        let t = TempDir::new("idx5");
        write(
            t.path(),
            "Packs/Drums/Kicks/Broken Kick.wav",
            b"not audio at all",
        );
        write(
            t.path(),
            "Packs/Drums/Kicks/Good Kick.wav",
            &sine_wav(60.0, 0.1, 44100),
        );
        let s = src(&t);
        let a = scan(&s, &t.path().join("cache")).unwrap();
        assert_eq!((a.index.entries.len(), a.stats.unreadable), (1, 1));
        let b = scan(&s, &t.path().join("cache")).unwrap();
        assert_eq!(b.stats.parsed, 0);
    }
}
