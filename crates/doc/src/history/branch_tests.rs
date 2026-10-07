// SPDX-License-Identifier: GPL-3.0-or-later
//! Branches, versions, and the persisted tree (SPEC 15.11, 15.12).

use super::*;
use crate::starter::starter_project;
use crate::store::{HistoryStore, Persister};
use protocol::ids::ChannelId;

fn tempo(b: f64) -> Edit {
    Edit::SetTempo { bpm: b }
}

fn user() -> Author {
    Author::User
}

fn agent(s: &str) -> Author {
    Author::Agent(s.into())
}

fn put(e: &mut Editor, a: Author, edits: Vec<Edit>) -> Applied {
    match e.submit(a, None, edits, 0).expect("edit") {
        Submitted::Applied(x) => x,
        Submitted::Queued => panic!("queued"),
    }
}

fn bpm(e: &Editor) -> f64 {
    e.document().project.tempo_bpm
}

fn editor() -> Editor {
    Editor::new(Document::new(), true)
}

#[test]
fn undo_and_redo_stay_inside_the_branch() {
    let mut e = editor();
    put(&mut e, user(), vec![tempo(100.0)]);
    put(&mut e, user(), vec![tempo(110.0)]);
    let a = e
        .create_branch(&agent("s1"), "Version A: faster", None)
        .unwrap();
    assert_eq!(a, "version-a-faster");
    assert_eq!(e.current_branch(), a);
    // Nothing on the branch yet: undo does not walk back into main.
    assert_eq!(e.undo(&Scope::Any), Err(HistoryError::NothingToUndo));
    put(&mut e, agent("s1"), vec![tempo(140.0)]);
    put(&mut e, agent("s1"), vec![tempo(150.0)]);
    e.undo(&Scope::Any).unwrap();
    assert_eq!(bpm(&e), 140.0);
    e.undo(&Scope::Any).unwrap();
    assert_eq!(bpm(&e), 110.0, "the branch's base state");
    assert_eq!(e.undo(&Scope::Any), Err(HistoryError::NothingToUndo));
    e.redo(&Scope::Any).unwrap();
    e.redo(&Scope::Any).unwrap();
    assert_eq!(bpm(&e), 150.0);
    assert_eq!(e.redo(&Scope::Any), Err(HistoryError::NothingToRedo));

    // Main is untouched, and has its own undo.
    e.switch_branch("main").unwrap();
    assert_eq!(bpm(&e), 110.0);
    assert_eq!(e.redo(&Scope::Any), Err(HistoryError::NothingToRedo));
    e.undo(&Scope::Any).unwrap();
    assert_eq!(bpm(&e), 100.0);
    // Back to A by name: its head is current.
    e.switch_branch("Version A: faster").unwrap();
    assert_eq!(bpm(&e), 150.0);
    e.history().check_integrity().unwrap();
}

#[test]
fn switching_and_creating_are_undoable_steps() {
    let mut e = editor();
    put(&mut e, user(), vec![tempo(100.0)]);
    let first = e.head_name();
    put(&mut e, user(), vec![tempo(110.0)]);
    e.create_branch(&agent("s"), "B", Some(&first)).unwrap();
    assert_eq!(bpm(&e), 100.0, "created at an older commit");
    put(&mut e, agent("s"), vec![tempo(90.0)]);
    e.switch_branch("main").unwrap();
    assert_eq!(bpm(&e), 110.0);
    assert!(e.can_undo_switch());
    assert!(e.undo_switch());
    assert_eq!((e.current_branch(), bpm(&e)), ("b", 90.0));
    assert!(e.undo_switch());
    assert_eq!((e.current_branch(), bpm(&e)), ("main", 110.0));
    assert!(!e.undo_switch());
    // A gesture blocks switching.
    assert!(e.begin_gesture(user(), "drag"));
    assert_eq!(e.switch_branch("b"), Err(BranchError::GestureOpen));
    e.end_gesture();
    assert_eq!(
        e.switch_branch("nope"),
        Err(BranchError::UnknownBranch("nope".into()))
    );
    assert_eq!(
        e.create_branch(&user(), "", None),
        Err(BranchError::BadName)
    );
    assert!(matches!(
        e.create_branch(&user(), "x", Some("zzz")),
        Err(BranchError::UnknownCommit(_))
    ));
}

