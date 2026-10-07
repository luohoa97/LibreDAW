// SPDX-License-Identifier: GPL-3.0-or-later
//! Save, open, new, close, and launch recovery (SPEC 7, Amendments 9 and
//! 10). Disk work runs on worker threads (`App::tasks`); the GTK thread only
//! captures plugin state and shows the result.
//!
//! There is no "save changes?" dialog: closing, quitting, opening another
//! project, and SIGTERM all save first. A project that was never saved goes
//! to `~/Music/LibreDAW/Untitled <n>.ldaw`. If that save fails the window
//! stays open and shows the error.

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};

use gtk::gio;
use gtk::glib;
use gtk::prelude::*;

use crate::app::App;
use doc::bundle;
use doc::document::Document;
use doc::persist::{self, Dirs, LastSession, ViewState};

/// The user's real directories.
pub fn real_dirs() -> Dirs {
    // `LIBREDAW_HOME=/some/dir` keeps all state under one directory
    // (screenshots and tests must not touch the user's files).
    if let Some(root) = std::env::var_os("LIBREDAW_HOME") {
        let root = PathBuf::from(root);
        return Dirs {
            music: root.join("music"),
            data: root.join("data"),
            config: root.join("config"),
        };
    }
    let home = glib::home_dir();
    Dirs {
        music: glib::user_special_dir(glib::UserDirectory::Music)
            .unwrap_or_else(|| home.join("Music")),
        data: glib::user_data_dir(),
        config: glib::user_config_dir(),
    }
}

pub fn display_name(path: &Option<PathBuf>) -> String {
    match path {
        Some(p) => p
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "Project".into()),
        None => "Untitled".into(),
    }
}

/// Makes sure a chosen path ends in `.ldaw`.
pub fn with_extension(p: &Path) -> PathBuf {
    if p.extension().is_some_and(|e| e == "ldaw") {
        p.to_path_buf()
    } else {
        let mut s = p.as_os_str().to_os_string();
        s.push(".ldaw");
        PathBuf::from(s)
    }
}

fn window_of(w: &impl IsA<gtk::Widget>) -> Option<gtk::Window> {
    w.root().and_then(|r| r.downcast::<gtk::Window>().ok())
}

/// Directory the autosave worker writes to: `<project>/.autosave`, or this
/// session's recovery bundle while the project has never been saved.
pub fn autosave_dir(app: &App) -> PathBuf {
    match app.ui.borrow().path.clone() {
        Some(p) => bundle::autosave_path(&p),
        None => app.dirs.recovery_bundle(&app.session_id),
    }
}

fn unix_secs(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// "14:05" for today, "Mar 3, 14:05" otherwise, in local time.
pub fn time_text(t: SystemTime) -> String {
    let Ok(dt) = glib::DateTime::from_unix_local(unix_secs(t) as i64) else {
        return "earlier".into();
    };
    let today = glib::DateTime::now_local().ok();
    let same_day = today.map(|n| n.ymd() == dt.ymd()).unwrap_or(false);
    let fmt = if same_day { "%H:%M" } else { "%b %-d, %H:%M" };
    dt.format(fmt)
        .map(|s| s.to_string())
        .unwrap_or_else(|_| "earlier".into())
}

// ---------------------------------------------------------------------------
// Save

/// Saves to the project's path, or to the next `Untitled <n>.ldaw` for a
/// project that was never saved. `then` gets the path or the error text.
pub fn save_current(app: &Rc<App>, then: impl FnOnce(Result<PathBuf, String>) + 'static) {
    let (existing, target) = {
        let p = app.ui.borrow().path.clone();
        match p {
            Some(p) => (true, p),
            None => (false, app.dirs.next_untitled()),
        }
    };
    if let Some(parent) = target.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        let msg = format!("Cannot create {}: {e}", parent.display());
        app.toast(&msg);
        then(Err(msg));
        return;
    }
    let doc = app.session.borrow_mut().snapshot_for_save();
    // Samples that undo and redo can still reach stay in the bundle (17.2).
    let keep = app.session.borrow().editor.history().keep();
    let home = app.session.borrow().sample_home().map(Path::to_path_buf);
    let view = app.collect_view();
    let t2 = target.clone();
    let a = app.clone();
    app.tasks.spawn(
        "save",
        move || {
            let r = (|| {
                if let Some(home) = &home {
                    let mut hashes: std::collections::BTreeSet<String> =
                        keep.samples.iter().cloned().collect();
                    hashes.extend(
                        doc.project
                            .samples
                            .iter()
                            .filter(|s| !s.local_only)
                            .map(|s| s.hash.clone()),
                    );
                    copy_samples(home, &t2, hashes).map_err(|e| e.to_string())?;
                }
                bundle::save_keeping(&t2, &doc, &keep).map_err(|e| e.to_string())?;
                bundle::clear_autosave(&t2).map_err(|e| e.to_string())
            })();
            if r.is_ok()
                && let Some(v) = view
            {
                let _ = v.write(&t2);
            }
            r
        },
        move |r| match r {
            Ok(_) => {
                after_saved(&a, &target, !existing);
                then(Ok(target));
            }
            Err(e) => {
                let msg = format!("Could not save the project: {e}");
                a.toast(&msg);
                then(Err(msg));
            }
        },
    );
}

