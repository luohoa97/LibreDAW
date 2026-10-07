// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;
use crate::engine_adapter::compiled_note_count;
use protocol::edit::{MixValue, NewInstrument, NewNote};
use protocol::engine::{MixControl, channel_control, track_control};
use protocol::ids::{ChannelId, PatternId, TrackId};
use protocol::model::SynthParams;
use std::time::Duration;

fn session() -> Session {
    Session::new(
        Document::new(),
        true,
        EngineLink::stub(48000.0),
        Registry::new(Vec::new(), 48000.0),
    )
}

fn user(s: &mut Session, edits: Vec<Edit>) -> Applied {
    match s.submit(Author::User, None, edits, 0).expect("edit") {
        Submitted::Applied(a) => a,
        Submitted::Queued => panic!("queued"),
    }
}

fn settle(s: &mut Session) {
    assert!(s.compiler.wait_idle(Duration::from_secs(5)));
}

fn with_channel(s: &mut Session) -> (ChannelId, PatternId) {
    let a = user(
        s,
        vec![Edit::AddChannel {
            name: "c".into(),
            instrument: NewInstrument::Synth {
                params: SynthParams::default(),
            },
            root_key: 60,
            track: TrackId::MASTER,
        }],
    );
    let c = ChannelId(a.created[0]);
    let p = user(
        s,
        vec![Edit::AddPattern {
            instrument: c,
            name: "p".into(),
            length_steps: 16,
        }],
    );
    (c, PatternId(p.created[0]))
}

#[test]
fn startup_fills_tables_and_compiles_once() {
    let mut s = session();
    settle(&mut s);
    assert_eq!(s.jobs_requested(), 1);
    assert_eq!(s.link.controls.tempo(), 120.0);
    s.tick();
    assert_eq!(
        s.link.submitted.len(),
        1,
        "first Compiled reached the engine"
    );
}

#[test]
fn faders_write_tables_without_recompiling() {
    let mut s = session();
    let (c, _) = with_channel(&mut s);
    settle(&mut s);
    let before = s.jobs_requested();
    user(
        &mut s,
        vec![
            Edit::SetChannelMix {
                channel: c,
                value: MixValue::VolumeDb(-7.5),
            },
            Edit::SetTrackMix {
                track: TrackId::MASTER,
                value: MixValue::Mute(true),
            },
            Edit::SetTempo { bpm: 140.0 },
        ],
    );
    assert_eq!(
        s.jobs_requested(),
        before,
        "control-only edits never recompile"
    );
    let (cs, _) = s.slots.channel_slot(c).unwrap();
    assert_eq!(
        s.link
            .controls
            .get(channel_control(cs, MixControl::VolumeDb)),
        -7.5
    );
    assert_eq!(
        s.link.controls.get(track_control(
            protocol::engine::TrackSlot::MASTER,
            MixControl::Mute
        )),
        1.0
    );
    assert_eq!(s.link.controls.tempo(), 140.0);
}

#[test]
fn structural_edits_recompile_and_the_newest_revision_reaches_the_engine() {
    let mut s = session();
    let (_, p) = with_channel(&mut s);
    for step in 0..16u8 {
        user(
            &mut s,
            vec![Edit::SetStep {
                pattern: p,
                step,
                on: true,
                vel: None,
            }],
        );
    }
    settle(&mut s);
    s.tick();
    assert!(s.jobs_compiled() >= 1);
    assert!(s.jobs_compiled() <= s.jobs_requested());
    let last = s.link.submitted.last().unwrap();
    assert_eq!(compiled_note_count(last), 16, "newest is never dropped");
}

#[test]
fn a_full_state_ring_is_retried_not_lost() {
    let mut s = session();
    settle(&mut s);
    s.link.ring_capacity = Some(0);
    s.tick();
    assert!(s.link.submitted.is_empty());
    let (_, p) = with_channel(&mut s);
    user(
        &mut s,
        vec![Edit::SetStep {
            pattern: p,
            step: 0,
            on: true,
            vel: None,
        }],
    );
    settle(&mut s);
    s.tick();
    assert!(s.link.submitted.is_empty());
    s.link.ring_capacity = None;
    s.tick();
    assert_eq!(s.link.submitted.len(), 1);
    assert_eq!(compiled_note_count(&s.link.submitted[0]), 1);
}

