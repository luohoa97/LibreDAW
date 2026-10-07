// SPDX-License-Identifier: GPL-3.0-or-later
//! Samples on disk (SPEC 15.1, 15.3, 17.2 fmt-4 and license-1).
//!
//! Bundle samples are content-addressed and immutable: `samples/<hash>.wav`,
//! where `<hash>` is the SHA-256 of the file, written with the 7.4
//! procedure (tmp file, fsync, rename, fsync of the directory) and never
//! overwritten. Autosave and history reference the same files.
//!
//! A `local_only` sample is never copied into the bundle. It is recorded as
//! hash plus absolute path in a per-machine file
//! (`~/.local/share/libredaw/local-samples.toml`), and the bundle gets a
//! generated `.gitignore` entry for it. A sample that cannot be found is a
//! placeholder: the project still loads and saves.

use std::collections::{BTreeMap, HashSet};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use protocol::model::{Project, SampleRef};
use protocol::validate::check_name;

use crate::bundle::{AUTOSAVE_DIR, BundleError, fsync_dir_of, io_error, write_atomic};
use crate::sha256::{Sha256, to_hex};

pub const SAMPLES_DIR: &str = "samples";
pub const LOCAL_SAMPLES_FILE: &str = "local-samples.toml";
/// Largest sample file `import_sample` accepts.
pub const MAX_SAMPLE_BYTES: u64 = 1 << 30;

/// File name of a bundle sample: `<hash>.wav`.
pub fn sample_file_name(hash: &str) -> String {
    format!("{hash}.wav")
}

/// The hash in a bundle sample file name, if it has our form.
pub fn parse_sample_file_name(name: &str) -> Option<&str> {
    let h = name.strip_suffix(".wav")?;
    is_hash(h).then_some(h)
}

fn is_hash(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Where a bundle's samples live: an autosave copy shares the samples of
/// the bundle that contains it.
pub fn samples_root(bundle: &Path) -> PathBuf {
    if bundle.file_name().is_some_and(|n| n == AUTOSAVE_DIR)
        && let Some(parent) = bundle.parent()
    {
        return parent.join(SAMPLES_DIR);
    }
    bundle.join(SAMPLES_DIR)
}

/// `$XDG_DATA_HOME/libredaw/local-samples.toml`, or the same under
/// `~/.local/share`. `None` if neither variable is usable.
pub fn default_local_samples_path() -> Option<PathBuf> {
    let base = match std::env::var_os("XDG_DATA_HOME") {
        Some(v) if Path::new(&v).is_absolute() => PathBuf::from(v),
        _ => PathBuf::from(std::env::var_os("HOME").filter(|h| !h.is_empty())?)
            .join(".local")
            .join("share"),
    };
    Some(base.join("libredaw").join(LOCAL_SAMPLES_FILE))
}

// ---------------------------------------------------------------------------
// The per-machine registry of local-only samples

/// `local-samples.toml`: hash to absolute path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalSamples {
    pub file: PathBuf,
    pub entries: BTreeMap<String, PathBuf>,
}

impl LocalSamples {
    /// Reads the file. A missing or damaged file is an empty registry:
    /// local samples are then placeholders until they are added again.
    pub fn load(file: &Path) -> LocalSamples {
        let text = fs::read_to_string(file).unwrap_or_default();
        LocalSamples {
            file: file.to_path_buf(),
            entries: parse_registry(&text),
        }
    }

    pub fn get(&self, hash: &str) -> Option<&Path> {
        self.entries.get(hash).map(PathBuf::as_path)
    }

    /// Writes the file with the 7.4 steps, creating its directory.
    pub fn save(&self) -> Result<(), BundleError> {
        if let Some(dir) = self.file.parent() {
            fs::create_dir_all(dir).map_err(io_error(dir))?;
        }
        write_atomic(&self.file, emit_registry(&self.entries).as_bytes())
    }
}

