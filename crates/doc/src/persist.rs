// SPDX-License-Identifier: GPL-3.0-or-later
//! Session persistence that is not the project itself (SPEC 7.6, Amendments
//! 9 and 10): where never-saved projects go, which project to reopen, the
//! per-project view file, and recovery of autosaved work. No GTK here, so it
//! is tested with temp directories.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::bundle::{self, AUTOSAVE_DIR, BundleError, Loaded, PROJECT_FILE};

pub const VIEW_FILE: &str = ".view.toml";

// ---------------------------------------------------------------------------
// Where things live. The roots are passed in so tests can use temp dirs.

/// Paths that depend on the user's directories.
#[derive(Clone, Debug)]
pub struct Dirs {
    /// `~/Music` (XDG music dir).
    pub music: PathBuf,
    /// `~/.local/share`.
    pub data: PathBuf,
    /// `~/.config`.
    pub config: PathBuf,
}

impl Dirs {
    pub fn projects(&self) -> PathBuf {
        self.music.join("LibreDAW")
    }

    pub fn recovery_root(&self) -> PathBuf {
        self.data.join("libredaw").join("recovery")
    }

    /// `~/.local/share/libredaw/local-samples.toml` (17.2): where local-only
    /// samples are recorded, outside every project.
    pub fn local_samples_file(&self) -> PathBuf {
        self.data
            .join("libredaw")
            .join(crate::samples::LOCAL_SAMPLES_FILE)
    }

    pub fn last_file(&self) -> PathBuf {
        self.config.join("libredaw").join("last.toml")
    }

    pub fn settings_file(&self) -> PathBuf {
        self.config.join("libredaw").join("settings.toml")
    }

    /// First free `Untitled <n>.ldaw` in the projects folder.
    pub fn next_untitled(&self) -> PathBuf {
        let root = self.projects();
        let mut n = 1;
        loop {
            let p = root.join(format!("Untitled {n}.ldaw"));
            if !p.exists() {
                return p;
            }
            n += 1;
        }
    }

    /// A recovery bundle path for a session id.
    pub fn recovery_bundle(&self, id: &str) -> PathBuf {
        self.recovery_root().join(format!("{id}.ldaw"))
    }
}

/// An id for this run's recovery bundle: unique enough for one user.
pub fn session_id(now_unix_s: u64, pid: u32) -> String {
    format!("{now_unix_s}-{pid}")
}

// ---------------------------------------------------------------------------
// last.toml: what to reopen at launch.

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LastSession {
    pub path: Option<PathBuf>,
    /// A message to show once at the next launch.
    pub note: Option<String>,
}

/// Wraps `s` in double quotes, escaping `"`, `\` and newlines; other control
/// characters are dropped. The inverse of [`unquote`].
pub fn quote(s: &str) -> String {
    let mut o = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            c if c.is_control() => {}
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

/// Reads a string written by [`quote`]; `None` if it is not a well-formed
/// quoted string.
pub fn unquote(s: &str) -> Option<String> {
    let s = s.trim().strip_prefix('"')?.strip_suffix('"')?;
    let mut o = String::new();
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c == '\\' {
            match it.next()? {
                'n' => o.push('\n'),
                '"' => o.push('"'),
                '\\' => o.push('\\'),
                _ => return None,
            }
        } else {
            o.push(c);
        }
    }
    Some(o)
}

/// The `key = value` lines of a tiny TOML-like file, trimmed; comments, blank
/// lines and lines without `=` are skipped.
pub fn key_values(text: &str) -> Vec<(&str, &str)> {
    text.lines()
        .filter_map(|l| {
            let l = l.trim();
            if l.is_empty() || l.starts_with('#') {
                return None;
            }
            l.split_once('=').map(|(k, v)| (k.trim(), v.trim()))
        })
        .collect()
}

impl LastSession {
    pub fn emit(&self) -> String {
        let mut o = String::from("# LibreDAW: what to reopen at the next launch.\n");
        if let Some(p) = &self.path {
            o.push_str(&format!("path = {}\n", quote(&p.to_string_lossy())));
        }
        if let Some(n) = &self.note {
            o.push_str(&format!("note = {}\n", quote(n)));
        }
        o
    }