fn after_saved(app: &Rc<App>, path: &Path, was_untitled: bool) {
    app.ui.borrow_mut().path = Some(path.to_path_buf());
    point_samples_at(app, Some(path));
    app.session.borrow_mut().editor.mark_saved();
    persist::remove_recovery(&app.dirs, &app.session_id);
    let note =
        was_untitled.then(|| format!("Your unsaved project was saved to {}", path.display()));
    let _ = LastSession {
        path: Some(path.to_path_buf()),
        note,
    }
    .write(&app.dirs);
    app.notify();
}

/// Ctrl+S: save in place, or ask where for a new project.
pub fn save(parent: &impl IsA<gtk::Widget>, app: &Rc<App>) {
    if app.ui.borrow().path.is_some() {
        // "Saved" only once it is on disk (a failure has its own toast).
        let a = app.clone();
        save_current(app, move |r| {
            if r.is_ok() {
                a.toast("Saved");
            }
        });
    } else {
        save_as(parent, app);
    }
}

pub fn save_as(parent: &impl IsA<gtk::Widget>, app: &Rc<App>) {
    let dialog = gtk::FileDialog::builder()
        .title("Save project")
        .initial_name(format!("{}.ldaw", display_name(&app.ui.borrow().path)))
        .build();
    let app = app.clone();
    dialog.save(
        window_of(parent).as_ref(),
        gio::Cancellable::NONE,
        move |res| {
            if let Ok(f) = res
                && let Some(p) = f.path()
            {
                let path = with_extension(&p);
                app.ui.borrow_mut().path = Some(path.clone());
                save_current(&app, move |r| {
                    let _ = (&path, r);
                });
            }
        },
    );
}

// ---------------------------------------------------------------------------
// Open and new

/// Saves unsaved work first, then runs `go`. A failed save stops here: the
/// error is already shown and the current project stays open.
fn save_then(app: &Rc<App>, go: impl FnOnce() + 'static) {
    if !app.is_dirty() {
        go();
        return;
    }
    save_current(app, move |r| {
        if r.is_ok() {
            go();
        }
    });
}

pub fn open(parent: &impl IsA<gtk::Widget>, app: &Rc<App>) {
    let parent_w = parent.clone().upcast::<gtk::Widget>();
    let app2 = app.clone();
    save_then(app, move || {
        let dialog = gtk::FileDialog::builder().title("Open project").build();
        let app = app2.clone();
        dialog.select_folder(
            window_of(&parent_w).as_ref(),
            gio::Cancellable::NONE,
            move |res| {
                if let Ok(f) = res
                    && let Some(p) = f.path()
                {
                    open_path(&app, p);
                }
            },
        );
    });
}

pub fn new_project(app: &Rc<App>) {
    let a = app.clone();
    save_then(app, move || fresh_project(&a));
}

/// An empty project with one pattern so the grid is usable.
pub fn fresh_project(a: &Rc<App>) {
    point_samples_at(a, None);
    a.session
        .borrow_mut()
        .replace_document(Document::new(), false);
    {
        let mut ui = a.ui.borrow_mut();
        ui.path = None;
        ui.pattern = None;
        ui.channel = None;
        ui.channel_cleared = false;
    }
    // A starter beat: one pattern and four channels, ready to play.
    crate::channels::add_starter_beat(a);
    a.session.borrow_mut().editor.mark_saved();
    a.notify();
}

/// Loads a bundle on a worker thread. Autosaved work newer than the saved
/// project is applied as one undoable step with a toast (Amendment 10).
pub fn open_path(app: &Rc<App>, path: PathBuf) {
    let p = path.clone();
    let a = app.clone();
    app.tasks.spawn(
        "open",
        move || persist::open_with_recovery(&p),
        move |r| match r {
            Ok(o) => {
                if !o.saved.missing_blobs.is_empty() {
                    a.toast(&format!(
                        "{} plugin state file(s) are missing; those plugins start with defaults",
                        o.saved.missing_blobs.len()
                    ));
                }
                point_samples_at(&a, Some(&path));
                a.session.borrow_mut().replace_document(o.saved.doc, true);
                {
                    let mut ui = a.ui.borrow_mut();
                    ui.path = Some(path.clone());
                    ui.pattern = None;
                    ui.channel = None;
                    ui.channel_cleared = false;
                }
                if let Some(v) = ViewState::read(&path) {
                    a.apply_view(&v);
                }
                a.notify();
                if let Some(rec) = o.recovered {
                    a.session.borrow_mut().apply_recovered(&rec.loaded.doc);
                    a.notify();
                    recovered_toast(&a, rec.modified);
                }
                let _ = LastSession {
                    path: Some(path),
                    note: None,
                }
                .write(&a.dirs);
            }
            Err(e) => a.toast(&format!("Cannot open the project: {e}")),
        },
    );
}

