// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;
use crate::document::commit_plugin_state;
use crate::document::tests::{Rng, random_edit};
use protocol::edit::{MixValue, NewInstrument, NewNote};
use protocol::ids::{ChannelId, InstanceId, TrackId};
use protocol::model::SynthParams;

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

fn editor() -> Editor {
    Editor::new(Document::new(), true)
}

fn bpm(e: &Editor) -> f64 {
    e.document().project.tempo_bpm
}

fn add_clap() -> Edit {
    Edit::AddChannel {
        name: "p".into(),
        instrument: NewInstrument::Clap {
            plugin_id: "a.b".into(),
            preset: None,
        },
        root_key: 60,
        track: TrackId::MASTER,
    }
}

#[test]
fn undo_redo_walk() {
    let mut e = editor();
    for b in [100.0, 110.0, 120.5] {
        put(&mut e, user(), vec![tempo(b)]);
    }
    assert_eq!(bpm(&e), 120.5);
    e.undo(&Scope::Any).unwrap();
    assert_eq!(bpm(&e), 110.0);
    e.undo(&Scope::Any).unwrap();
    e.undo(&Scope::Any).unwrap();
    assert_eq!(bpm(&e), 120.0);
    assert_eq!(e.undo(&Scope::Any), Err(HistoryError::NothingToUndo));
    e.redo(&Scope::Any).unwrap();
    e.redo(&Scope::Any).unwrap();
    e.redo(&Scope::Any).unwrap();
    assert_eq!(bpm(&e), 120.5);
    assert_eq!(e.redo(&Scope::Any), Err(HistoryError::NothingToRedo));
}

#[test]
fn edit_after_undo_makes_a_branch_and_keeps_the_old_one() {
    let mut e = editor();
    put(&mut e, user(), vec![tempo(100.0)]);
    put(&mut e, user(), vec![tempo(110.0)]);
    e.undo(&Scope::Any).unwrap();
    put(&mut e, user(), vec![tempo(130.0)]);
    // 3 edits + root, nothing discarded.
    assert_eq!(e.history().len(), 4);
    e.history().check_integrity().unwrap();
    assert_eq!(bpm(&e), 130.0);
    e.undo(&Scope::Any).unwrap();
    // Redo goes to the branch most recently used, which is the new one.
    e.redo(&Scope::Any).unwrap();
    assert_eq!(bpm(&e), 130.0);
    let infos = e.history().infos();
    assert_eq!(infos.len(), 4);
    assert_eq!(infos.iter().filter(|i| i.current).count(), 1);
}

#[test]
fn undo_keeps_next_id_and_ids_are_not_reused() {
    let mut e = editor();
    let a = put(&mut e, user(), vec![Edit::AddTrack { name: "t".into() }]);
    let after_add = e.document().next_id;
    e.undo(&Scope::Any).unwrap();
    assert_eq!(e.document().next_id, after_add);
    let b = put(&mut e, user(), vec![Edit::AddTrack { name: "u".into() }]);
    assert_ne!(a.created, b.created);
    assert!(b.created[0] > a.created[0]);
}

#[test]
fn revision_grows_on_undo_and_redo() {
    let mut e = editor();
    put(&mut e, user(), vec![tempo(100.0)]);
    let r1 = e.document().revision;
    e.undo(&Scope::Any).unwrap();
    let r2 = e.document().revision;
    e.redo(&Scope::Any).unwrap();
    let r3 = e.document().revision;
    assert!(r1 < r2 && r2 < r3);
}

#[test]
fn batch_is_one_undo_step() {
    let mut e = editor();
    put(
        &mut e,
        Author::Script,
        vec![
            Edit::AddTrack { name: "a".into() },
            Edit::AddTrack { name: "b".into() },
            tempo(99.0),
        ],
    );
    assert_eq!(e.history().len(), 2);
    e.undo(&Scope::Any).unwrap();
    assert_eq!(e.document().project.tracks.len(), 1);
    assert_eq!(bpm(&e), 120.0);
}

#[test]
fn failed_batch_leaves_no_entry() {
    let mut e = editor();
    let r = e.submit(user(), None, vec![tempo(100.0), tempo(-1.0)], 0);
    assert!(r.is_err());
    assert_eq!(e.history().len(), 1);
    assert_eq!(bpm(&e), 120.0);
    assert!(!e.is_dirty());
}

#[test]
fn empty_batch_is_a_no_op() {
    let mut e = editor();
    put(&mut e, user(), vec![]);
    assert_eq!(e.history().len(), 1);
}