    pub fn parse(text: &str) -> LastSession {
        let mut l = LastSession::default();
        for (k, v) in key_values(text) {
            match k {
                "path" => l.path = unquote(v).map(PathBuf::from),
                "note" => l.note = unquote(v),
                _ => {}
            }
        }
        l
    }

    pub fn read(dirs: &Dirs) -> LastSession {
        fs::read_to_string(dirs.last_file())
            .map(|t| LastSession::parse(&t))
            .unwrap_or_default()
    }

    pub fn write(&self, dirs: &Dirs) -> std::io::Result<()> {
        let f = dirs.last_file();
        if let Some(d) = f.parent() {
            fs::create_dir_all(d)?;
        }
        let tmp = f.with_extension("toml.tmp");
        fs::write(&tmp, self.emit())?;
        fs::rename(&tmp, &f)
    }
}

// ---------------------------------------------------------------------------
// .view.toml: how the window looked (not part of the project format).

const PAGES: [&str; 3] = ["pattern", "song", "mixer"];
const FOCUSES: [&str; 3] = ["both", "steps", "notes"];

#[derive(Clone, Debug, PartialEq)]
pub struct ViewState {
    pub pattern: Option<u32>,
    pub channel: Option<u32>,
    pub track: Option<u32>,
    pub px_per_tick: f64,
    pub row_h: f64,
    pub scroll_x: f64,
    pub scroll_y: f64,
    pub snap: u32,
    /// Page shown: "pattern", "song", or "mixer".
    pub page: String,
    /// "both", "steps", or "notes": what the Pattern page shows.
    pub focus: String,
    pub sounds_open: bool,
    pub inspector_open: bool,
    /// Share of the Pattern page height given to steps, 0.1 to 0.9.
    pub split: f64,
}

impl Default for ViewState {
    fn default() -> ViewState {
        ViewState {
            pattern: None,
            channel: None,
            track: None,
            px_per_tick: 0.12,
            row_h: 16.0,
            scroll_x: 0.0,
            scroll_y: 0.0,
            snap: 0,
            page: "pattern".into(),
            focus: "both".into(),
            sounds_open: false,
            inspector_open: false,
            split: 0.42,
        }
    }
}

impl ViewState {
    pub fn emit(&self) -> String {
        let mut o = String::from("# Window view of this project. Safe to delete.\n");
        for (k, v) in [
            ("pattern", self.pattern),
            ("channel", self.channel),
            ("track", self.track),
        ] {
            if let Some(v) = v {
                o.push_str(&format!("{k} = {v}\n"));
            }
        }
        o.push_str(&format!("px_per_tick = {}\n", self.px_per_tick));
        o.push_str(&format!("row_h = {}\n", self.row_h));
        o.push_str(&format!("scroll_x = {}\n", self.scroll_x));
        o.push_str(&format!("scroll_y = {}\n", self.scroll_y));
        o.push_str(&format!("snap = {}\n", self.snap));
        o.push_str(&format!("page = {}\n", quote(&self.page)));
        o.push_str(&format!("focus = {}\n", quote(&self.focus)));
        o.push_str(&format!("sounds_open = {}\n", self.sounds_open));
        o.push_str(&format!("inspector_open = {}\n", self.inspector_open));
        o.push_str(&format!("split = {}\n", self.split));
        o
    }

