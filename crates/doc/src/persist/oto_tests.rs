// SPDX-License-Identifier: GPL-3.0-or-later
//! The rename to Oto (SPEC Amendment 19): folder, extension, headers.

use super::*;
use crate::document::{Document, apply};
use protocol::edit::Edit;
use std::sync::atomic::{AtomicU32, Ordering};

struct Tmp(PathBuf);

impl Tmp {
    fn new() -> Tmp {
        static N: AtomicU32 = AtomicU32::new(0);
        let p = std::env::temp_dir().join(format!(
            "oto-persist-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&p).unwrap();
        Tmp(p)
    }
    fn dirs(&self) -> Dirs {
        Dirs {
            music: self.0.join("Music"),
            data: self.0.join("data"),
            config: self.0.join("config"),
        }
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn tempo_doc(bpm: f64) -> Document {
    apply(&Document::new(), &Edit::SetTempo { bpm }).unwrap().0
}

#[test]
fn projects_folder_is_oto() {
    let t = Tmp::new();
    let d = t.dirs();
    assert_eq!(d.projects(), d.music.join("Oto"));
    assert!(!d.music.join("LibreDAW").exists());
}

#[test]
fn old_projects_folder_is_renamed_to_oto() {
    let t = Tmp::new();
    let d = t.dirs();
    fs::create_dir_all(d.music.join("LibreDAW/Song.ldaw")).unwrap();
    assert_eq!(d.projects(), d.music.join("Oto"));
    assert!(d.music.join("Oto/Song.ldaw").is_dir());
    assert!(!d.music.join("LibreDAW").exists());
    assert_eq!(
        d.projects(),
        d.music.join("Oto"),
        "second call changes nothing"
    );
}

#[test]
fn both_folders_leave_the_old_one_alone() {
    let t = Tmp::new();
    let d = t.dirs();
    fs::create_dir_all(d.music.join("LibreDAW/Old.ldaw")).unwrap();
    fs::create_dir_all(d.music.join("Oto")).unwrap();
    assert_eq!(d.projects(), d.music.join("Oto"));
    assert!(d.music.join("LibreDAW/Old.ldaw").is_dir());
}

#[test]
fn failed_rename_keeps_the_old_folder() {
    use std::os::unix::fs::PermissionsExt;
    let t = Tmp::new();
    let d = t.dirs();
    fs::create_dir_all(d.music.join("LibreDAW/Old.ldaw")).unwrap();
    fs::set_permissions(&d.music, fs::Permissions::from_mode(0o555)).unwrap();
    let can_write = fs::create_dir(d.music.join("probe")).is_ok();
    let got = d.projects();
    fs::set_permissions(&d.music, fs::Permissions::from_mode(0o755)).unwrap();
    if can_write {
        // A user that ignores permissions (root): the rename cannot fail.
        return;
    }
    assert_eq!(got, d.music.join("LibreDAW"));
    assert!(d.music.join("LibreDAW/Old.ldaw").is_dir());
}

#[test]
fn recovery_bundles_of_both_names_are_found() {
    let t = Tmp::new();
    let d = t.dirs();
    crate::bundle::save(&d.recovery_root().join("old.ldaw"), &tempo_doc(100.0)).unwrap();
    crate::bundle::save(&d.recovery_root().join("new.oto"), &tempo_doc(110.0)).unwrap();
    let mut names: Vec<_> = find_recovery_bundles(&d)
        .into_iter()
        .map(|(p, _)| p.file_name().unwrap().to_string_lossy().to_string())
        .collect();
    names.sort();
    assert_eq!(names, vec!["new.oto", "old.ldaw"]);
}

#[test]
fn last_session_header_is_oto_and_the_old_header_still_reads() {
    let l = LastSession {
        path: Some(PathBuf::from("/m/Oto/a.oto")),
        note: None,
    };
    assert!(l.emit().starts_with("# Oto: what to reopen"));
    let old = "# LibreDAW: what to reopen at the next launch.\npath = \"/m/LibreDAW/a.ldaw\"\n";
    assert_eq!(
        LastSession::parse(old).path,
        Some(PathBuf::from("/m/LibreDAW/a.ldaw"))
    );
}