#[test]
fn gesture_collapses_into_one_entry() {
    let mut e = editor();
    assert!(e.begin_gesture(user(), "fader"));
    assert!(!e.begin_gesture(user(), "again"));
    for i in 0..50 {
        e.gesture_edit(&[Edit::SetTrackMix {
            track: TrackId::MASTER,
            value: MixValue::VolumeDb(-(i as f64)),
        }])
        .unwrap();
    }
    assert_eq!(e.history().len(), 2, "one entry for the whole drag");
    assert!(e.end_gesture().is_empty());
    assert_eq!(e.document().project.tracks[0].mix.volume_db, -49.0);
    e.undo(&Scope::Any).unwrap();
    assert_eq!(e.document().project.tracks[0].mix.volume_db, 0.0);
    e.redo(&Scope::Any).unwrap();
    assert_eq!(e.document().project.tracks[0].mix.volume_db, -49.0);
}

#[test]
fn gesture_without_edits_leaves_no_entry() {
    let mut e = editor();
    e.begin_gesture(user(), "x");
    e.end_gesture();
    assert_eq!(e.history().len(), 1);
    assert!(!e.is_dirty());
}

#[test]
fn undo_redo_disabled_during_gesture() {
    let mut e = editor();
    put(&mut e, user(), vec![tempo(100.0)]);
    e.undo(&Scope::Any).unwrap();
    e.begin_gesture(user(), "g");
    assert!(!e.can_undo(&Scope::Any));
    assert!(!e.can_redo(&Scope::Any));
    assert_eq!(e.undo(&Scope::Any), Err(HistoryError::GestureOpen));
    assert_eq!(e.redo(&Scope::Any), Err(HistoryError::GestureOpen));
    e.end_gesture();
    assert!(e.can_redo(&Scope::Any));
}