#[test]
fn ids_are_never_reused_across_branches() {
    let mut e = Editor::new(starter_project(), true);
    let kick = e.document().project.channels[0].id;
    let clip_edit = || Edit::AddClip {
        instrument: kick,
        pattern: None,
        start: 4 * 3840,
        len: 3840,
    };
    e.create_branch(&agent("s"), "A", None).unwrap();
    let a = put(&mut e, agent("s"), vec![clip_edit()]).created;
    e.switch_branch("main").unwrap();
    let m = put(&mut e, user(), vec![clip_edit()]).created;
    assert!(a.iter().all(|x| !m.contains(x)), "{a:?} {m:?}");
    e.switch_branch("a").unwrap();
    let later = Edit::AddClip {
        instrument: kick,
        pattern: None,
        start: 9 * 3840,
        len: 3840,
    };
    let a2 = put(
        &mut e,
        agent("s"),
        vec![later, Edit::SetTempo { bpm: 99.0 }],
    );
    assert!(a2.created.iter().all(|x| !a.contains(x) && !m.contains(x)));
}

#[test]
fn archive_rename_and_bring_back() {
    let mut e = editor();
    e.create_branch(&agent("s"), "Draft", None).unwrap();
    assert_eq!(e.archive_branch("draft"), Err(BranchError::IsCurrent));
    e.switch_branch("main").unwrap();
    e.archive_branch("draft").unwrap();
    let infos = e.branch_infos();
    assert!(infos.iter().find(|b| b.branch == "draft").unwrap().archived);
    // Commits are kept; switching brings it back.
    e.rename_branch("draft", "Better name").unwrap();
    e.switch_branch("draft").unwrap();
    let infos = e.branch_infos();
    let b = infos.iter().find(|b| b.branch == "draft").unwrap();
    assert!(!b.archived);
    assert_eq!(b.name, "Better name");
    assert_eq!(b.author, "agent:s");
    // Names are made unique as ids.
    let id2 = e.create_branch(&user(), "Draft", None).unwrap();
    assert_eq!(id2, "draft-2");
}

#[test]
fn author_scoping_is_per_branch() {
    let mut e = editor();
    put(&mut e, user(), vec![tempo(100.0)]);
    e.create_branch(&agent("s1"), "A", None).unwrap();
    put(&mut e, agent("s1"), vec![tempo(120.0)]);
    let me = Scope::Only(agent("s1"));
    e.undo(&me).unwrap();
    assert_eq!(bpm(&e), 100.0);
    // The base belongs to the user's main: an agent cannot walk into it.
    assert_eq!(e.undo(&me), Err(HistoryError::NothingToUndo));
    e.switch_branch("main").unwrap();
    assert_eq!(e.undo(&me), Err(HistoryError::NotYours));
    assert!(e.undo(&Scope::Any).is_ok());
}

#[test]
fn versions_name_commits_and_restore_adds_a_commit_on_top() {
    let mut e = editor();
    put(&mut e, user(), vec![tempo(100.0)]);
    let slug = e.save_version("Darker 808").unwrap();
    assert_eq!(slug, "darker-808");
    let named = e.head_name();
    put(&mut e, user(), vec![tempo(150.0)]);
    let len = e.history().len();
    e.restore_version(user(), &named).unwrap();
    assert_eq!(bpm(&e), 100.0);
    assert_eq!(e.history().len(), len + 1, "nothing was thrown away");
    e.undo(&Scope::Any).unwrap();
    assert_eq!(bpm(&e), 150.0);
    let nodes = e.history_nodes(None, 0);
    assert!(
        nodes
            .iter()
            .any(|n| n.name.as_deref() == Some("Darker 808"))
    );
    assert!(
        nodes
            .iter()
            .any(|n| n.description.starts_with("Restore Darker 808"))
    );
    assert_eq!(
        e.restore_version(user(), "unknown").unwrap_err(),
        BranchError::UnknownCommit("unknown".into())
    );
    // Newest first, limit and since work.
    assert_eq!(e.history_nodes(None, 2).len(), 2);
    let nodes = e.history_nodes(None, 0);
    let oldest = nodes.last().unwrap().commit.clone();
    assert_eq!(e.history_nodes(Some(&oldest), 0).len(), nodes.len() - 1);
}

