// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;
use crate::document::tests::{Rng, random_edit};
use crate::document::{apply, apply_batch, commit_plugin_state, state_file_name};
use protocol::edit::{Edit, NewInstrument};
use protocol::ids::{InstanceId, TrackId};
use std::collections::BTreeSet;
use std::sync::atomic::{AtomicU32, Ordering};

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> TempDir {
        static N: AtomicU32 = AtomicU32::new(0);
        let p = std::env::temp_dir().join(format!(
            "libredaw-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&p).unwrap();
        TempDir(p)
    }
    fn bundle(&self) -> PathBuf {
        self.0.join("Song.ldaw")
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn blob(n: u8, len: usize) -> Arc<[u8]> {
    Arc::from(vec![n; len])
}

/// A document with a synth channel, a pattern, and two CLAP plugins
/// (one channel instrument, one insert), each with a captured blob.
fn doc_with_plugins(gen_a: u32, gen_b: u32, fill: u8) -> (Document, InstanceId, InstanceId) {
    let d = Document::new();
    let (d, ids) = apply_batch(
        &d,
        &[
            Edit::AddChannel {
                name: "Surge".into(),
                instrument: NewInstrument::Clap {
                    plugin_id: "a.b".into(),
                },
                root_key: 60,
                track: TrackId::MASTER,
            },
            Edit::AddInsert {
                track: TrackId::MASTER,
                index: 0,
                plugin_id: "c.d".into(),
            },
            Edit::AddPattern {
                name: "P".into(),
                length_steps: 16,
            },
        ],
    )
    .unwrap();
    let (a, b) = (InstanceId(ids[1]), InstanceId(ids[2]));
    let d = commit_plugin_state(
        &d,
        a,
        &state_file_name(a, gen_a),
        blob(fill, 100),
        Some("1.0"),
    )
    .unwrap();
    let d = commit_plugin_state(
        &d,
        b,
        &state_file_name(b, gen_b),
        blob(fill + 1, 50),
        Some("2.0"),
    )
    .unwrap();
    (d, a, b)
}

fn blob_files(bundle: &Path) -> BTreeSet<String> {
    fs::read_dir(bundle.join(STATE_DIR))
        .map(|rd| {
            rd.map(|e| e.unwrap().file_name().into_string().unwrap())
                .collect()
        })
        .unwrap_or_default()
}

fn referenced(d: &Document) -> BTreeSet<String> {
    let mut s = BTreeSet::new();
    for_each_clap(&d.project, |r| {
        if let Some(n) = &r.state_file {
            s.insert(n.clone());
        }
    });
    s
}

#[test]
fn save_and_load_round_trip_with_blobs() {
    let t = TempDir::new();
    let (d, a, b) = doc_with_plugins(1, 1, 7);
    let rep = save(&t.bundle(), &d).unwrap();
    assert_eq!(rep.blobs_written.len(), 2);
    assert!(rep.blobs_deleted.is_empty());
    assert!(t.bundle().join(PROJECT_FILE).is_file());
    assert_eq!(
        blob_files(&t.bundle()),
        BTreeSet::from([format!("{}-1.bin", a.0), format!("{}-1.bin", b.0)])
    );
    let l = load(&t.bundle()).unwrap();
    assert!(l.missing_blobs.is_empty());
    assert_eq!(*l.doc.project, *d.project, "project incl. blob bytes");
    assert_eq!(l.doc.next_id, d.next_id);
    assert_eq!(l.doc.revision, 0);
}

#[test]
fn saved_text_is_canonical_and_stable() {
    let t = TempDir::new();
    let (d, _, _) = doc_with_plugins(1, 1, 7);
    save(&t.bundle(), &d).unwrap();
    let first = fs::read_to_string(t.bundle().join(PROJECT_FILE)).unwrap();
    let l = load(&t.bundle()).unwrap();
    save(&t.bundle(), &l.doc).unwrap();
    let second = fs::read_to_string(t.bundle().join(PROJECT_FILE)).unwrap();
    assert_eq!(first, second);
}

#[test]
fn next_id_rules_on_load() {
    let t = TempDir::new();
    let mut d = Document::new();
    d = apply(&d, &Edit::AddTrack { name: "x".into() }).unwrap().0;
    d = apply(&d, &Edit::AddTrack { name: "y".into() }).unwrap().0;
    // Undo-like drop of the last track: next_id stays above the dropped id.
    let (d2, _) = apply(&d, &Edit::RemoveTrack { track: TrackId(2) }).unwrap();
    assert_eq!(d2.next_id, 3);
    save(&t.bundle(), &d2).unwrap();
    let l = load(&t.bundle()).unwrap();
    assert_eq!(l.doc.next_id, 3, "counter survives a save and reopen");

    // A file whose counter is behind its ids is raised.
    let path = t.bundle().join(PROJECT_FILE);
    let text = fs::read_to_string(&path)
        .unwrap()
        .replace("next_id = 3", "next_id = 1");
    fs::write(&path, text).unwrap();
    let l = load(&t.bundle()).unwrap();
    assert_eq!(l.doc.next_id, l.doc.project.max_id() + 1);
}

#[test]
fn gc_removes_unreferenced_blobs_and_tmp_files() {
    let t = TempDir::new();
    let (d1, a, b) = doc_with_plugins(1, 1, 7);
    save(&t.bundle(), &d1).unwrap();
    // New capture of `a`: generation 2. Old gen 1 is garbage afterwards.
    let d2 = commit_plugin_state(&d1, a, &state_file_name(a, 2), blob(9, 80), None).unwrap();
    fs::write(t.bundle().join(STATE_DIR).join("junk.tmp"), b"x").unwrap();
    fs::write(t.bundle().join(STATE_DIR).join("notes.txt"), b"keep").unwrap();
    let rep = save(&t.bundle(), &d2).unwrap();
    assert_eq!(rep.blobs_written, vec![format!("{}-2.bin", a.0)]);
    let mut del = rep.blobs_deleted.clone();
    del.sort();
    let mut want = vec!["junk.tmp".to_string(), format!("{}-1.bin", a.0)];
    want.sort();
    assert_eq!(del, want);
    let files = blob_files(&t.bundle());
    assert!(files.contains("notes.txt"), "foreign files are left alone");
    assert!(files.contains(&format!("{}-1.bin", b.0)));
    assert!(files.contains(&format!("{}-2.bin", a.0)));
    assert!(!files.contains(&format!("{}-1.bin", a.0)));
    // Old blob bytes stay in memory for undo and can be re-saved.
    let l = load(&t.bundle()).unwrap();
    assert!(l.missing_blobs.is_empty());
    let back = save(&t.bundle(), &d1).unwrap();
    assert_eq!(back.blobs_written, vec![format!("{}-1.bin", a.0)]);
}

#[test]
fn existing_blob_is_not_rewritten() {
    let t = TempDir::new();
    let (d, a, _) = doc_with_plugins(1, 1, 7);
    save(&t.bundle(), &d).unwrap();
    let p = t.bundle().join(STATE_DIR).join(format!("{}-1.bin", a.0));
    let before = fs::metadata(&p).unwrap().modified().unwrap();
    let rep = save(&t.bundle(), &d).unwrap();
    assert!(rep.blobs_written.is_empty());
    assert_eq!(fs::metadata(&p).unwrap().modified().unwrap(), before);
}

#[test]
fn same_name_different_content_is_refused() {
    let t = TempDir::new();
    let (d, a, _) = doc_with_plugins(1, 1, 7);
    save(&t.bundle(), &d).unwrap();
    let d2 = commit_plugin_state(&d, a, &state_file_name(a, 1), blob(7, 101), None).unwrap();
    assert!(matches!(
        save(&t.bundle(), &d2),
        Err(BundleError::BlobConflict(_))
    ));
}

#[test]
fn missing_blob_on_load_is_reported_and_resave_drops_the_reference() {
    let t = TempDir::new();
    let (d, a, _) = doc_with_plugins(1, 1, 7);
    save(&t.bundle(), &d).unwrap();
    fs::remove_file(t.bundle().join(STATE_DIR).join(format!("{}-1.bin", a.0))).unwrap();
    let l = load(&t.bundle()).unwrap();
    assert_eq!(l.missing_blobs, vec![format!("{}-1.bin", a.0)]);
    // The placeholder keeps its reference while the document is open.
    let mut found = false;
    for_each_clap(&l.doc.project, |r| {
        if r.instance == a {
            found = r.state_file.is_some() && r.state_bytes.is_none();
        }
    });
    assert!(found);
    // Saving cannot write bytes it does not have, and must not point at
    // nothing: the file still loads cleanly.
    save(&t.bundle(), &l.doc).unwrap();
    let again = load(&t.bundle()).unwrap();
    assert!(again.missing_blobs.is_empty());
}

#[test]
fn blob_kept_across_resave_when_bytes_are_not_in_memory() {
    // A loaded project holds bytes, but a placeholder-style ref with a
    // present file and no bytes must keep its reference.
    let t = TempDir::new();
    let (d, a, _) = doc_with_plugins(1, 1, 7);
    save(&t.bundle(), &d).unwrap();
    let l = load(&t.bundle()).unwrap();
    let mut proj = (*l.doc.project).clone();
    for_each_clap_mut(&mut proj, |r| r.state_bytes = None);
    let doc = Document::from_project(proj, l.doc.next_id);
    save(&t.bundle(), &doc).unwrap();
    assert!(
        blob_files(&t.bundle()).contains(&format!("{}-1.bin", a.0)),
        "referenced blob survives GC"
    );
}

#[test]
fn newer_format_and_garbage_are_refused() {
    let t = TempDir::new();
    fs::create_dir_all(t.bundle()).unwrap();
    fs::write(t.bundle().join(PROJECT_FILE), "format_version = 99\n").unwrap();
    assert!(matches!(
        load(&t.bundle()),
        Err(BundleError::Format(FormatError::TooNew { .. }))
    ));
    fs::write(t.bundle().join(PROJECT_FILE), "not toml {{{").unwrap();
    assert!(matches!(load(&t.bundle()), Err(BundleError::Format(_))));
    fs::write(t.bundle().join(PROJECT_FILE), [0xff, 0xfe, 0x00]).unwrap();
    assert!(matches!(load(&t.bundle()), Err(BundleError::Format(_))));
    assert!(matches!(
        load(&t.0.join("nothing.ldaw")),
        Err(BundleError::Io { .. })
    ));
}

#[test]
fn project_file_has_no_blob_bytes_and_is_text() {
    let t = TempDir::new();
    let (d, a, _) = doc_with_plugins(3, 4, 7);
    save(&t.bundle(), &d).unwrap();
    let text = fs::read_to_string(t.bundle().join(PROJECT_FILE)).unwrap();
    assert!(text.starts_with(&format!(
        "format_version = {}\n",
        protocol::consts::FORMAT_VERSION
    )));
    assert!(text.contains(&format!("{}-3.bin", a.0)));
}

/// The steps a save of `new` over `old` goes through, in order.
fn steps_of(dir: &Path, doc: &Document) -> Vec<SaveStep> {
    let mut v = Vec::new();
    save_with_hook(dir, doc, &mut |s| {
        v.push(s.clone());
        true
    })
    .unwrap();
    v
}

#[test]
fn crash_after_every_step_leaves_old_or_new_and_the_next_save_cleans_up() {
    // Old: both plugins gen 1. New: plugin a gen 2 (new blob, old one
    // becomes garbage), plus a tempo change so the files differ.
    let (old, a, _) = doc_with_plugins(1, 1, 7);
    let new = {
        let d = commit_plugin_state(&old, a, &state_file_name(a, 2), blob(9, 80), None).unwrap();
        apply(&d, &Edit::SetTempo { bpm: 99.0 }).unwrap().0
    };
    // Learn the step sequence from a dry run.
    let probe = TempDir::new();
    save(&probe.bundle(), &old).unwrap();
    let steps = steps_of(&probe.bundle(), &new);
    assert!(steps.len() >= 7, "{steps:?}");
    assert_eq!(steps[0], SaveStep::BlobTmpWritten(state_file_name(a, 2)));
    assert!(steps.contains(&SaveStep::StateDirSynced));
    assert!(steps.contains(&SaveStep::ProjectRenamed));
    assert!(matches!(steps.last(), Some(SaveStep::BlobDeleted(_))));

    for crash_at in 0..steps.len() {
        let t = TempDir::new();
        save(&t.bundle(), &old).unwrap();
        let mut n = 0usize;
        let r = save_with_hook(&t.bundle(), &new, &mut |_| {
            n += 1;
            n - 1 != crash_at
        });
        assert!(
            matches!(r, Err(BundleError::Crashed(_))),
            "crash {crash_at} {r:?}"
        );

        // The project loads, is exactly old or exactly new, and every
        // blob it names is readable.
        let l = load(&t.bundle()).unwrap_or_else(|e| panic!("crash {crash_at}: {e}"));
        assert!(l.missing_blobs.is_empty(), "crash {crash_at}");
        let renamed_project = steps[..=crash_at].contains(&SaveStep::ProjectRenamed);
        let expect = if renamed_project { &new } else { &old };
        assert_eq!(*l.doc.project, *expect.project, "crash {crash_at}");

        // A fresh save of the new document completes and leaves exactly
        // the referenced blobs.
        save(&t.bundle(), &new).unwrap();
        assert_eq!(
            blob_files(&t.bundle()),
            referenced(&new),
            "crash {crash_at}"
        );
        let l = load(&t.bundle()).unwrap();
        assert_eq!(*l.doc.project, *new.project);
    }
}

#[test]
fn crash_during_first_save_never_leaves_a_half_project() {
    let (d, _, _) = doc_with_plugins(1, 1, 7);
    let probe = TempDir::new();
    let steps = steps_of(&probe.bundle(), &d);
    for crash_at in 0..steps.len() {
        let t = TempDir::new();
        let mut n = 0usize;
        let _ = save_with_hook(&t.bundle(), &d, &mut |_| {
            n += 1;
            n - 1 != crash_at
        });
        let renamed = steps[..=crash_at].contains(&SaveStep::ProjectRenamed);
        match load(&t.bundle()) {
            Ok(l) => {
                assert!(renamed, "crash {crash_at}: project.toml appeared early");
                assert_eq!(*l.doc.project, *d.project);
            }
            Err(_) => assert!(!renamed, "crash {crash_at}"),
        }
        save(&t.bundle(), &d).unwrap();
        assert_eq!(*load(&t.bundle()).unwrap().doc.project, *d.project);
    }
}

#[test]
fn random_documents_survive_save_and_load() {
    for seed in 1..=15u64 {
        let mut r = Rng(0xDEAD_BEEF_1234_5678 ^ seed.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        let mut d = Document::new();
        for _ in 0..250 {
            let e = random_edit(&mut r, &d);
            if let Ok((nd, _)) = apply(&d, &e) {
                d = nd;
            }
            // Capture state for CLAP refs now and then.
            if r.chance(10) {
                let mut insts = Vec::new();
                for_each_clap(&d.project, |c| insts.push(c.instance));
                if let Some(i) = r.pick(&insts) {
                    let gen_no = r.below(1000) as u32 + 1;
                    d = commit_plugin_state(
                        &d,
                        i,
                        &state_file_name(i, gen_no),
                        blob(r.below(255) as u8, r.below(64) as usize),
                        None,
                    )
                    .unwrap();
                }
            }
        }
        let t = TempDir::new();
        save(&t.bundle(), &d).unwrap();
        let l = load(&t.bundle()).unwrap();
        // A ref with a state file name but no bytes (an edit recorded the
        // name only) is saved without the name, as `save` documents.
        let mut expect = (*d.project).clone();
        for_each_clap_mut(&mut expect, |r| {
            if r.state_bytes.is_none() {
                r.state_file = None;
            }
        });
        assert_eq!(*l.doc.project, expect, "seed {seed}");
        assert!(l.doc.next_id >= d.next_id.min(l.doc.project.max_id() + 1));
        assert_eq!(blob_files(&t.bundle()), referenced(&l.doc));
        // Edit after reopen: ids stay unique (17.1 property).
        let mut seen: HashSet<u32> = HashSet::new();
        let floor = l.doc.next_id;
        let mut doc = l.doc;
        for _ in 0..80 {
            let e = random_edit(&mut r, &doc);
            if let Ok((nd, created)) = apply(&doc, &e) {
                for id in created {
                    assert!(id >= floor);
                    assert!(seen.insert(id));
                }
                doc = nd;
            }
        }
    }
}

#[test]
fn autosave_write_detect_load_clear() {
    let t = TempDir::new();
    let (d, _, _) = doc_with_plugins(1, 1, 7);
    save(&t.bundle(), &d).unwrap();
    assert!(!autosave_is_newer(&t.bundle()));
    let d2 = apply(&d, &Edit::SetTempo { bpm: 77.0 }).unwrap().0;
    // Make sure the autosave mtime is later than project.toml's.
    std::thread::sleep(Duration::from_millis(30));
    save_autosave(&t.bundle(), &d2).unwrap();
    assert!(autosave_is_newer(&t.bundle()));
    let l = load_autosave(&t.bundle()).unwrap();
    assert_eq!(l.doc.project.tempo_bpm, 77.0);
    assert!(l.missing_blobs.is_empty());
    // The main project is untouched by the autosave.
    assert_eq!(load(&t.bundle()).unwrap().doc.project.tempo_bpm, 120.0);
    // An explicit save clears the autosave.
    save_and_clear_autosave(&t.bundle(), &d2).unwrap();
    assert!(!autosave_path(&t.bundle()).exists());
    assert!(!autosave_is_newer(&t.bundle()));
    clear_autosave(&t.bundle()).unwrap();
}

#[test]
fn autosave_for_a_never_saved_bundle_counts_as_newer() {
    let t = TempDir::new();
    let (d, _, _) = doc_with_plugins(1, 1, 7);
    save_autosave(&t.bundle(), &d).unwrap();
    assert!(autosave_is_newer(&t.bundle()));
    assert!(load(&t.bundle()).is_err());
    assert!(load_autosave(&t.bundle()).is_ok());
}

#[test]
fn autosave_crash_leaves_previous_autosave_intact() {
    let t = TempDir::new();
    let (d, a, _) = doc_with_plugins(1, 1, 7);
    save_autosave(&t.bundle(), &d).unwrap();
    let d2 = commit_plugin_state(&d, a, &state_file_name(a, 2), blob(1, 5), None).unwrap();
    let mut n = 0;
    let _ = save_with_hook(&autosave_path(&t.bundle()), &d2, &mut |_| {
        n += 1;
        n < 2
    });
    let l = load_autosave(&t.bundle()).unwrap();
    assert!(l.missing_blobs.is_empty());
}

fn secs(t0: Instant, s: f64) -> Instant {
    t0 + Duration::from_secs_f64(s)
}

#[test]
fn autosave_waits_for_three_quiet_seconds() {
    let t0 = Instant::now();
    let mut d = AutosaveDebounce::standard();
    assert!(!d.due(secs(t0, 100.0)), "nothing changed, nothing to write");
    d.changed(t0);
    assert!(d.pending());
    assert!(!d.due(secs(t0, 2.9)));
    assert!(d.due(secs(t0, 3.0)));
    assert!(!d.pending());
    assert!(!d.due(secs(t0, 3.1)), "fires once per batch of changes");
}

#[test]
fn each_edit_restarts_the_quiet_period() {
    let t0 = Instant::now();
    let mut d = AutosaveDebounce::standard();
    d.changed(t0);
    d.changed(secs(t0, 2.0));
    assert!(!d.due(secs(t0, 4.9)), "3 s after the *last* edit");
    assert!(d.due(secs(t0, 5.0)));
}

#[test]
fn continuous_edits_still_autosave_every_fifteen_seconds() {
    let t0 = Instant::now();
    let mut d = AutosaveDebounce::standard();
    let mut fired = Vec::new();
    // An edit every second for 40 seconds.
    for i in 0..=40 {
        let now = secs(t0, i as f64);
        d.changed(now);
        if d.due(now) {
            fired.push(i);
        }
    }
    assert_eq!(fired, vec![15, 31], "at most 15 s of work is ever unsaved");
}

#[test]
fn clearing_forgets_pending_changes() {
    let t0 = Instant::now();
    let mut d = AutosaveDebounce::standard();
    d.changed(t0);
    d.clear();
    assert!(!d.due(secs(t0, 60.0)));
    assert!(!d.pending());
}

#[test]
fn plugin_state_is_captured_every_sixty_seconds_when_a_plugin_reported_changes() {
    let t0 = Instant::now();
    let mut c = CaptureClock::standard(t0);
    assert!(!c.due(secs(t0, 61.0)), "no change reported");
    c.plugin_dirty();
    assert!(!c.due(secs(t0, 30.0)), "not before the period is over");
    assert!(c.due(secs(t0, 61.0)));
    assert!(!c.due(secs(t0, 62.0)), "flag was consumed");
    c.plugin_dirty();
    assert!(!c.due(secs(t0, 100.0)), "next period starts at the capture");
    assert!(c.due(secs(t0, 121.0)));
}

#[test]
fn plugin_state_is_captured_right_after_a_gesture() {
    let t0 = Instant::now();
    let mut c = CaptureClock::standard(t0);
    c.after_gesture(secs(t0, 5.0));
    assert!(c.due(secs(t0, 5.0)));
    assert!(!c.due(secs(t0, 6.0)));
}

#[test]
fn autosave_worker_writes_and_coalesces() {
    let t = TempDir::new();
    let w = AutosaveWorker::spawn();
    let (d, _, _) = doc_with_plugins(1, 1, 7);
    let mut doc = d;
    for i in 0..20 {
        doc = apply(
            &doc,
            &Edit::SetTempo {
                bpm: 100.0 + i as f64,
            },
        )
        .unwrap()
        .0;
        w.submit(autosave_path(&t.bundle()), doc.clone());
    }
    let last_rev = doc.revision;
    let results = w.results.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(results.result.is_ok());
    // Wait until the newest revision was written.
    let mut rev = results.revision;
    while rev < last_rev {
        rev = w
            .results
            .recv_timeout(Duration::from_secs(10))
            .unwrap()
            .revision;
    }
    w.shutdown();
    let l = load_autosave(&t.bundle()).unwrap();
    assert_eq!(l.doc.project.tempo_bpm, 119.0);
}

#[test]
fn autosave_worker_reports_errors() {
    let t = TempDir::new();
    // A file where the bundle directory should be.
    fs::write(t.bundle(), b"in the way").unwrap();
    let w = AutosaveWorker::spawn();
    w.submit(autosave_path(&t.bundle()), Document::new());
    let r = w.results.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(r.result.is_err());
}
