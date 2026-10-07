// SPDX-License-Identifier: GPL-3.0-or-later
//! What Home lists (SPEC 19.1): unsaved work first, then saved projects,
//! newest first. No GTK here; `home.rs` draws it and `ProjectList` answers
//! from the same scan.

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use doc::bundle::{AUTOSAVE_DIR, PROJECT_FILE};
use doc::persist::{self, Dirs, LastSession};

/// How many projects in the folder Home reads.
const MAX_PROJECTS: usize = 200;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// The work of a session that ended without saving.
    Recovery,
    /// A saved project with changes newer than its last save.
    Autosave,
    Saved,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HomeItem {
    pub name: String,
    pub path: PathBuf,
    /// Seconds since the epoch.
    pub modified: u64,
    pub kind: Kind,
}

#[derive(Clone, Debug, Default)]
pub struct HomeList {
    pub unsaved: Vec<HomeItem>,
    pub recent: Vec<HomeItem>,
}

impl HomeList {
    pub fn is_empty(&self) -> bool {
        self.unsaved.is_empty() && self.recent.is_empty()
    }
}

fn mtime(p: &Path) -> u64 {
    std::fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn name_of(p: &Path) -> String {
    p.file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "Project".into())
}

/// Newest first; equal times by name.
fn newest_first(v: &mut [HomeItem]) {
    v.sort_by(|a, b| {
        b.modified
            .cmp(&a.modified)
            .then_with(|| a.name.cmp(&b.name))
    });
}

/// Everything Home shows. `exclude` are recovery bundles that belong to
/// this run (its own, or one already restored into it).
pub fn scan(dirs: &Dirs, exclude: &[PathBuf]) -> HomeList {
    let mut unsaved: Vec<HomeItem> = persist::find_recovery_bundles(dirs)
        .into_iter()
        .filter(|(p, _)| !exclude.contains(p))
        .map(|(path, m)| HomeItem {
            name: "Unsaved Project".into(),
            modified: m
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            path,
            kind: Kind::Recovery,
        })
        .collect();

    let mut paths: Vec<PathBuf> = std::fs::read_dir(dirs.projects())
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| persist::is_bundle(p))
                .take(MAX_PROJECTS)
                .collect()
        })
        .unwrap_or_default();
    if let Some(p) = LastSession::read(dirs).path
        && persist::is_bundle(&p)
        && !paths.contains(&p)
    {
        paths.push(p);
    }

    let mut recent = Vec::new();
    for path in paths {
        let saved = mtime(&path.join(PROJECT_FILE));
        if persist::has_autosave(&path) {
            let auto = mtime(&path.join(AUTOSAVE_DIR).join(PROJECT_FILE));
            if auto > saved {
                unsaved.push(HomeItem {
                    name: name_of(&path),
                    path: path.clone(),
                    modified: auto,
                    kind: Kind::Autosave,
                });
            }
        }
        recent.push(HomeItem {
            name: name_of(&path),
            path,
            modified: saved,
            kind: Kind::Saved,
        });
    }
    newest_first(&mut unsaved);
    newest_first(&mut recent);
    HomeList { unsaved, recent }
}

/// "Edited 2 hours ago".
pub fn age_text(now: u64, then: u64) -> String {
    let d = now.saturating_sub(then);
    let n = |v: u64, unit: &str| format!("Edited {v} {unit}{} ago", if v == 1 { "" } else { "s" });
    match d {
        0..60 => "Edited just now".into(),
        60..3600 => n(d / 60, "minute"),
        3600..86_400 => n(d / 3600, "hour"),
        86_400..172_800 => "Edited yesterday".into(),
        172_800..2_592_000 => n(d / 86_400, "day"),
        2_592_000..31_536_000 => n(d / 2_592_000, "month"),
        _ => n(d / 31_536_000, "year"),
    }
}

pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// What Discard and Move to Trash send to the trash: a recovery bundle or
/// saved project whole, but only the autosave of a saved project.
pub fn trash_target(item: &HomeItem) -> PathBuf {
    match item.kind {
        Kind::Autosave => item.path.join(AUTOSAVE_DIR),
        Kind::Recovery | Kind::Saved => item.path.clone(),
    }
}