#[test]
fn memory_limit_never_drops_branch_heads_or_versions() {
    let limits = Limits {
        max_entries: 5,
        max_bytes: usize::MAX,
    };
    let mut e = Editor::with_limits(Document::new(), true, limits);
    put(&mut e, user(), vec![tempo(100.0)]);
    e.create_branch(&agent("s"), "keep-me", None).unwrap();
    put(&mut e, agent("s"), vec![tempo(101.0)]);
    let head = e.head_name();
    e.save_version("v1").unwrap();
    e.switch_branch("main").unwrap();
    for i in 0..30 {
        put(&mut e, user(), vec![tempo(110.0 + i as f64)]);
    }
    assert!(
        e.history().resolve(&head).is_some(),
        "the branch head stays"
    );
    e.switch_branch("keep-me").unwrap();
    assert_eq!(bpm(&e), 101.0);
    e.history().check_integrity().unwrap();
}

// ----- persistence --------------------------------------------------------

fn flush(e: &mut Editor, p: &mut Persister) {
    if let Some(b) = e.take_pending_history() {
        let written = p.write(&b).unwrap();
        e.history_persisted(&written);
    }
}

fn temp_bundle() -> crate::store::tests::TempDir {
    crate::store::tests::TempDir::new()
}

#[test]
fn the_tree_survives_a_restart_with_branches_versions_and_ids() {
    let t = temp_bundle();
    let mut e = Editor::new(starter_project(), true);
    e.enable_persistence(Some(&t.0));
    let store = HistoryStore::open(&t.0).unwrap();
    let mut p = Persister::new(store, e.known_hashes());
    let kick = e.document().project.channels[0].id;
    put(&mut e, user(), vec![tempo(100.0)]);
    flush(&mut e, &mut p);
    e.save_version("Base").unwrap();
    e.create_branch(&agent("s1"), "Faster", None).unwrap();
    let pat0 = e.document().project.patterns[0].id;
    put(
        &mut e,
        agent("s1"),
        vec![
            tempo(160.0),
            Edit::SetStep {
                pattern: pat0,
                step: 3,
                on: true,
                vel: None,
            },
        ],
    );
    flush(&mut e, &mut p);
    e.switch_branch("main").unwrap();
    put(
        &mut e,
        user(),
        vec![Edit::RenameChannel {
            channel: kick,
            name: "BD".into(),
        }],
    );
    let next_id = e.document().next_id;
    flush(&mut e, &mut p);
    let main_head = e.head_name();
    e.switch_branch("faster").unwrap();
    flush(&mut e, &mut p);
    assert!(!e.has_pending_history());
    // Every entry now has a content hash for a name.
    assert!(
        e.history_nodes(None, 0)
            .iter()
            .all(|n| n.commit.len() == 64)
    );
    let want_nodes = e.history_nodes(None, 0);
    let want_doc = e.document().project.clone();
    let want_branches = e.branch_infos();
    drop(p);

    let mut store = HistoryStore::open(&t.0).unwrap();
    let mut r = Editor::restore(&mut store, Limits::default(), Some(&want_doc), Some(&t.0))
        .unwrap()
        .expect("history");
    assert_eq!(r.document().project, want_doc);
    assert!(!r.is_dirty(), "HEAD equals the saved project");
    assert_eq!(r.current_branch(), "faster");
    assert!(r.document().next_id >= next_id, "ids are never reused");
    assert_eq!(r.branch_infos(), want_branches);
    let got: Vec<_> = r
        .history_nodes(None, 0)
        .into_iter()
        .map(|n| (n.commit, n.parent, n.branch, n.author, n.name))
        .collect();
    let want: Vec<_> = want_nodes
        .into_iter()
        .map(|n| (n.commit, n.parent, n.branch, n.author, n.name))
        .collect();
    assert_eq!(got, want);
    // Undo works on the restored tree, and main is intact.
    r.undo(&Scope::Any).unwrap();
    assert_eq!(bpm(&r), 100.0);
    r.switch_branch("main").unwrap();
    assert_eq!(r.head_name(), main_head);
    assert_eq!(r.document().project.channels[0].name, "BD");
    r.history().check_integrity().unwrap();

    // New edits after a restore continue the same tree and persist again.
    r.enable_persistence(Some(&t.0));
    let mut p = Persister::new(store, r.known_hashes());
    put(&mut r, user(), vec![tempo(77.0)]);
    flush(&mut r, &mut p);
    let s2 = HistoryStore::open(&t.0).unwrap();
    assert!(s2.commit_count() >= 5);
    assert_eq!(s2.head().map(|h| h.0), Some("main"));
}