#[test]
fn undo_and_redo_rewrite_the_tables() {
    let mut s = session();
    let (c, _) = with_channel(&mut s);
    user(
        &mut s,
        vec![Edit::SetChannelMix {
            channel: c,
            value: MixValue::Pan(-0.75),
        }],
    );
    let (cs, _) = s.slots.channel_slot(c).unwrap();
    let pan = |s: &Session| s.link.controls.get(channel_control(cs, MixControl::Pan));
    assert_eq!(pan(&s), -0.75);
    s.undo(&Scope::Any).unwrap();
    assert_eq!(pan(&s), 0.0);
    s.redo(&Scope::Any).unwrap();
    assert_eq!(pan(&s), -0.75);
    // Synth knobs too.
    user(
        &mut s,
        vec![Edit::SetSynthParam {
            channel: c,
            param: protocol::model::SynthParam::CutoffHz,
            value: 777.0,
        }],
    );
    let idx = protocol::engine::param_index(cs, protocol::model::SynthParam::CutoffHz.index());
    assert_eq!(s.link.params.get(idx), 777.0);
    s.undo(&Scope::Any).unwrap();
    assert_eq!(
        s.link.params.get(idx),
        SynthParams::default().cutoff_hz as f32
    );
}

#[test]
fn fader_gesture_writes_every_step_but_makes_one_undo_entry() {
    let mut s = session();
    let (c, _) = with_channel(&mut s);
    let n = s.editor.history().len();
    assert!(s.begin_gesture(Author::User, "fader"));
    let (cs, _) = s.slots.channel_slot(c).unwrap();
    for i in 0..20 {
        s.gesture_edit(&[Edit::SetChannelMix {
            channel: c,
            value: MixValue::VolumeDb(-(i as f64)),
        }])
        .unwrap();
        assert_eq!(
            s.link
                .controls
                .get(channel_control(cs, MixControl::VolumeDb)),
            -(i as f32)
        );
    }
    assert!(s.end_gesture().is_empty());
    assert_eq!(s.editor.history().len(), n + 1);
}

#[test]
fn script_batch_waits_for_the_gesture_and_then_applies() {
    let mut s = session();
    assert!(s.begin_gesture(Author::User, "drag"));
    let r = s
        .submit(Author::Script, None, vec![Edit::SetTempo { bpm: 99.0 }], 5)
        .unwrap();
    assert!(matches!(r, Submitted::Queued));
    assert_eq!(s.link.controls.tempo(), 120.0);
    let done = s.end_gesture();
    assert_eq!(done.len(), 1);
    assert!(done[0].result.is_ok());
    assert_eq!(s.link.controls.tempo(), 99.0);
}

#[test]
fn plugin_param_edits_replay_as_events_on_undo_but_are_not_echoed_from_plugins() {
    let mut s = session();
    let a = user(
        &mut s,
        vec![Edit::AddChannel {
            name: "p".into(),
            instrument: NewInstrument::Clap {
                plugin_id: "not.installed".into(),
                preset: None,
            },
            root_key: 60,
            track: TrackId::MASTER,
        }],
    );
    let inst = InstanceId(a.created[1]);
    assert!(
        s.take_messages()
            .iter()
            .any(|m| m.contains("not.installed")),
        "missing plugin is reported"
    );
    // A script sets a parameter: queued as an event for the plugin.
    user(
        &mut s,
        vec![Edit::SetPluginParam {
            instance: inst,
            param_id: 4,
            value: 0.8,
        }],
    );
    s.tick();
    assert_eq!(s.link.plugin_events.len(), 1);
    assert_eq!(s.link.plugin_events[0].param_id, 4);
    assert_eq!(s.link.plugin_events[0].value, 0.8);
    // Undo replays the difference: value goes back to "unset", which has no
    // event; redo sends 0.8 again.
    s.undo(&Scope::Any).unwrap();
    s.redo(&Scope::Any).unwrap();
    s.tick();
    assert_eq!(s.link.plugin_events.len(), 2);
    // A plugin-originated change is recorded but not sent back.
    let mut rep = TickReport::default();
    s.plugin_param_of(inst, 4, 0.1, &mut rep);
    s.tick();
    assert_eq!(s.link.plugin_events.len(), 2);
    assert!(rep.changed);
    let Instrument::Clap(r) = &s.document().project.channels[0].instrument else {
        panic!()
    };
    assert_eq!(r.params[0].value, 0.1);
}