    /// Reads what it understands and ignores the rest, so an old or edited
    /// file never blocks opening a project.
    pub fn parse(text: &str) -> ViewState {
        let mut v = ViewState::default();
        let f = |s: &str| s.parse::<f64>().ok().filter(|x| x.is_finite());
        for (k, val) in key_values(text) {
            match k {
                "pattern" => v.pattern = val.parse().ok(),
                "channel" => v.channel = val.parse().ok(),
                "track" => v.track = val.parse().ok(),
                "px_per_tick" => v.px_per_tick = f(val).unwrap_or(v.px_per_tick),
                "row_h" => v.row_h = f(val).unwrap_or(v.row_h),
                "scroll_x" => v.scroll_x = f(val).unwrap_or(v.scroll_x),
                "scroll_y" => v.scroll_y = f(val).unwrap_or(v.scroll_y),
                "snap" => v.snap = val.parse().unwrap_or(v.snap),
                "page" => {
                    if let Some(p) = unquote(val).filter(|p| PAGES.contains(&p.as_str())) {
                        v.page = p;
                    }
                }
                "focus" => {
                    if let Some(p) = unquote(val).filter(|p| FOCUSES.contains(&p.as_str())) {
                        v.focus = p;
                    }
                }
                "sounds_open" => v.sounds_open = val.parse().unwrap_or(v.sounds_open),
                "inspector_open" => v.inspector_open = val.parse().unwrap_or(v.inspector_open),
                "split" => v.split = f(val).map(|x| x.clamp(0.1, 0.9)).unwrap_or(v.split),
                _ => {}
            }
        }
        v
    }

    pub fn read(bundle: &Path) -> Option<ViewState> {
        fs::read_to_string(bundle.join(VIEW_FILE))
            .ok()
            .map(|t| ViewState::parse(&t))
    }

    pub fn write(&self, bundle: &Path) -> std::io::Result<()> {
        let f = bundle.join(VIEW_FILE);
        let tmp = bundle.join(format!("{VIEW_FILE}.tmp"));
        fs::write(&tmp, self.emit())?;
        fs::rename(&tmp, &f)
    }
}

// ---------------------------------------------------------------------------
// Recovery.

/// A project as found at launch or on open.
#[derive(Debug)]
pub struct Opened {
    /// The saved project, or for a recovery-only bundle the autosave itself.
    pub saved: Loaded,
    /// Newer autosaved work, applied as one undoable step on top of `saved`.
    pub recovered: Option<Recovered>,
}

#[derive(Debug)]
pub struct Recovered {
    pub loaded: Loaded,
    pub modified: SystemTime,
}

/// Opens a bundle. If it has an autosave newer than `project.toml`, loads
/// that too so the caller can offer it as one undoable step (Amendment 10).
pub fn open_with_recovery(dir: &Path) -> Result<Opened, BundleError> {
    let saved = bundle::load(dir)?;
    let recovered = if bundle::autosave_is_newer(dir) {
        let a = bundle::autosave_path(dir);
        let modified = fs::metadata(a.join(PROJECT_FILE))
            .and_then(|m| m.modified())
            .unwrap_or_else(|_| SystemTime::now());
        // A damaged autosave must not stop the saved project from opening.
        bundle::load_autosave(dir)
            .ok()
            .map(|loaded| Recovered { loaded, modified })
    } else {
        None
    };
    Ok(Opened { saved, recovered })
}

/// Recovery bundles of crashed sessions, newest first.
pub fn find_recovery_bundles(dirs: &Dirs) -> Vec<(PathBuf, SystemTime)> {
    let mut v: Vec<(PathBuf, SystemTime)> = fs::read_dir(dirs.recovery_root())
        .map(|rd| {
            rd.filter_map(|e| {
                let p = e.ok()?.path();
                let m = fs::metadata(p.join(PROJECT_FILE)).ok()?.modified().ok()?;
                (p.extension().is_some_and(|x| x == "ldaw")).then_some((p, m))
            })
            .collect()
        })
        .unwrap_or_default();
    v.sort_by_key(|b| std::cmp::Reverse(b.1));
    v
}

/// Removes a recovery bundle once its work is safe elsewhere.
pub fn remove_recovery(dirs: &Dirs, id: &str) {
    let _ = fs::remove_dir_all(dirs.recovery_bundle(id));
}

/// Text for the toast after recovery.
pub fn recovered_message(time_text: &str) -> String {
    format!("Recovered unsaved work from {time_text}. Undo to go back to the last save.")
}

/// Does the bundle directory look like a project (has `project.toml`)?
pub fn is_bundle(p: &Path) -> bool {
    p.join(PROJECT_FILE).is_file()
}

/// Is there an autosave inside this bundle at all?
pub fn has_autosave(p: &Path) -> bool {
    p.join(AUTOSAVE_DIR).join(PROJECT_FILE).is_file()
}

#[cfg(test)]
mod tests;