#[test]
fn save_garbage_collection_keeps_what_any_commit_references() {
    use crate::bundle::{Keep, save_keeping};
    use crate::samples::{SAMPLES_DIR, sample_file_name};
    let t = temp_bundle();
    let bundle = t.0.join("Song.ldaw");
    let hash = "cd".repeat(32);
    let file = bundle.join(SAMPLES_DIR).join(sample_file_name(&hash));
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, b"RIFF....WAVE").unwrap();

    let mut e = Editor::new(Document::new(), true);
    e.enable_persistence(Some(&bundle));
    let mut p = Persister::new(HistoryStore::open(&bundle).unwrap(), e.known_hashes());
    put(
        &mut e,
        user(),
        vec![Edit::AddSample {
            sample: protocol::model::SampleRef {
                hash: hash.clone(),
                orig_name: "k.wav".into(),
                size: 12,
                local_only: false,
            },
        }],
    );
    flush(&mut e, &mut p);
    put(
        &mut e,
        user(),
        vec![Edit::RemoveSample { hash: hash.clone() }],
    );
    flush(&mut e, &mut p);

    // Without the store's keep set the sample is garbage...
    let mut plain = Keep::default();
    plain.merge(&Keep::default());
    let lone = Document::new();
    let r = save_keeping(&bundle, &lone, &plain).unwrap();
    assert_eq!(r.samples_deleted, vec![sample_file_name(&hash)]);

    // ...with it (and with nothing of the history in memory) it stays.
    std::fs::write(&file, b"RIFF....WAVE").unwrap();
    let reopened = HistoryStore::open(&bundle).unwrap();
    let mut keep = Keep::default();
    keep.merge(reopened.keep());
    let r = save_keeping(&bundle, &lone, &keep).unwrap();
    assert!(r.samples_deleted.is_empty(), "{r:?}");
    assert!(file.exists());
    // After compaction that drops the old commits, the next save may
    // collect it.
    let mut s = HistoryStore::open(&bundle).unwrap();
    s.compact(u64::MAX).unwrap();
    assert!(!s.keep().samples.contains(&hash));
}

#[test]
fn an_open_gesture_is_written_once_when_it_ends() {
    let t = temp_bundle();
    let mut e = Editor::new(starter_project(), true);
    e.enable_persistence(Some(&t.0));
    let store = HistoryStore::open(&t.0).unwrap();
    let mut p = Persister::new(store, e.known_hashes());
    flush(&mut e, &mut p);
    let base = p.store.commit_count();
    assert!(e.begin_gesture(user(), "drag tempo"));
    for b in [101.0, 102.0, 103.0] {
        e.gesture_edit(&[tempo(b)]).unwrap();
        flush(&mut e, &mut p);
    }
    assert_eq!(p.store.commit_count(), base, "the gesture entry waits");
    e.end_gesture();
    flush(&mut e, &mut p);
    assert_eq!(p.store.commit_count(), base + 1);
    let s = HistoryStore::open(&t.0).unwrap();
    let nodes = s.nodes(None, 1);
    assert_eq!(nodes[0].description, "drag tempo");
}