/// Opens the work of a crashed session that never saved (a recovery bundle).
pub fn open_recovery_bundle(app: &Rc<App>, bundle_dir: PathBuf, modified: SystemTime) {
    let (a, d) = (app.clone(), bundle_dir.clone());
    app.tasks.spawn(
        "recover",
        move || bundle::load(&d),
        move |r| match r {
            Ok(l) => {
                // The crashed session's bundle goes away later; keep its
                // samples in this run's recovery bundle.
                let ours = a.dirs.recovery_bundle(&a.session_id);
                let hashes: Vec<String> = l
                    .doc
                    .project
                    .samples
                    .iter()
                    .map(|s| s.hash.clone())
                    .collect();
                if let Err(e) = copy_samples(&bundle_dir, &ours, hashes) {
                    a.toast(&format!("Could not keep the recovered samples: {e}"));
                }
                point_samples_at(&a, None);
                a.session
                    .borrow_mut()
                    .replace_document(Document::new(), true);
                a.session.borrow_mut().apply_recovered(&l.doc);
                a.ui.borrow_mut().path = None;
                a.ui.borrow_mut().pattern = None;
                a.reset_selection();
                a.notify();
                recovered_toast(&a, modified);
                // The old recovery bundle goes away once this session has
                // written its own.
                *a.stale_recovery.borrow_mut() = Some(bundle_dir);
            }
            Err(e) => a.toast(&format!("Could not recover the autosaved project: {e}")),
        },
    );
}

/// "Restored your unsaved changes from 15:54" with Undo (the recovery is
/// one undoable step back to the last save).
fn recovered_toast(app: &Rc<App>, modified: SystemTime) {
    let a = app.clone();
    app.toast_action(
        &format!("Restored your unsaved changes from {}", time_text(modified)),
        "Undo",
        move || a.undo(),
    );
}

/// At launch: reopen the last project (with its view), or recover a crashed
/// session, or start fresh. Also shows the note left by the last close.
pub fn restore_last_session(app: &Rc<App>) {
    let last = LastSession::read(&app.dirs);
    if let Some(n) = &last.note {
        app.toast(n);
        let _ = LastSession {
            path: last.path.clone(),
            note: None,
        }
        .write(&app.dirs);
    }
    if let Some(p) = last.path.filter(|p| persist::is_bundle(p)) {
        open_path(app, p);
        return;
    }
    if let Some((b, m)) = persist::find_recovery_bundles(&app.dirs).into_iter().next() {
        open_recovery_bundle(app, b, m);
        return;
    }
    fresh_project(app);
}

// ---------------------------------------------------------------------------
// Samples (15.1, 17.2)

/// Where the samples of the open project live: the saved bundle, or this
/// run's recovery bundle while the project has no path yet.
pub fn point_samples_at(app: &Rc<App>, path: Option<&Path>) {
    let home = match path {
        Some(p) => p.to_path_buf(),
        None => app.dirs.recovery_bundle(&app.session_id),
    };
    app.session.borrow_mut().set_sample_home(Some(home));
}

/// Copies the sample files `hashes` that exist under `from` into `to`,
/// never overwriting. Both are bundle directories.
pub fn copy_samples(
    from: &Path,
    to: &Path,
    hashes: impl IntoIterator<Item = String>,
) -> std::io::Result<()> {
    if from == to {
        return Ok(());
    }
    let (src, dst) = (
        doc::samples::samples_root(from),
        doc::samples::samples_root(to),
    );
    for h in hashes {
        let name = doc::samples::sample_file_name(&h);
        let (s, d) = (src.join(&name), dst.join(&name));
        if !s.is_file() || d.exists() {
            continue;
        }
        std::fs::create_dir_all(&dst)?;
        let tmp = dst.join(format!(".copy-{name}.tmp"));
        std::fs::copy(&s, &tmp)?;
        std::fs::rename(&tmp, &d)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_is_added_once() {
        assert_eq!(
            with_extension(Path::new("/a/Song")),
            PathBuf::from("/a/Song.ldaw")
        );
        assert_eq!(
            with_extension(Path::new("/a/Song.ldaw")),
            PathBuf::from("/a/Song.ldaw")
        );
        assert_eq!(
            with_extension(Path::new("/a/Song.v2")),
            PathBuf::from("/a/Song.v2.ldaw")
        );
    }

    #[test]
    fn names_for_titles() {
        assert_eq!(display_name(&None), "Untitled");
        assert_eq!(
            display_name(&Some(PathBuf::from("/x/My Beat.ldaw"))),
            "My Beat"
        );
    }
}