fn emit_registry(entries: &BTreeMap<String, PathBuf>) -> String {
    let mut s = String::from(
        "# Local-only samples on this machine (never copied into a project).\n\
         # Written by Oto; safe to delete.\n",
    );
    for (hash, path) in entries {
        s.push_str("\n[[sample]]\n");
        s.push_str(&format!("hash = {}\n", quote(hash)));
        s.push_str(&format!("path = {}\n", quote(&path.to_string_lossy())));
    }
    s
}

/// A TOML basic string.
fn quote(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if c.is_control() => o.push_str(&format!("\\u{:04X}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

/// Parses a TOML basic string followed by nothing or a comment.
fn unquote(v: &str) -> Option<String> {
    let mut it = v.trim().strip_prefix('"')?.chars();
    let mut out = String::new();
    loop {
        match it.next()? {
            '"' => break,
            '\\' => match it.next()? {
                '"' => out.push('"'),
                '\\' => out.push('\\'),
                'n' => out.push('\n'),
                'r' => out.push('\r'),
                't' => out.push('\t'),
                'u' => out.push(hex_char(&mut it, 4)?),
                'U' => out.push(hex_char(&mut it, 8)?),
                _ => return None,
            },
            c => out.push(c),
        }
    }
    let rest: String = it.collect();
    let rest = rest.trim();
    (rest.is_empty() || rest.starts_with('#')).then_some(out)
}

fn hex_char(it: &mut std::str::Chars<'_>, n: usize) -> Option<char> {
    let mut v = 0u32;
    for _ in 0..n {
        v = v * 16 + it.next()?.to_digit(16)?;
    }
    char::from_u32(v)
}

fn parse_registry(text: &str) -> BTreeMap<String, PathBuf> {
    let mut out = BTreeMap::new();
    let mut hash: Option<String> = None;
    let mut path: Option<String> = None;
    let mut flush = |hash: &mut Option<String>, path: &mut Option<String>| {
        if let (Some(h), Some(p)) = (hash.take(), path.take())
            && is_hash(&h)
            && Path::new(&p).is_absolute()
        {
            out.insert(h, PathBuf::from(p));
        }
        *hash = None;
        *path = None;
    };
    for line in text.lines() {
        let line = line.trim();
        if line == "[[sample]]" {
            flush(&mut hash, &mut path);
        } else if let Some((k, v)) = line.split_once('=') {
            match k.trim() {
                "hash" => hash = unquote(v),
                "path" => path = unquote(v),
                _ => {}
            }
        }
    }
    flush(&mut hash, &mut path);
    out
}

// ---------------------------------------------------------------------------
// Import

/// Imports a WAV file as a project sample (SPEC 15.1) and returns the
/// `SampleRef` to register with `Edit::AddSample`.
///
/// Not `local_only`: the file is copied to `<bundle>/samples/<hash>.wav`
/// unless that file already exists (it is never overwritten).
/// `local_only`: nothing is copied; hash and absolute path go to the
/// registry at `default_local_samples_path()`.
pub fn import_sample(
    bundle: &Path,
    path: &Path,
    local_only: bool,
) -> Result<SampleRef, BundleError> {
    let registry = default_local_samples_path().ok_or(BundleError::NoDataDir)?;
    import_sample_with(bundle, path, local_only, &registry)
}

/// `import_sample` with an explicit `local-samples.toml` path (for tests).
pub fn import_sample_with(
    bundle: &Path,
    path: &Path,
    local_only: bool,
    registry: &Path,
) -> Result<SampleRef, BundleError> {
    let orig_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .map(str::to_string)
        .filter(|n| check_name("sample.orig_name", n).is_ok())
        .ok_or_else(|| BundleError::BadName(path.to_path_buf()))?;
    let mut src = File::open(path).map_err(io_error(path))?;
    let meta = src.metadata().map_err(io_error(path))?;
    if !meta.is_file() {
        return Err(BundleError::NotWav(path.to_path_buf()));
    }
    if meta.len() > MAX_SAMPLE_BYTES {
        return Err(BundleError::TooLarge(path.to_path_buf()));
    }
    let mut head = [0u8; 12];
    // A user library also holds FLAC, Ogg and WavPack files; those are only
    // ever referenced in place (`local_only`), never copied into a bundle.
    if src.read_exact(&mut head).is_err()
        || !(is_wav_head(&head) || (local_only && is_other_audio_head(&head)))
    {
        return Err(BundleError::NotWav(path.to_path_buf()));
    }

    let dir = bundle.join(SAMPLES_DIR);
    let (hash, size) = if local_only {
        let (h, n) = hash_stream(&head, &mut src, None, path)?;
        (h, n)
    } else {
        fs::create_dir_all(&dir).map_err(io_error(&dir))?;
        static N: AtomicU32 = AtomicU32::new(0);
        let tmp = dir.join(format!(
            ".import-{}-{}.tmp",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let result = copy_into_bundle(&head, &mut src, path, &dir, &tmp);
        if result.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        result?
    };

    if local_only {
        let abs = fs::canonicalize(path).map_err(io_error(path))?;
        if abs.to_str().is_none() {
            return Err(BundleError::BadName(path.to_path_buf()));
        }
        let mut reg = LocalSamples::load(registry);
        if reg.entries.get(&hash) != Some(&abs) {
            reg.entries.insert(hash.clone(), abs);
            reg.save()?;
        }
    }
    Ok(SampleRef {
        hash,
        orig_name,
        size,
        local_only,
    })
}

fn is_wav_head(head: &[u8; 12]) -> bool {
    &head[0..4] == b"RIFF" && &head[8..12] == b"WAVE"
}

fn is_other_audio_head(head: &[u8; 12]) -> bool {
    head.starts_with(b"fLaC") || head.starts_with(b"OggS") || head.starts_with(b"wvpk")
}

/// Hashes the rest of `src` (after `head`), copying it to `out` if given.
fn hash_stream(
    head: &[u8],
    src: &mut File,
    mut out: Option<&mut File>,
    path: &Path,
) -> Result<(String, u64), BundleError> {
    let mut hasher = Sha256::new();
    hasher.update(head);
    let mut size = head.len() as u64;
    if let Some(o) = out.as_deref_mut() {
        o.write_all(head).map_err(io_error(path))?;
    }
    let mut buf = vec![0u8; 64 << 10];
    loop {
        let n = src.read(&mut buf).map_err(io_error(path))?;
        if n == 0 {
            break;
        }
        size += n as u64;
        if size > MAX_SAMPLE_BYTES {
            return Err(BundleError::TooLarge(path.to_path_buf()));
        }
        hasher.update(&buf[..n]);
        if let Some(o) = out.as_deref_mut() {
            o.write_all(&buf[..n]).map_err(io_error(path))?;
        }
    }
    Ok((to_hex(&hasher.finalize()), size))
}

/// Steps of 7.4 for one immutable file: tmp, fsync, rename, directory fsync.
fn copy_into_bundle(
    head: &[u8],
    src: &mut File,
    path: &Path,
    dir: &Path,
    tmp: &Path,
) -> Result<(String, u64), BundleError> {
    let mut out = File::create(tmp).map_err(io_error(tmp))?;
    let (hash, size) = hash_stream(head, src, Some(&mut out), path)?;
    out.sync_all().map_err(io_error(tmp))?;
    drop(out);
    let fin = dir.join(sample_file_name(&hash));
    match fs::metadata(&fin) {
        Ok(m) if m.len() == size => {
            // Same content already stored: keep it, drop the copy.
            fs::remove_file(tmp).map_err(io_error(tmp))?;
        }
        Ok(_) => return Err(BundleError::SampleConflict(hash)),
        Err(_) => {
            fs::rename(tmp, &fin).map_err(io_error(&fin))?;
            fsync_dir_of(dir)?;
        }
    }
    Ok((hash, size))
}

// ---------------------------------------------------------------------------
// Lookup

/// The file that holds a sample's audio, if it can be found: the bundle
/// copy, or for a local-only sample the registered path.
pub fn resolve_sample(bundle: &Path, sample: &SampleRef, local: &LocalSamples) -> Option<PathBuf> {
    let p = if sample.local_only {
        local.get(&sample.hash)?.to_path_buf()
    } else {
        samples_root(bundle).join(sample_file_name(&sample.hash))
    };
    p.is_file().then_some(p)
}

/// Hashes of samples the project names whose file cannot be found. Those
/// are placeholders (silence, a toast); loading and saving still work.
pub fn missing_samples(bundle: &Path, project: &Project, local: &LocalSamples) -> Vec<String> {
    project
        .samples
        .iter()
        .filter(|s| resolve_sample(bundle, s, local).is_none())
        .map(|s| s.hash.clone())
        .collect()
}

// ---------------------------------------------------------------------------
// Save support

/// Every 64-digit lowercase hex string in quotes in `text`. A superset of
/// the sample hashes of a project file, which is all garbage collection needs.
pub(crate) fn hashes_in_text(text: &str) -> HashSet<String> {
    text.split('"')
        .filter(|p| is_hash(p))
        .map(str::to_string)
        .collect()
}

/// Sample files and leftover tmp files in `<bundle>/samples/` that `marks`
/// does not name, sorted.
pub(crate) fn stale_sample_files(
    bundle: &Path,
    marks: &HashSet<String>,
) -> Result<Vec<String>, BundleError> {
    let dir = bundle.join(SAMPLES_DIR);
    let rd = match fs::read_dir(&dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(io_error(&dir)(e)),
    };
    let mut stale = Vec::new();
    for entry in rd {
        let entry = entry.map_err(io_error(&dir))?;
        let Some(n) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let ours = match parse_sample_file_name(&n) {
            Some(h) => !marks.contains(h),
            None => n.ends_with(".tmp"),
        };
        if ours {
            stale.push(n);
        }
    }
    stale.sort();
    Ok(stale)
}

const IGNORE_BEGIN: &str = "# BEGIN libredaw (generated; edits between the markers are lost)";
const IGNORE_END: &str = "# END libredaw";

/// The managed block of the bundle's `.gitignore`: the autosave folder and
/// a path for each local-only sample, in case a copy ever lands in `samples/`.
pub fn gitignore_block(local_hashes: &[&str]) -> String {
    let mut hashes: Vec<&str> = local_hashes.to_vec();
    hashes.sort_unstable();
    hashes.dedup();
    let mut s = format!("{IGNORE_BEGIN}\n/{AUTOSAVE_DIR}/\n");
    for h in hashes {
        s.push_str(&format!("/{SAMPLES_DIR}/{}\n", sample_file_name(h)));
    }
    s.push_str(IGNORE_END);
    s.push('\n');
    s
}

/// Brings the managed block of `<bundle>/.gitignore` up to date. Without
/// local-only samples the block is removed (and the file, if nothing else
/// is in it). Lines outside the markers are kept.
pub(crate) fn update_gitignore(bundle: &Path, local_hashes: &[&str]) -> Result<(), BundleError> {
    let file = bundle.join(".gitignore");
    let old = fs::read_to_string(&file).unwrap_or_default();
    let (before, after) = match (old.find(IGNORE_BEGIN), old.find(IGNORE_END)) {
        (Some(b), Some(e)) if b < e => {
            let end = e + IGNORE_END.len();
            let end = old[end..].find('\n').map_or(old.len(), |i| end + i + 1);
            (old[..b].to_string(), old[end..].to_string())
        }
        _ => (old.clone(), String::new()),
    };
    let had_block = old.contains(IGNORE_BEGIN);
    let mut new = before;
    new.push_str(&after);
    if !local_hashes.is_empty() {
        if !new.is_empty() && !new.ends_with('\n') {
            new.push('\n');
        }
        new.push_str(&gitignore_block(local_hashes));
    }
    if new == old {
        return Ok(());
    }
    if new.trim().is_empty() {
        if had_block {
            match fs::remove_file(&file) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(io_error(&file)(e)),
            }
        }
        return Ok(());
    }
    fs::create_dir_all(bundle).map_err(io_error(bundle))?;
    write_atomic(&file, new.as_bytes())
}

#[cfg(test)]
mod tests;