#[test]
fn script_and_agent_batches_queue_fifo_while_gesture_is_open() {
    let mut e = editor();
    e.begin_gesture(user(), "drag");
    e.gesture_edit(&[tempo(101.0)]).unwrap();
    let q1 = e
        .submit(
            Author::Script,
            None,
            vec![Edit::AddTrack { name: "s".into() }],
            1,
        )
        .unwrap();
    // Invalid tempo: fails when it runs, not when queued.
    let q2 = e.submit(agent("a1"), None, vec![tempo(5555.0)], 2).unwrap();
    let q3 = e.submit(agent("a1"), None, vec![tempo(140.0)], 3).unwrap();
    assert!(matches!(q1, Submitted::Queued));
    assert!(matches!(q2, Submitted::Queued));
    assert!(matches!(q3, Submitted::Queued));
    assert_eq!(e.queue_len(), 3);
    assert_eq!(e.document().project.tracks.len(), 1);
    assert_eq!(bpm(&e), 101.0);
    let done = e.end_gesture();
    assert_eq!(
        done.iter().map(|d| d.token).collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    assert!(done[0].result.is_ok());
    assert!(done[1].result.is_err(), "queued batch that fails reports");
    assert!(done[2].result.is_ok());
    assert_eq!(bpm(&e), 140.0);
    assert_eq!(e.document().project.tracks.len(), 2);
    // Gesture entry + 2 separate groups + root.
    assert_eq!(e.history().len(), 4);
    assert_eq!(e.queue_len(), 0);
}

#[test]
fn queued_batch_can_be_cancelled_for_the_busy_timeout() {
    let mut e = editor();
    e.begin_gesture(user(), "drag");
    e.submit(Author::Script, None, vec![tempo(100.0)], 7)
        .unwrap();
    assert!(e.cancel_queued(7));
    assert!(!e.cancel_queued(7));
    assert!(e.end_gesture().is_empty());
    assert_eq!(bpm(&e), 120.0);
}

#[test]
fn author_tags_and_scoped_undo() {
    let mut e = editor();
    put(&mut e, user(), vec![tempo(100.0)]);
    put(&mut e, agent("s1"), vec![tempo(110.0)]);
    put(&mut e, agent("s2"), vec![tempo(120.5)]);
    let tags: Vec<_> = e.history().infos().into_iter().map(|i| i.author).collect();
    assert_eq!(tags, vec!["user", "user", "agent:s1", "agent:s2"]);

    let s1 = Scope::Only(agent("s1"));
    assert_eq!(e.undo(&s1), Err(HistoryError::NotYours));
    assert!(!e.can_undo(&s1));
    let s2 = Scope::Only(agent("s2"));
    e.undo(&s2).unwrap();
    assert_eq!(bpm(&e), 110.0);
    e.undo(&s1).unwrap();
    assert_eq!(bpm(&e), 100.0);
    // The top is the user's now: no agent may undo it.
    assert_eq!(e.undo(&s1), Err(HistoryError::NotYours));
    assert_eq!(e.redo(&s2), Err(HistoryError::NotYours));
    e.redo(&s1).unwrap();
    e.redo(&s2).unwrap();
    assert_eq!(bpm(&e), 120.5);
    // The user can undo an agent's work.
    e.undo(&Scope::Any).unwrap();
    e.undo(&Scope::Any).unwrap();
    assert_eq!(bpm(&e), 100.0);
}

#[test]
fn author_tag_round_trip() {
    for a in [user(), Author::Script, agent("abc")] {
        assert_eq!(Author::from_tag(&a.tag()), Some(a));
    }
    assert_eq!(Author::from_tag("agent:"), None);
    assert_eq!(Author::from_tag("robot"), None);
}

#[test]
fn dirty_tracking() {
    let mut e = editor();
    assert!(!e.is_dirty());
    put(&mut e, user(), vec![tempo(100.0)]);
    assert!(e.is_dirty());
    e.mark_saved();
    assert!(!e.is_dirty());
    e.undo(&Scope::Any).unwrap();
    assert!(e.is_dirty());
    e.redo(&Scope::Any).unwrap();
    assert!(!e.is_dirty(), "back at the saved snapshot");
    e.merge(&[tempo(100.0)]).unwrap();
    assert!(e.is_dirty(), "merged change is unsaved");
    e.mark_saved();
    assert!(!e.is_dirty());
    let new = Editor::new(Document::new(), false);
    assert!(new.is_dirty());
}

#[test]
fn merge_is_not_an_undo_step_and_survives_undo_redo() {
    let mut e = editor();
    put(&mut e, user(), vec![add_clap()]);
    let n = e.history().len();
    let inst = InstanceId(e.document().project.max_id());
    let bytes: Arc<[u8]> = Arc::from(vec![9u8; 10]);
    e.merge_with(|d| commit_plugin_state(d, inst, "2-1.bin", bytes.clone(), None))
        .unwrap();
    assert_eq!(e.history().len(), n);
    put(&mut e, user(), vec![tempo(99.0)]);
    e.undo(&Scope::Any).unwrap();
    let Instrument::Clap(r) = &e.document().project.channels[0].instrument else {
        panic!()
    };
    assert_eq!(r.state_file.as_deref(), Some("2-1.bin"));
}

#[test]
fn entry_limit_evicts_oldest_and_keeps_the_tree_valid() {
    let lim = Limits {
        max_entries: 10,
        max_bytes: usize::MAX,
    };
    let mut e = Editor::with_limits(Document::new(), true, lim);
    for i in 0..30 {
        put(&mut e, user(), vec![tempo(100.0 + i as f64)]);
        assert!(e.history().len() <= 10);
        e.history().check_integrity().unwrap();
    }
    // The newest 10 states are reachable: tempos 120 to 129.
    for _ in 0..9 {
        e.undo(&Scope::Any).unwrap();
    }
    assert_eq!(bpm(&e), 120.0);
    assert_eq!(e.undo(&Scope::Any), Err(HistoryError::NothingToUndo));
}

#[test]
fn entry_limit_prefers_dropping_stale_branches() {
    let lim = Limits {
        max_entries: 6,
        max_bytes: usize::MAX,
    };
    let mut e = Editor::with_limits(Document::new(), true, lim);
    put(&mut e, user(), vec![tempo(100.0)]);
    put(&mut e, user(), vec![tempo(101.0)]);
    e.undo(&Scope::Any).unwrap();
    e.undo(&Scope::Any).unwrap();
    for i in 0..4 {
        put(&mut e, user(), vec![tempo(200.0 + i as f64)]);
    }
    assert!(e.history().len() <= 6);
    e.history().check_integrity().unwrap();
    assert_eq!(bpm(&e), 203.0);
    // The stale branch went first, so the whole new chain is still there.
    for _ in 0..4 {
        e.undo(&Scope::Any).unwrap();
    }
    assert_eq!(bpm(&e), 120.0);
}

#[test]
fn the_current_entry_is_never_evicted() {
    let lim = Limits {
        max_entries: 1,
        max_bytes: usize::MAX,
    };
    let mut e = Editor::with_limits(Document::new(), true, lim);
    for i in 0..5 {
        put(&mut e, user(), vec![tempo(100.0 + i as f64)]);
        assert_eq!(e.history().len(), 1);
    }
    assert_eq!(bpm(&e), 104.0);
    assert_eq!(e.undo(&Scope::Any), Err(HistoryError::NothingToUndo));
}

#[test]
fn byte_limit_counts_shared_blobs_once() {
    let mut e = Editor::with_limits(
        Document::new(),
        true,
        Limits {
            max_entries: 200,
            max_bytes: 3 << 20,
        },
    );
    put(&mut e, user(), vec![add_clap()]);
    let inst = InstanceId(e.document().project.max_id());
    let blob: Arc<[u8]> = Arc::from(vec![0u8; 1 << 20]);
    e.merge_with(|d| commit_plugin_state(d, inst, "2-1.bin", blob.clone(), None))
        .unwrap();
    for i in 0..40 {
        put(&mut e, user(), vec![tempo(100.0 + i as f64)]);
    }
    assert_eq!(e.history().len(), 42, "shared blob counted once: all kept");
    let size = e.history().size_bytes();
    assert!((1 << 20..2 << 20).contains(&size), "{size}");

    // Distinct blobs add up and force eviction.
    for g in 2..8u32 {
        let b: Arc<[u8]> = Arc::from(vec![g as u8; 1 << 20]);
        e.merge_with(|d| commit_plugin_state(d, inst, &format!("2-{g}.bin"), b.clone(), None))
            .unwrap();
        put(&mut e, user(), vec![tempo(300.0 + g as f64)]);
    }
    assert!(e.history().size_bytes() <= 3 << 20);
    assert!(e.history().len() < 42 + 6);
    e.history().check_integrity().unwrap();
}

#[test]
fn describe_uses_variant_name() {
    let mut e = editor();
    put(&mut e, user(), vec![tempo(100.0)]);
    put(&mut e, user(), vec![tempo(100.0), tempo(101.0)]);
    let d: Vec<_> = e
        .history()
        .infos()
        .into_iter()
        .map(|i| i.description)
        .collect();
    assert_eq!(d[1], "SetTempo");
    assert_eq!(d[2], "SetTempo and 1 more");
}

#[test]
fn random_editing_with_undo_redo_gestures_keeps_every_invariant() {
    for seed in 1..=25u64 {
        let mut r = Rng(0xC2B2_AE3D_27D4_EB4F ^ seed.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        let lim = Limits {
            max_entries: 40,
            max_bytes: usize::MAX,
        };
        let mut e = Editor::with_limits(Document::new(), true, lim);
        let mut seen: HashSet<u32> = HashSet::new();
        let authors = [user(), Author::Script, agent("a"), agent("b")];
        let mut tok = 0u64;
        let collect = |created: &[u32], seen: &mut HashSet<u32>| {
            for id in created {
                assert!(seen.insert(*id), "seed {seed}: id {id} reused");
            }
        };
        for _ in 0..500 {
            match r.below(12) {
                0 | 1 => {
                    let scope = if r.chance(50) {
                        Scope::Any
                    } else {
                        Scope::Only(authors[r.below(4) as usize].clone())
                    };
                    let _ = e.undo(&scope);
                }
                2 => {
                    let _ = e.redo(&Scope::Any);
                }
                3 => {
                    if e.gesture_open() {
                        for d in e.end_gesture() {
                            if let Ok(a) = d.result {
                                collect(&a.created, &mut seen);
                            }
                        }
                    } else {
                        e.begin_gesture(user(), "g");
                    }
                }
                _ => {
                    let a = authors[r.below(4) as usize].clone();
                    let n = 1 + r.below(3) as usize;
                    let mut edits = Vec::new();
                    let mut probe = e.document().clone();
                    for _ in 0..n {
                        let ed = random_edit(&mut r, &probe);
                        if let Ok((nd, _)) = crate::document::apply(&probe, &ed) {
                            probe = nd;
                        }
                        edits.push(ed);
                    }
                    tok += 1;
                    if let Ok(Submitted::Applied(x)) = e.submit(a, None, edits, tok) {
                        collect(&x.created, &mut seen);
                    }
                }
            }
            protocol::validate::validate(&e.document().project).unwrap();
            e.history().check_integrity().unwrap();
            assert!(e.history().len() <= 40);
            assert!(e.document().next_id > e.document().project.max_id());
            assert!(Arc::ptr_eq(&e.document().project, e.history().project()));
        }
        for d in e.end_gesture() {
            if let Ok(a) = d.result {
                collect(&a.created, &mut seen);
            }
        }
    }
}

#[test]
fn undo_restores_the_exact_previous_snapshot() {
    let mut e = editor();
    let before = e.document().project.clone();
    put(
        &mut e,
        user(),
        vec![
            Edit::AddChannel {
                name: "c".into(),
                instrument: NewInstrument::Synth {
                    params: SynthParams::default(),
                },
                root_key: 60,
                track: TrackId::MASTER,
            },
            Edit::AddPattern {
                instrument: ChannelId(1),
                name: "p".into(),
                length_steps: 16,
            },
        ],
    );
    let proj = e.document().project.clone();
    let (_c, p) = (proj.channels[0].id, proj.patterns[0].id);
    put(
        &mut e,
        user(),
        vec![Edit::AddNotes {
            pattern: p,
            notes: vec![NewNote {
                start: 0,
                len: 5,
                key: 1,
                vel: 1,
            }],
        }],
    );
    e.undo(&Scope::Any).unwrap();
    assert!(Arc::ptr_eq(&e.document().project, &proj));
    e.undo(&Scope::Any).unwrap();
    assert!(Arc::ptr_eq(&e.document().project, &before));
}