#[test]
fn plugin_param_inside_a_plugin_gesture_is_one_undo_step() {
    let mut s = session();
    let a = user(
        &mut s,
        vec![Edit::AddChannel {
            name: "p".into(),
            instrument: NewInstrument::Clap {
                plugin_id: "not.installed".into(),
                preset: None,
            },
            root_key: 60,
            track: TrackId::MASTER,
        }],
    );
    let inst = InstanceId(a.created[1]);
    let n = s.editor.history().len();
    let mut rep = TickReport::default();
    s.plugin_gesture_begin();
    for i in 0..10 {
        s.plugin_param_of(inst, 1, i as f64 / 10.0, &mut rep);
    }
    // Undo is disabled while the plugin gesture is open.
    assert_eq!(s.undo(&Scope::Any), Err(HistoryError::GestureOpen));
    s.plugin_gesture_end(&mut rep);
    assert_eq!(s.editor.history().len(), n + 1);
    s.undo(&Scope::Any).unwrap();
    let Instrument::Clap(r) = &s.document().project.channels[0].instrument else {
        panic!()
    };
    assert!(r.params.is_empty());
}

#[test]
fn plugin_param_outside_a_gesture_is_not_an_undo_step_but_marks_dirty() {
    let mut s = session();
    let a = user(
        &mut s,
        vec![Edit::AddChannel {
            name: "p".into(),
            instrument: NewInstrument::Clap {
                plugin_id: "not.installed".into(),
                preset: None,
            },
            root_key: 60,
            track: TrackId::MASTER,
        }],
    );
    s.editor.mark_saved();
    let inst = InstanceId(a.created[1]);
    let n = s.editor.history().len();
    let mut rep = TickReport::default();
    s.plugin_param_of(inst, 1, 0.5, &mut rep);
    assert_eq!(s.editor.history().len(), n);
    assert!(s.editor.is_dirty());
}

#[test]
fn replace_document_resets_history_and_recompiles() {
    let mut s = session();
    let (_, p) = with_channel(&mut s);
    let _ = p;
    settle(&mut s);
    let before = s.jobs_requested();
    let rev = s.document().revision;
    s.replace_document(Document::new(), true);
    assert!(s.document().revision > rev);
    assert_eq!(s.editor.history().len(), 1);
    assert!(s.jobs_requested() > before);
    assert!(!s.editor.is_dirty());
}

#[test]
fn removing_a_channel_frees_its_slot_and_bumps_generation_on_reuse() {
    let mut s = session();
    let (c, _) = with_channel(&mut s);
    let (slot, gen_a) = s.slots.channel_slot(c).unwrap();
    user(&mut s, vec![Edit::RemoveChannel { channel: c }]);
    assert!(s.slots.channel_slot(c).is_none());
    let (c2, _) = with_channel(&mut s);
    let (slot2, gen_b) = s.slots.channel_slot(c2).unwrap();
    assert_eq!(slot, slot2);
    assert!(gen_b > gen_a);
}

#[test]
fn snapshot_for_save_returns_the_current_document() {
    let mut s = session();
    let (_, p) = with_channel(&mut s);
    user(
        &mut s,
        vec![Edit::AddNotes {
            pattern: p,
            notes: vec![NewNote {
                start: 0,
                len: 10,
                key: 40,
                vel: 90,
            }],
        }],
    );
    let d = s.snapshot_for_save();
    assert_eq!(d.revision, s.document().revision);
    assert_eq!(d.project.note_count(), 1);
}

#[test]
fn rejected_batch_reports_the_edit_index_and_changes_nothing() {
    let mut s = session();
    let rev = s.document().revision;
    let err = s
        .submit(
            Author::User,
            None,
            vec![
                Edit::SetTempo { bpm: 100.0 },
                Edit::SetTempo { bpm: 5000.0 },
            ],
            0,
        )
        .unwrap_err();
    assert_eq!(err.index, Some(1));
    assert_eq!(s.document().revision, rev);
    assert_eq!(s.link.controls.tempo(), 120.0);
}