/// The folder a renamed project moves to, or what is wrong with the name.
pub fn rename_target(path: &Path, new_name: &str) -> Result<PathBuf, String> {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_string())
        .unwrap_or_else(|| "oto".into());
    let mut name = new_name.trim();
    if let Some(s) = name.strip_suffix(&format!(".{ext}")) {
        name = s.trim_end();
    }
    if name.is_empty() {
        return Err("Type a name for the project".into());
    }
    if name.starts_with('.') || name.contains(['/', '\\']) {
        return Err("That name cannot be used".into());
    }
    let target = path.with_file_name(format!("{name}.{ext}"));
    if target != path && target.exists() {
        return Err("A project with that name already exists".into());
    }
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    fn temp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("oto-home-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn dirs(root: &Path) -> Dirs {
        Dirs {
            music: root.join("music"),
            data: root.join("data"),
            config: root.join("config"),
        }
    }

    fn project(dir: &Path, name: &str, age_s: u64) -> PathBuf {
        let p = dir.join(name);
        std::fs::create_dir_all(&p).unwrap();
        let f = p.join(PROJECT_FILE);
        std::fs::write(&f, "x").unwrap();
        let t = SystemTime::now() - Duration::from_secs(age_s);
        std::fs::File::options()
            .write(true)
            .open(&f)
            .unwrap()
            .set_modified(t)
            .unwrap();
        p
    }

    #[test]
    fn saved_projects_are_newest_first() {
        let root = temp("sort");
        let d = dirs(&root);
        let folder = d.projects();
        project(&folder, "Old.oto", 5 * 86_400);
        project(&folder, "New.oto", 60);
        project(&folder, "Middle.oto", 3 * 3600);
        std::fs::write(folder.join("notes.txt"), "not a project").unwrap();
        let l = scan(&d, &[]);
        let names: Vec<_> = l.recent.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["New", "Middle", "Old"]);
        assert!(l.unsaved.is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn unsaved_work_is_its_own_list_and_skips_this_run() {
        let root = temp("unsaved");
        let d = dirs(&root);
        let crashed = project(&d.recovery_root(), "1-10.oto", 600);
        let ours = project(&d.recovery_root(), "2-20.oto", 10);
        let saved = project(&d.projects(), "Song.oto", 7200);
        // An autosave newer than the save counts as unsaved work too.
        let auto = saved.join(AUTOSAVE_DIR);
        std::fs::create_dir_all(&auto).unwrap();
        std::fs::write(auto.join(PROJECT_FILE), "y").unwrap();
        let l = scan(&d, std::slice::from_ref(&ours));
        let kinds: Vec<_> = l.unsaved.iter().map(|i| (i.kind, i.path.clone())).collect();
        assert_eq!(
            kinds,
            [(Kind::Autosave, saved.clone()), (Kind::Recovery, crashed)]
        );
        assert_eq!(l.recent.len(), 1);
        assert_eq!(l.recent[0].kind, Kind::Saved);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_empty_home_is_empty() {
        let root = temp("empty");
        assert!(scan(&dirs(&root), &[]).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn ages_read_plainly() {
        let now = 10_000_000;
        assert_eq!(age_text(now, now - 5), "Edited just now");
        assert_eq!(age_text(now, now - 60), "Edited 1 minute ago");
        assert_eq!(age_text(now, now - 7200), "Edited 2 hours ago");
        assert_eq!(age_text(now, now - 90_000), "Edited yesterday");
        assert_eq!(age_text(now, now - 3 * 86_400), "Edited 3 days ago");
        assert_eq!(age_text(now, now + 50), "Edited just now");
    }

    #[test]
    fn discard_moves_things_to_the_trash_not_the_project_data() {
        let item = |kind, p: &str| HomeItem {
            name: "x".into(),
            path: PathBuf::from(p),
            modified: 0,
            kind,
        };
        assert_eq!(
            trash_target(&item(Kind::Recovery, "/r/1.oto")),
            PathBuf::from("/r/1.oto")
        );
        // Discarding the unsaved changes of a saved project keeps the project.
        assert_eq!(
            trash_target(&item(Kind::Autosave, "/m/Song.oto")),
            PathBuf::from("/m/Song.oto").join(AUTOSAVE_DIR)
        );
    }

    #[test]
    fn home_never_deletes() {
        // Discard and Move to Trash go through gio's trash; nothing in the
        // Home code removes files itself.
        let src =
            std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/home.rs"))
                .unwrap();
        assert!(src.contains(".trash("));
        for bad in ["remove_dir_all", "remove_file", "remove_dir("] {
            assert!(!src.contains(bad), "home.rs must not use {bad}");
        }
    }

    #[test]
    fn renames_keep_the_extension_and_refuse_bad_names() {
        let root = temp("rename");
        let a = project(&root, "A.oto", 1);
        project(&root, "B.oto", 1);
        assert_eq!(
            rename_target(&a, " Fresh ").unwrap(),
            root.join("Fresh.oto")
        );
        assert_eq!(
            rename_target(&a, "Fresh.oto").unwrap(),
            root.join("Fresh.oto")
        );
        assert_eq!(rename_target(&a, "A").unwrap(), a);
        assert!(rename_target(&a, "").is_err());
        assert!(rename_target(&a, "../x").is_err());
        assert!(rename_target(&a, ".hidden").is_err());
        assert!(rename_target(&a, "B").is_err());
        let _ = std::fs::remove_dir_all(&root);
    }
}