#[test]
fn restore_after_damage_keeps_what_is_intact() {
    let t = temp_bundle();
    let mut e = Editor::new(Document::new(), true);
    e.enable_persistence(None);
    let store = HistoryStore::open(&t.0).unwrap();
    let mut p = Persister::new(store, e.known_hashes());
    put(&mut e, user(), vec![tempo(100.0)]);
    put(&mut e, user(), vec![tempo(110.0)]);
    flush(&mut e, &mut p);
    // Lose HEAD and every branch file, as if they were never written.
    let dir = t.0.join(crate::store::HISTORY_DIR);
    std::fs::remove_file(dir.join("HEAD")).unwrap();
    std::fs::remove_dir_all(dir.join("branches")).unwrap();
    let mut store = HistoryStore::open(&t.0).unwrap();
    let r = Editor::restore(&mut store, Limits::default(), None, None)
        .unwrap()
        .unwrap();
    assert_eq!(bpm(&r), 110.0, "the newest commit is current");
    assert_eq!(r.current_branch(), "main");
    assert_eq!(r.branch_infos().len(), 1);
    assert!(
        Editor::restore(
            &mut HistoryStore::new(&t.0.join("empty")),
            Limits::default(),
            None,
            None
        )
        .unwrap()
        .is_none()
    );
}

#[test]
fn history_diff_reads_like_the_screen() {
    let mut e = Editor::new(starter_project(), true);
    let before = e.head_name();
    let pat = e.document().project.patterns[0].id;
    let kick = e.document().project.channels[0].id;
    put(
        &mut e,
        user(),
        vec![
            tempo(140.0),
            Edit::SetStep {
                pattern: pat,
                step: 3,
                on: true,
                vel: None,
            },
            Edit::SetStep {
                pattern: pat,
                step: 5,
                on: true,
                vel: None,
            },
            Edit::SetStep {
                pattern: pat,
                step: 7,
                on: true,
                vel: None,
            },
            Edit::SetChannelMix {
                channel: kick,
                value: protocol::edit::MixValue::VolumeDb(-6.0),
            },
            Edit::AddClip {
                instrument: kick,
                pattern: None,
                start: 5 * 3840,
                len: 3840,
            },
        ],
    );
    let after = e.head_name();
    let lines = e.diff(&before, &after).unwrap();
    assert!(lines.contains(&"tempo 120 -> 140".to_string()), "{lines:?}");
    assert!(
        lines.contains(&"Kick: 3 steps added in clip Kick 1".to_string()),
        "{lines:?}"
    );
    assert!(
        lines.contains(&"Kick: volume 0 -> -6 dB".to_string()),
        "{lines:?}"
    );
    assert!(
        lines.contains(&"Kick: clip added at bar 6".to_string()),
        "{lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("Kick: new clip content Kick 2"))
    );
    // Reading it the other way round reverses it, and equal projects are empty.
    let back = e.diff(&after, &before).unwrap();
    assert!(back.contains(&"tempo 140 -> 120".to_string()));
    assert!(
        back.contains(&"Kick: 3 steps removed in clip Kick 1".to_string()),
        "{back:?}"
    );
    assert!(e.diff(&after, &after).unwrap().is_empty());
    assert!(matches!(
        e.diff("nope", &after),
        Err(BranchError::UnknownCommit(_))
    ));
}

#[test]
fn history_diff_summarizes_many_clip_changes() {
    let mut e = Editor::new(starter_project(), true);
    let before = e.head_name();
    let clips: Vec<_> = e.document().project.clips.iter().map(|c| c.id).collect();
    put(
        &mut e,
        user(),
        vec![Edit::MoveClips {
            clips,
            dt: 4 * 3840,
        }],
    );
    let lines = e.diff(&before, &e.head_name()).unwrap();
    assert_eq!(lines, ["16 clips moved"]);
    let _ = ChannelId(0);
}
