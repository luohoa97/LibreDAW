// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;
use crate::document::{Document, apply};
use protocol::edit::Edit;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

struct Tmp(PathBuf);

impl Tmp {
    fn new() -> Tmp {
        static N: AtomicU32 = AtomicU32::new(0);
        let p = std::env::temp_dir().join(format!(
            "libredaw-persist-{}-{}",
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
    fn bundle(&self) -> PathBuf {
        self.0.join("Song.ldaw")
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
fn untitled_projects_get_the_first_free_number() {
    let t = Tmp::new();
    let d = t.dirs();
    assert_eq!(d.next_untitled(), d.projects().join("Untitled 1.ldaw"));
    fs::create_dir_all(d.projects().join("Untitled 1.ldaw")).unwrap();
    fs::create_dir_all(d.projects().join("Untitled 2.ldaw")).unwrap();
    assert_eq!(d.next_untitled(), d.projects().join("Untitled 3.ldaw"));
    fs::remove_dir_all(d.projects().join("Untitled 1.ldaw")).unwrap();
    assert_eq!(d.next_untitled(), d.projects().join("Untitled 1.ldaw"));
}

#[test]
fn recovery_bundles_live_under_the_data_dir() {
    let t = Tmp::new();
    let d = t.dirs();
    let id = session_id(1_700_000_000, 42);
    assert_eq!(id, "1700000000-42");
    assert_eq!(
        d.recovery_bundle(&id),
        d.data.join("libredaw/recovery/1700000000-42.ldaw")
    );
}

#[test]
fn last_session_round_trips_awkward_text() {
    let l = LastSession {
        path: Some(PathBuf::from(
            "/home/u/Music/LibreDAW/My \"Song\" \\ 1.ldaw",
        )),
        note: Some("Saved to ~/Music/LibreDAW/Untitled 1.ldaw\nsecond line".into()),
    };
    assert_eq!(LastSession::parse(&l.emit()), l);
    assert_eq!(LastSession::parse(""), LastSession::default());
    assert_eq!(
        LastSession::parse("path = nonsense\nfoo = 1"),
        LastSession::default()
    );
}

#[test]
fn last_session_file_is_written_atomically_under_config() {
    let t = Tmp::new();
    let d = t.dirs();
    assert_eq!(LastSession::read(&d), LastSession::default());
    let l = LastSession {
        path: Some(PathBuf::from("/x/y.ldaw")),
        note: None,
    };
    l.write(&d).unwrap();
    assert_eq!(LastSession::read(&d), l);
    assert!(!d.last_file().with_extension("toml.tmp").exists());
}

#[test]
fn view_state_round_trips_and_tolerates_junk() {
    let v = ViewState {
        pattern: Some(7),
        channel: Some(3),
        track: Some(0),
        px_per_tick: 0.25,
        row_h: 20.5,
        scroll_x: 120.0,
        scroll_y: 640.0,
        snap: 3,
        split_rack: 300,
        split_mixer: 900,
    };
    assert_eq!(ViewState::parse(&v.emit()), v);
    let junk = ViewState::parse("px_per_tick = NaN\nrow_h = abc\nsnap = -1\nwhat = 2\n=\n");
    assert_eq!(junk, ViewState::default());
}

#[test]
fn view_file_lives_in_the_bundle_and_is_not_the_project() {
    let t = Tmp::new();
    crate::bundle::save(&t.bundle(), &tempo_doc(100.0)).unwrap();
    assert!(ViewState::read(&t.bundle()).is_none());
    let v = ViewState {
        pattern: Some(2),
        ..ViewState::default()
    };
    v.write(&t.bundle()).unwrap();
    assert_eq!(ViewState::read(&t.bundle()), Some(v));
    // The project still loads and saves; the view file is untouched by saves.
    crate::bundle::save(&t.bundle(), &tempo_doc(101.0)).unwrap();
    assert!(ViewState::read(&t.bundle()).is_some());
    let text = fs::read_to_string(t.bundle().join(PROJECT_FILE)).unwrap();
    assert!(!text.contains("px_per_tick"));
}

#[test]
fn a_plain_project_opens_without_recovery() {
    let t = Tmp::new();
    crate::bundle::save(&t.bundle(), &tempo_doc(100.0)).unwrap();
    let o = open_with_recovery(&t.bundle()).unwrap();
    assert_eq!(o.saved.doc.project.tempo_bpm, 100.0);
    assert!(o.recovered.is_none());
}

#[test]
fn newer_autosave_is_offered_as_recovery() {
    let t = Tmp::new();
    crate::bundle::save(&t.bundle(), &tempo_doc(100.0)).unwrap();
    std::thread::sleep(Duration::from_millis(30));
    crate::bundle::save_autosave(&t.bundle(), &tempo_doc(150.0)).unwrap();
    let o = open_with_recovery(&t.bundle()).unwrap();
    assert_eq!(
        o.saved.doc.project.tempo_bpm, 100.0,
        "saved project is the base"
    );
    let r = o.recovered.expect("autosave is newer");
    assert_eq!(r.loaded.doc.project.tempo_bpm, 150.0);
    assert!(r.modified <= SystemTime::now());
}

#[test]
fn older_autosave_is_ignored() {
    let t = Tmp::new();
    crate::bundle::save_autosave(&t.bundle(), &tempo_doc(150.0)).unwrap();
    std::thread::sleep(Duration::from_millis(30));
    crate::bundle::save(&t.bundle(), &tempo_doc(100.0)).unwrap();
    let o = open_with_recovery(&t.bundle()).unwrap();
    assert!(o.recovered.is_none());
}

#[test]
fn a_damaged_autosave_does_not_block_opening() {
    let t = Tmp::new();
    crate::bundle::save(&t.bundle(), &tempo_doc(100.0)).unwrap();
    std::thread::sleep(Duration::from_millis(30));
    let a = crate::bundle::autosave_path(&t.bundle());
    fs::create_dir_all(&a).unwrap();
    fs::write(a.join(PROJECT_FILE), "garbage {{{").unwrap();
    let o = open_with_recovery(&t.bundle()).unwrap();
    assert_eq!(o.saved.doc.project.tempo_bpm, 100.0);
    assert!(o.recovered.is_none());
}

#[test]
fn recovery_bundles_are_found_newest_first_and_only_bundles() {
    let t = Tmp::new();
    let d = t.dirs();
    crate::bundle::save(&d.recovery_bundle("a"), &tempo_doc(100.0)).unwrap();
    std::thread::sleep(Duration::from_millis(30));
    crate::bundle::save(&d.recovery_bundle("b"), &tempo_doc(110.0)).unwrap();
    fs::create_dir_all(d.recovery_root().join("stray")).unwrap();
    fs::write(d.recovery_root().join("note.txt"), "x").unwrap();
    let found = find_recovery_bundles(&d);
    let names: Vec<_> = found
        .iter()
        .map(|(p, _)| p.file_name().unwrap().to_string_lossy().to_string())
        .collect();
    assert_eq!(names, vec!["b.ldaw", "a.ldaw"]);
    remove_recovery(&d, "b");
    assert_eq!(find_recovery_bundles(&d).len(), 1);
    assert!(find_recovery_bundles(&Tmp::new().dirs()).is_empty());
}

#[test]
fn recovery_message_text() {
    assert_eq!(
        recovered_message("14:05"),
        "Recovered unsaved work from 14:05. Undo to go back to the last save."
    );
}

#[test]
fn bundle_probes() {
    let t = Tmp::new();
    assert!(!is_bundle(&t.bundle()));
    crate::bundle::save(&t.bundle(), &tempo_doc(100.0)).unwrap();
    assert!(is_bundle(&t.bundle()));
    assert!(!has_autosave(&t.bundle()));
    crate::bundle::save_autosave(&t.bundle(), &tempo_doc(101.0)).unwrap();
    assert!(has_autosave(&t.bundle()));
}
