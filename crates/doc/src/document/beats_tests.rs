// SPDX-License-Identifier: GPL-3.0-or-later
//! Tests for the Milestone B edits and the changed semantics of 17.2.

use super::tests::{Rng, base, ok, synth};
use super::*;
use protocol::beats::{
    Bass808Param, BuiltinFx, BuiltinFxKind, SampleMode, SamplerParam, SaturatorCurve,
};
use protocol::ids::{ClipId, PlaylistTrackId};
use protocol::model::SampleRef;

fn hash(n: u32) -> String {
    format!("{n:064x}")
}

fn sample(n: u32) -> SampleRef {
    SampleRef {
        hash: hash(n),
        orig_name: format!("s{n}.wav"),
        size: 1000 + n as u64,
        local_only: n.is_multiple_of(2),
    }
}

fn fails(d: &Document, e: Edit) -> EditError {
    apply(d, &e).expect_err(&format!("should fail: {e:?}"))
}

fn add_sample(d: &Document, n: u32) -> Document {
    ok(d, Edit::AddSample { sample: sample(n) }).0
}

fn step_note(d: &Document, p: PatternId, c: ChannelId, step: u8) -> Note {
    let pat = d.project.pattern(p).unwrap();
    let t = step as u32 * pat.step_ticks;
    *pat.notes_of(c).iter().find(|n| n.start == t).expect("note")
}

fn step_on(d: &Document, p: PatternId, c: ChannelId, step: u8) -> Document {
    ok(
        d,
        Edit::SetStep {
            pattern: p,
            channel: c,
            step,
            on: true,
            vel: None,
        },
    )
    .0
}

fn lanes(
    d: &Document,
    p: PatternId,
    c: ChannelId,
    step: u8,
    off: Option<i8>,
    repeat: Option<u8>,
) -> Result<Document, EditError> {
    apply(
        d,
        &Edit::SetStepLanes {
            pattern: p,
            channel: c,
            step,
            vel: None,
            off,
            repeat,
        },
    )
    .map(|r| r.0)
}

// ---------------------------------------------------------------------------
// Steps, pitch lane, ratchets (17.2 fmt-3)

#[test]
fn step_lanes_set_velocity_pitch_and_ratchet() {
    let (d, c, p) = base();
    let d = step_on(&d, p, c, 4);
    let (d, _) = ok(
        &d,
        Edit::SetStepLanes {
            pattern: p,
            channel: c,
            step: 4,
            vel: Some(64),
            off: Some(-5),
            repeat: Some(4),
        },
    );
    let n = step_note(&d, p, c, 4);
    assert_eq!((n.vel, n.off, n.key, n.repeat), (64, -5, 55, 4));
    let pat = d.project.pattern(p).unwrap();
    assert!(n.is_step_note(60, pat));
    // `None` keeps a value.
    let d = lanes(&d, p, c, 4, Some(0), None).unwrap();
    let n = step_note(&d, p, c, 4);
    assert_eq!((n.vel, n.off, n.key, n.repeat), (64, 0, 60, 4));
}

#[test]
fn step_lanes_reject_bad_values_and_missing_step_notes() {
    let (d, c, p) = base();
    assert!(matches!(
        lanes(&d, p, c, 4, Some(1), None),
        Err(EditError::BadArgument { .. })
    ));
    let d = step_on(&d, p, c, 4);
    for off in [25i8, -25, 100] {
        assert!(matches!(
            lanes(&d, p, c, 4, Some(off), None),
            Err(EditError::Invalid { .. })
        ));
    }
    for rep in [0u8, 5, 7, 9] {
        assert!(lanes(&d, p, c, 4, None, Some(rep)).is_err(), "{rep}");
    }
    assert!(lanes(&d, p, c, 16, None, Some(2)).is_err());
    assert!(lanes(&d, p, ChannelId(999), 4, None, Some(2)).is_err());
    // 240 ticks: 8 divides, but not for a 100 tick step.
    let (d2, _) = ok(
        &d,
        Edit::SetStepTicks {
            pattern: p,
            step_ticks: 100,
        },
    );
    assert!(lanes(&d2, p, c, 4, None, Some(8)).is_err());
    assert!(lanes(&d2, p, c, 4, None, Some(4)).is_ok());
    // Root key 0 cannot take a negative offset: key would leave 0..127.
    let d = ok(&d, Edit::SetRootKey { channel: c, key: 0 }).0;
    assert!(lanes(&d, p, c, 4, Some(-1), None).is_err());
    assert!(lanes(&d, p, c, 4, Some(1), None).is_ok());
}

#[test]
fn toggle_off_removes_step_notes_whatever_their_offset() {
    let (d, c, p) = base();
    let d = step_on(&d, p, c, 2);
    let d = lanes(&d, p, c, 2, Some(7), Some(2)).unwrap();
    // Toggling on again keeps the existing note, off included.
    let d2 = step_on(&d, p, c, 2);
    assert_eq!(step_note(&d2, p, c, 2).off, 7);
    assert_eq!(d2.project.note_count(), 1);
    let (d3, _) = ok(
        &d,
        Edit::SetStep {
            pattern: p,
            channel: c,
            step: 2,
            on: false,
            vel: None,
        },
    );
    assert_eq!(d3.project.note_count(), 0);
}

#[test]
fn root_key_rewrites_key_to_new_root_plus_off() {
    let (d, c, p) = base();
    let d = step_on(&d, p, c, 0);
    let d = step_on(&d, p, c, 1);
    let d = lanes(&d, p, c, 1, Some(12), None).unwrap();
    let (d, _) = ok(
        &d,
        Edit::SetRootKey {
            channel: c,
            key: 40,
        },
    );
    assert_eq!(step_note(&d, p, c, 0).key, 40);
    let n = step_note(&d, p, c, 1);
    assert_eq!((n.key, n.off), (52, 12));
    let pat = d.project.pattern(p).unwrap();
    assert!(n.is_step_note(40, pat));
    // A rewrite that would leave 0..127 fails and changes nothing.
    let e = fails(
        &d,
        Edit::SetRootKey {
            channel: c,
            key: 120,
        },
    );
    assert!(matches!(e, EditError::Invalid { .. }), "{e}");
    assert_eq!(d.project.channel(c).unwrap().root_key, 40);
}

#[test]
fn piano_roll_move_and_resize_clear_the_pitch_offset() {
    let (d, c, p) = base();
    let d = step_on(&d, p, c, 3);
    let d = lanes(&d, p, c, 3, Some(5), None).unwrap();
    let id = step_note(&d, p, c, 3).id;

    let (m, _) = ok(
        &d,
        Edit::MoveNotes {
            pattern: p,
            notes: vec![id],
            dt: 0,
            dkey: 2,
        },
    );
    let n = m.project.pattern(p).unwrap().notes_of(c)[0];
    assert_eq!((n.off, n.key), (0, 67));

    let (r, _) = ok(
        &d,
        Edit::ResizeNotes {
            pattern: p,
            notes: vec![id],
            dlen: 20,
        },
    );
    let n = r.project.pattern(p).unwrap().notes_of(c)[0];
    assert_eq!((n.off, n.key, n.len), (0, 65, 260));

    // A zero move leaves it a step note.
    let (z, _) = ok(
        &d,
        Edit::MoveNotes {
            pattern: p,
            notes: vec![id],
            dt: 0,
            dkey: 0,
        },
    );
    assert_eq!(z.project.pattern(p).unwrap().notes_of(c)[0].off, 5);
}

#[test]
fn ratchet_blocks_resizes_that_break_divisibility() {
    let (d, c, p) = base();
    let d = step_on(&d, p, c, 0);
    let d = lanes(&d, p, c, 0, None, Some(8)).unwrap();
    let id = step_note(&d, p, c, 0).id;
    assert!(matches!(
        fails(
            &d,
            Edit::ResizeNotes {
                pattern: p,
                notes: vec![id],
                dlen: 1
            }
        ),
        EditError::BadArgument { .. }
    ));
    assert!(
        apply(
            &d,
            &Edit::ResizeNotes {
                pattern: p,
                notes: vec![id],
                dlen: 8
            }
        )
        .is_ok()
    );
}

#[test]
fn step_ticks_keeps_offsets_and_rejects_indivisible_ratchets() {
    let (d, c, p) = base();
    let d = step_on(&d, p, c, 2);
    let d = lanes(&d, p, c, 2, Some(-3), Some(3)).unwrap();
    let (d, _) = ok(
        &d,
        Edit::SetStepTicks {
            pattern: p,
            step_ticks: 480,
        },
    );
    let pat = d.project.pattern(p).unwrap();
    let n = step_note(&d, p, c, 2);
    assert_eq!(
        (n.start, n.len, n.off, n.key, n.repeat),
        (960, 480, -3, 57, 3)
    );
    assert!(n.is_step_note(60, pat));
    // 3 does not divide 100.
    let e = fails(
        &d,
        Edit::SetStepTicks {
            pattern: p,
            step_ticks: 100,
        },
    );
    assert!(matches!(e, EditError::Invalid { .. }), "{e}");
}

#[test]
fn note_repeat_checks_values_and_lengths() {
    let (d, c, p) = base();
    let (d, _) = ok(
        &d,
        Edit::AddNotes {
            pattern: p,
            channel: c,
            notes: vec![
                NewNote {
                    start: 0,
                    len: 480,
                    key: 60,
                    vel: 100,
                },
                NewNote {
                    start: 480,
                    len: 7,
                    key: 60,
                    vel: 100,
                },
            ],
        },
    );
    let ids: Vec<NoteId> = d
        .project
        .pattern(p)
        .unwrap()
        .notes_of(c)
        .iter()
        .map(|n| n.id)
        .collect();
    let set = |notes: Vec<NoteId>, repeat: u8| Edit::SetNoteRepeat {
        pattern: p,
        notes,
        repeat,
    };
    assert!(apply(&d, &set(vec![ids[0]], 6)).is_ok());
    assert!(apply(&d, &set(vec![ids[0]], 5)).is_err());
    assert!(apply(&d, &set(vec![ids[1]], 2)).is_err());
    assert!(apply(&d, &set(vec![NoteId(9999)], 2)).is_err());
    // Atomic: the second note fails, so the first stays at 1.
    assert!(apply(&d, &set(ids.clone(), 2)).is_err());
}

#[test]
fn swing_and_choke_are_range_checked() {
    let (d, c, p) = base();
    let (d, _) = ok(
        &d,
        Edit::SetSwing {
            pattern: p,
            swing: 750,
        },
    );
    assert_eq!(d.project.pattern(p).unwrap().swing, 750);
    assert!(matches!(
        fails(
            &d,
            Edit::SetSwing {
                pattern: p,
                swing: 751
            }
        ),
        EditError::Invalid { .. }
    ));
    assert!(matches!(
        fails(
            &d,
            Edit::SetSwing {
                pattern: PatternId(77),
                swing: 1
            }
        ),
        EditError::NotFound { .. }
    ));
    let (d, _) = ok(
        &d,
        Edit::SetChokeGroup {
            channel: c,
            group: 16,
        },
    );
    assert_eq!(d.project.channel(c).unwrap().choke_group, 16);
    assert!(
        fails(
            &d,
            Edit::SetChokeGroup {
                channel: c,
                group: 17
            }
        )
        .to_string()
        .contains("choke_group")
    );
    let (d, _) = ok(
        &d,
        Edit::SetChokeGroup {
            channel: c,
            group: 0,
        },
    );
    assert_eq!(d.project.channel(c).unwrap().choke_group, 0);
}

// ---------------------------------------------------------------------------
// Samples, sampler, 808

#[test]
fn samples_register_sort_and_refuse_removal_while_used() {
    let (d, _, _) = base();
    let d = add_sample(&d, 9);
    let d = add_sample(&d, 3);
    let d = add_sample(&d, 3); // no-op
    let hashes: Vec<String> = d.project.samples.iter().map(|s| s.hash.clone()).collect();
    assert_eq!(hashes, vec![hash(3), hash(9)]);

    let (d, c) = ok(
        &d,
        Edit::AddChannel {
            name: "Kick".into(),
            instrument: NewInstrument::Sampler {
                sample: Some(hash(9)),
                mode: SampleMode::OneShot,
            },
            root_key: 60,
            track: TrackId::MASTER,
        },
    );
    let ch = ChannelId(c[0]);
    let e = fails(&d, Edit::RemoveSample { hash: hash(9) });
    assert!(e.to_string().contains("used by channel"), "{e}");
    let (d, _) = ok(&d, Edit::RemoveSample { hash: hash(3) });
    assert_eq!(d.project.samples.len(), 1);
    let (d, _) = ok(
        &d,
        Edit::SetSamplerSample {
            channel: ch,
            sample: None,
        },
    );
    let (d, _) = ok(&d, Edit::RemoveSample { hash: hash(9) });
    assert!(d.project.samples.is_empty());
    assert!(matches!(
        fails(&d, Edit::RemoveSample { hash: hash(9) }),
        EditError::NotFound { .. }
    ));
}

#[test]
fn sample_refs_are_validated() {
    let (d, _, _) = base();
    let mut s = sample(1);
    s.hash = "ABC".into();
    assert!(apply(&d, &Edit::AddSample { sample: s }).is_err());
    let mut s = sample(1);
    s.hash = hash(1).to_uppercase().replace('0', "A");
    assert!(apply(&d, &Edit::AddSample { sample: s }).is_err());
    let mut s = sample(1);
    s.orig_name = String::new();
    assert!(apply(&d, &Edit::AddSample { sample: s }).is_err());
    // Channel and sampler edits name only registered samples.
    let e = apply(
        &d,
        &Edit::AddChannel {
            name: "S".into(),
            instrument: NewInstrument::Sampler {
                sample: Some(hash(1)),
                mode: SampleMode::Pitched,
            },
            root_key: 60,
            track: TrackId::MASTER,
        },
    );
    assert!(matches!(e, Err(EditError::NotFound { .. })));
}

#[test]
fn sampler_edits() {
    let (d, _, _) = base();
    let d = add_sample(&d, 1);
    let (d, c) = ok(
        &d,
        Edit::AddChannel {
            name: "S".into(),
            instrument: NewInstrument::Sampler {
                sample: None,
                mode: SampleMode::OneShot,
            },
            root_key: 60,
            track: TrackId::MASTER,
        },
    );
    let ch = ChannelId(c[0]);
    let (d, _) = ok(
        &d,
        Edit::SetSamplerSample {
            channel: ch,
            sample: Some(hash(1)),
        },
    );
    assert!(matches!(
        fails(
            &d,
            Edit::SetSamplerSample {
                channel: ch,
                sample: Some(hash(2))
            }
        ),
        EditError::NotFound { .. }
    ));
    let (d, _) = ok(
        &d,
        Edit::SetSamplerMode {
            channel: ch,
            mode: SampleMode::Pitched,
            reverse: true,
        },
    );
    let (d, _) = ok(
        &d,
        Edit::SetSamplerParam {
            channel: ch,
            param: SamplerParam::Semitones,
            value: -7.0,
        },
    );
    let Instrument::Sampler(s) = &d.project.channel(ch).unwrap().instrument else {
        panic!()
    };
    assert_eq!(s.sample.as_deref(), Some(hash(1).as_str()));
    assert_eq!(
        (s.mode, s.reverse, s.params.semitones),
        (SampleMode::Pitched, true, -7.0)
    );
    // Range checks, and end must stay above start.
    for (param, value) in [
        (SamplerParam::Semitones, 49.0),
        (SamplerParam::Cents, f64::NAN),
        (SamplerParam::Sustain, -0.1),
        (SamplerParam::GainDb, 13.0),
        (SamplerParam::End, 0.0),
        (SamplerParam::Start, 1.0),
    ] {
        assert!(
            matches!(
                fails(
                    &d,
                    Edit::SetSamplerParam {
                        channel: ch,
                        param,
                        value
                    }
                ),
                EditError::Invalid { .. }
            ),
            "{param:?} {value}"
        );
    }
    // Wrong instrument kinds.
    let (d, sy) = ok(
        &d,
        Edit::AddChannel {
            name: "Y".into(),
            instrument: synth(),
            root_key: 60,
            track: TrackId::MASTER,
        },
    );
    let sy = ChannelId(sy[0]);
    for e in [
        Edit::SetSamplerSample {
            channel: sy,
            sample: None,
        },
        Edit::SetSamplerMode {
            channel: sy,
            mode: SampleMode::OneShot,
            reverse: false,
        },
        Edit::SetSamplerParam {
            channel: sy,
            param: SamplerParam::Start,
            value: 0.0,
        },
        Edit::SetBass808Mono {
            channel: ch,
            mono: true,
        },
        Edit::SetBass808Param {
            channel: ch,
            param: Bass808Param::Tune,
            value: 0.0,
        },
    ] {
        assert!(matches!(fails(&d, e), EditError::BadArgument { .. }));
    }
}

#[test]
fn bass808_edits() {
    let (d, _, _) = base();
    let (d, c) = ok(
        &d,
        Edit::AddChannel {
            name: "808".into(),
            instrument: NewInstrument::Bass808 { mono: false },
            root_key: 36,
            track: TrackId::MASTER,
        },
    );
    let ch = ChannelId(c[0]);
    let (d, _) = ok(
        &d,
        Edit::SetBass808Mono {
            channel: ch,
            mono: true,
        },
    );
    let (d, _) = ok(
        &d,
        Edit::SetBass808Param {
            channel: ch,
            param: Bass808Param::DecayMs,
            value: 800.0,
        },
    );
    let Instrument::Bass808(b) = &d.project.channel(ch).unwrap().instrument else {
        panic!()
    };
    assert!(b.mono);
    assert_eq!(b.params.decay_ms, 800.0);
    for (param, value) in [
        (Bass808Param::DecayMs, 49.0),
        (Bass808Param::Click, 1.5),
        (Bass808Param::ToneHz, f64::INFINITY),
    ] {
        assert!(
            fails(
                &d,
                Edit::SetBass808Param {
                    channel: ch,
                    param,
                    value
                }
            )
            .to_string()
            .contains("bass808")
        );
    }
}

// ---------------------------------------------------------------------------
// Effects and routing

fn two_tracks(d: &Document) -> (Document, TrackId, TrackId) {
    let (d, a) = ok(d, Edit::AddTrack { name: "A".into() });
    let (d, b) = ok(&d, Edit::AddTrack { name: "B".into() });
    (d, TrackId(a[0]), TrackId(b[0]))
}

fn builtin(d: &Document, t: TrackId, kind: BuiltinFxKind) -> (Document, InstanceId) {
    let n = d.project.track(t).unwrap().inserts.len() as u8;
    let (d, c) = ok(
        d,
        Edit::AddBuiltinInsert {
            track: t,
            index: n,
            fx: kind,
        },
    );
    (d, InstanceId(c[0]))
}

#[test]
fn builtin_inserts_params_and_order() {
    let (d, _, _) = base();
    let (d, a, _) = two_tracks(&d);
    let (d, eq) = builtin(&d, a, BuiltinFxKind::Eq);
    let (d, sat) = builtin(&d, a, BuiltinFxKind::Saturator);
    let (d, dl) = builtin(&d, a, BuiltinFxKind::Delay);
    let (d, _) = ok(
        &d,
        Edit::AddInsert {
            track: a,
            index: 1,
            plugin_id: "x.y".into(),
        },
    );
    let order = |d: &Document| -> Vec<u32> {
        d.project
            .track(a)
            .unwrap()
            .inserts
            .iter()
            .map(|i| i.instance().0)
            .collect()
    };
    assert_eq!(order(&d).len(), 4);
    assert_eq!(order(&d)[0], eq.0);

    let (d, _) = ok(
        &d,
        Edit::SetFxParam {
            track: a,
            instance: eq,
            param: 2,
            value: 6.0,
        },
    );
    let Insert::Builtin { fx, .. } = &d.project.track(a).unwrap().inserts[0] else {
        panic!()
    };
    assert_eq!(fx.param(2), Some(6.0));
    for (param, value) in [(2u8, 25.0), (2, f64::NAN), (8, 0.0), (200, 0.0)] {
        assert!(
            apply(
                &d,
                &Edit::SetFxParam {
                    track: a,
                    instance: eq,
                    param,
                    value
                }
            )
            .is_err(),
            "{param} {value}"
        );
    }
    let (d, _) = ok(
        &d,
        Edit::SetSaturatorCurve {
            track: a,
            instance: sat,
            curve: SaturatorCurve::Fold,
        },
    );
    let (d, _) = ok(
        &d,
        Edit::SetDelayPingPong {
            track: a,
            instance: dl,
            ping_pong: true,
        },
    );
    assert!(matches!(
        fails(
            &d,
            Edit::SetSaturatorCurve {
                track: a,
                instance: dl,
                curve: SaturatorCurve::Hard
            }
        ),
        EditError::BadArgument { .. }
    ));
    assert!(matches!(
        fails(
            &d,
            Edit::SetDelayPingPong {
                track: a,
                instance: sat,
                ping_pong: true
            }
        ),
        EditError::BadArgument { .. }
    ));
    // Setting an effect parameter on a CLAP insert is refused.
    let clap = d.project.track(a).unwrap().inserts[1].instance();
    assert!(matches!(
        fails(
            &d,
            Edit::SetFxParam {
                track: a,
                instance: clap,
                param: 0,
                value: 0.0
            }
        ),
        EditError::BadArgument { .. }
    ));

    // Move: to the end, to the front, and past the end.
    let (d, _) = ok(
        &d,
        Edit::MoveInsert {
            track: a,
            instance: eq,
            index: 3,
        },
    );
    assert_eq!(order(&d)[3], eq.0);
    let (d, _) = ok(
        &d,
        Edit::MoveInsert {
            track: a,
            instance: eq,
            index: 0,
        },
    );
    assert_eq!(order(&d)[0], eq.0);
    assert!(
        fails(
            &d,
            Edit::MoveInsert {
                track: a,
                instance: eq,
                index: 4
            }
        )
        .to_string()
        .contains("past the end")
    );
    // RemoveInsert works on built-in effects too.
    let (d, _) = ok(
        &d,
        Edit::RemoveInsert {
            track: a,
            instance: dl,
        },
    );
    assert_eq!(order(&d).len(), 3);
}

#[test]
fn insert_and_pool_limits() {
    let (d, _, _) = base();
    let (mut d, a, _) = two_tracks(&d);
    for _ in 0..MAX_INSERTS {
        d = builtin(&d, a, BuiltinFxKind::Reverb).0;
    }
    assert!(matches!(
        fails(
            &d,
            Edit::AddBuiltinInsert {
                track: a,
                index: 0,
                fx: BuiltinFxKind::Eq
            }
        ),
        EditError::Invalid { .. }
    ));
    // Pool of 16 reverbs project-wide.
    let mut d = Document::new();
    let mut tracks = Vec::new();
    for _ in 0..8 {
        let (nd, c) = ok(&d, Edit::AddTrack { name: "t".into() });
        d = nd;
        tracks.push(TrackId(c[0]));
    }
    let mut made = 0;
    'o: for t in &tracks {
        for _ in 0..4 {
            if made == FX_POOL_REVERB {
                break 'o;
            }
            d = builtin(&d, *t, BuiltinFxKind::Reverb).0;
            made += 1;
        }
    }
    let e = fails(
        &d,
        Edit::AddBuiltinInsert {
            track: tracks[7],
            index: 0,
            fx: BuiltinFxKind::Reverb,
        },
    );
    assert!(e.to_string().contains("reverbs"), "{e}");
}

#[test]
fn sends_add_update_remove_and_reject_cycles() {
    let (d, _, _) = base();
    let (d, a, b) = two_tracks(&d);
    let (d, c) = ok(&d, Edit::AddTrack { name: "C".into() });
    let c = TrackId(c[0]);
    let send = |t, to, level_db| Edit::SetSend {
        track: t,
        to,
        level_db,
        pre_fader: false,
    };
    let (d, _) = ok(&d, send(a, c, -6.0));
    let (d, _) = ok(&d, send(a, b, -3.0));
    let (d, _) = ok(&d, send(a, c, -9.0)); // update
    let sends = &d.project.track(a).unwrap().sends;
    assert_eq!(sends.len(), 2);
    assert!(sends[0].to < sends[1].to, "sorted by target");
    assert_eq!(sends.iter().find(|s| s.to == c).unwrap().level_db, -9.0);

    // Rejections.
    assert!(matches!(
        fails(&d, send(a, a, 0.0)),
        EditError::BadArgument { .. }
    ));
    assert!(matches!(
        fails(&d, send(a, TrackId::MASTER, 0.0)),
        EditError::BadArgument { .. }
    ));
    assert!(matches!(
        fails(&d, send(a, TrackId(999), 0.0)),
        EditError::NotFound { .. }
    ));
    assert!(matches!(
        fails(&d, send(a, b, 13.0)),
        EditError::Invalid { .. }
    ));
    assert!(matches!(
        fails(&d, send(a, b, f64::NAN)),
        EditError::Invalid { .. }
    ));
    // a -> b exists; b -> a closes a loop.
    assert!(matches!(
        fails(&d, send(b, a, 0.0)),
        EditError::Invalid {
            reason: ValidationError::RoutingCycle
        }
    ));
    // At most MAX_SENDS.
    let mut d2 = d.clone();
    let mut extra = Vec::new();
    for _ in 0..3 {
        let (nd, t) = ok(&d2, Edit::AddTrack { name: "R".into() });
        d2 = nd;
        extra.push(TrackId(t[0]));
    }
    d2 = ok(&d2, send(a, extra[0], 0.0)).0;
    d2 = ok(&d2, send(a, extra[1], 0.0)).0;
    assert!(matches!(
        fails(&d2, send(a, extra[2], 0.0)),
        EditError::Invalid { .. }
    ));
    // Remove.
    let (d, _) = ok(&d, Edit::RemoveSend { track: a, to: c });
    assert_eq!(d.project.track(a).unwrap().sends.len(), 1);
    assert!(matches!(
        fails(&d, Edit::RemoveSend { track: a, to: c }),
        EditError::NotFound { .. }
    ));
}

#[test]
fn sidechain_sets_clears_and_rejects_loops() {
    let (d, _, _) = base();
    let (d, a, b) = two_tracks(&d);
    let (d, comp) = builtin(&d, a, BuiltinFxKind::Compressor);
    let (d, eq) = builtin(&d, a, BuiltinFxKind::Eq);
    let (d, _) = ok(
        &d,
        Edit::SetSidechain {
            track: a,
            instance: comp,
            source: Some(b),
        },
    );
    let key = |d: &Document| match &d.project.track(a).unwrap().inserts[0] {
        Insert::Builtin {
            fx: BuiltinFx::Compressor { sidechain, .. },
            ..
        } => *sidechain,
        _ => panic!(),
    };
    assert_eq!(key(&d), Some(b));
    assert!(matches!(
        fails(
            &d,
            Edit::SetSidechain {
                track: a,
                instance: eq,
                source: Some(b)
            }
        ),
        EditError::BadArgument { .. }
    ));
    assert!(matches!(
        fails(
            &d,
            Edit::SetSidechain {
                track: a,
                instance: comp,
                source: Some(a)
            }
        ),
        EditError::BadArgument { .. }
    ));
    assert!(matches!(
        fails(
            &d,
            Edit::SetSidechain {
                track: a,
                instance: comp,
                source: Some(TrackId(500))
            }
        ),
        EditError::NotFound { .. }
    ));
    // b feeds a's compressor, so a send a -> b would loop.
    assert!(matches!(
        fails(
            &d,
            Edit::SetSend {
                track: a,
                to: b,
                level_db: 0.0,
                pre_fader: false
            }
        ),
        EditError::Invalid {
            reason: ValidationError::RoutingCycle
        }
    ));
    // A compressor on b keyed from a closes the loop too.
    let (d2, comp_b) = builtin(&d, b, BuiltinFxKind::Compressor);
    assert!(matches!(
        fails(
            &d2,
            Edit::SetSidechain {
                track: b,
                instance: comp_b,
                source: Some(a)
            }
        ),
        EditError::Invalid {
            reason: ValidationError::RoutingCycle
        }
    ));
    let (d, _) = ok(
        &d,
        Edit::SetSidechain {
            track: a,
            instance: comp,
            source: None,
        },
    );
    assert_eq!(key(&d), None);
}

#[test]
fn remove_track_removes_sends_to_it_and_clears_sidechains() {
    let (d, _, _) = base();
    let (d, a, b) = two_tracks(&d);
    let (d, c) = ok(&d, Edit::AddTrack { name: "C".into() });
    let c = TrackId(c[0]);
    let (d, comp) = builtin(&d, c, BuiltinFxKind::Compressor);
    let (d, _) = ok(
        &d,
        Edit::SetSidechain {
            track: c,
            instance: comp,
            source: Some(b),
        },
    );
    let (d, _) = ok(
        &d,
        Edit::SetSend {
            track: a,
            to: b,
            level_db: 0.0,
            pre_fader: true,
        },
    );
    let (d, _) = ok(
        &d,
        Edit::SetSend {
            track: a,
            to: c,
            level_db: 0.0,
            pre_fader: true,
        },
    );
    let (d, _) = ok(&d, Edit::RemoveTrack { track: b });
    assert_eq!(d.project.track(a).unwrap().sends.len(), 1);
    assert_eq!(d.project.track(a).unwrap().sends[0].to, c);
    let Insert::Builtin {
        fx: BuiltinFx::Compressor { sidechain, .. },
        ..
    } = &d.project.track(c).unwrap().inserts[0]
    else {
        panic!()
    };
    assert_eq!(*sidechain, None);
    // Removing a sender needs no cleanup elsewhere.
    let (d, _) = ok(&d, Edit::RemoveTrack { track: a });
    assert!(d.project.track(c).is_some());
}

// ---------------------------------------------------------------------------
// Playlist

fn song() -> (Document, PlaylistTrackId, PatternId) {
    let (d, _, p) = base();
    let (d, t) = ok(
        &d,
        Edit::AddPlaylistTrack {
            name: "Drums".into(),
        },
    );
    (d, PlaylistTrackId(t[0]), p)
}

fn clips(d: &Document, t: PlaylistTrackId) -> Vec<(u32, u32, u32)> {
    d.project
        .playlist
        .iter()
        .find(|x| x.id == t)
        .unwrap()
        .clips
        .iter()
        .map(|c| (c.id.0, c.start, c.len))
        .collect()
}

#[test]
fn playlist_tracks_and_clips() {
    let (d, t, p) = song();
    let add = |start, len| Edit::AddClip {
        track: t,
        pattern: p,
        start,
        len,
    };
    let (d, a) = ok(&d, add(3840, 3840));
    let (d, b) = ok(&d, add(0, 3840));
    assert_eq!(
        clips(&d, t).iter().map(|c| c.1).collect::<Vec<_>>(),
        [0, 3840],
        "sorted by start"
    );
    assert_eq!(a.len(), 1);
    assert_ne!(a, b);
    // Overlap, bad length, bad pattern, bad track.
    assert!(matches!(
        fails(&d, add(3000, 100)),
        EditError::Invalid {
            reason: ValidationError::Overlap { .. }
        }
    ));
    assert!(fails(&d, add(7680, 0)).to_string().contains("clip.len"));
    assert!(fails(&d, add(u32::MAX, 5)).to_string().contains("clip.end"));
    assert!(matches!(
        fails(
            &d,
            Edit::AddClip {
                track: t,
                pattern: PatternId(999),
                start: 9000,
                len: 10
            }
        ),
        EditError::NotFound { .. }
    ));
    assert!(matches!(
        fails(
            &d,
            Edit::AddClip {
                track: PlaylistTrackId(999),
                pattern: p,
                start: 9000,
                len: 10
            }
        ),
        EditError::NotFound { .. }
    ));
    // Touching clips are fine.
    assert!(apply(&d, &add(7680, 10)).is_ok());

    let (d, _) = ok(
        &d,
        Edit::RenamePlaylistTrack {
            track: t,
            name: "Beat".into(),
        },
    );
    assert_eq!(d.project.playlist[0].name, "Beat");
    assert!(
        fails(
            &d,
            Edit::RenamePlaylistTrack {
                track: t,
                name: String::new()
            }
        )
        .to_string()
        .contains("playlist.name")
    );

    let ids = [ClipId(a[0]), ClipId(b[0])];
    assert!(matches!(
        fails(
            &d,
            Edit::RemoveClips {
                clips: vec![ids[0], ClipId(5555)]
            }
        ),
        EditError::NotFound { .. }
    ));
    let (d2, _) = ok(
        &d,
        Edit::RemoveClips {
            clips: vec![ids[0], ids[0]],
        },
    );
    assert_eq!(clips(&d2, t).len(), 1);
    // Removing the track removes its clips.
    let (d3, _) = ok(&d, Edit::RemovePlaylistTrack { track: t });
    assert!(d3.project.playlist.is_empty());
}

#[test]
fn clips_move_resize_and_change_rows() {
    let (d, t1, p) = song();
    let (d, t2) = ok(
        &d,
        Edit::AddPlaylistTrack {
            name: "Bass".into(),
        },
    );
    let t2 = PlaylistTrackId(t2[0]);
    let (d, a) = ok(
        &d,
        Edit::AddClip {
            track: t1,
            pattern: p,
            start: 0,
            len: 960,
        },
    );
    let (d, b) = ok(
        &d,
        Edit::AddClip {
            track: t1,
            pattern: p,
            start: 960,
            len: 960,
        },
    );
    let (a, b) = (ClipId(a[0]), ClipId(b[0]));
    // Move both right: no clash among themselves, and rows by id order.
    let (m, _) = ok(
        &d,
        Edit::MoveClips {
            clips: vec![a, b],
            dt: 480,
            dtrack: 0,
        },
    );
    assert_eq!(
        clips(&m, t1).iter().map(|c| c.1).collect::<Vec<_>>(),
        [480, 1440]
    );
    let (m, _) = ok(
        &d,
        Edit::MoveClips {
            clips: vec![b],
            dt: 0,
            dtrack: 1,
        },
    );
    assert_eq!(clips(&m, t2).len(), 1);
    assert_eq!(clips(&m, t1).len(), 1);
    // Failures: overlap, out of range, negative start, missing row.
    assert!(matches!(
        fails(
            &d,
            Edit::MoveClips {
                clips: vec![a],
                dt: 100,
                dtrack: 0
            }
        ),
        EditError::Invalid {
            reason: ValidationError::Overlap { .. }
        }
    ));
    assert!(
        fails(
            &d,
            Edit::MoveClips {
                clips: vec![a],
                dt: -1,
                dtrack: 0
            }
        )
        .to_string()
        .contains("clip.start")
    );
    assert!(
        fails(
            &d,
            Edit::MoveClips {
                clips: vec![a],
                dt: 0,
                dtrack: 2
            }
        )
        .to_string()
        .contains("clip.track")
    );
    assert!(
        fails(
            &d,
            Edit::MoveClips {
                clips: vec![a],
                dt: 0,
                dtrack: -1
            }
        )
        .to_string()
        .contains("clip.track")
    );
    fails(
        &d,
        Edit::MoveClips {
            clips: vec![a],
            dt: i64::MAX,
            dtrack: 0,
        },
    );
    assert!(matches!(
        fails(
            &d,
            Edit::MoveClips {
                clips: vec![ClipId(404)],
                dt: 0,
                dtrack: 0
            }
        ),
        EditError::NotFound { .. }
    ));
    // Resize.
    let (r, _) = ok(
        &d,
        Edit::ResizeClips {
            clips: vec![a, b],
            dlen: -480,
        },
    );
    assert_eq!(
        clips(&r, t1).iter().map(|c| c.2).collect::<Vec<_>>(),
        [480, 480]
    );
    assert!(
        fails(
            &d,
            Edit::ResizeClips {
                clips: vec![a],
                dlen: -960
            }
        )
        .to_string()
        .contains("clip.len")
    );
    assert!(matches!(
        fails(
            &d,
            Edit::ResizeClips {
                clips: vec![a],
                dlen: 100
            }
        ),
        EditError::Invalid {
            reason: ValidationError::Overlap { .. }
        }
    ));
    fails(
        &d,
        Edit::ResizeClips {
            clips: vec![a],
            dlen: i64::MAX,
        },
    );
}

#[test]
fn remove_pattern_removes_its_clips() {
    let (d, t, p) = song();
    let (d, q) = ok(
        &d,
        Edit::AddPattern {
            name: "B".into(),
            length_steps: 16,
        },
    );
    let q = PatternId(q[0]);
    let (d, _) = ok(
        &d,
        Edit::AddClip {
            track: t,
            pattern: p,
            start: 0,
            len: 960,
        },
    );
    let (d, _) = ok(
        &d,
        Edit::AddClip {
            track: t,
            pattern: q,
            start: 960,
            len: 960,
        },
    );
    let (d, _) = ok(&d, Edit::RemovePattern { pattern: p });
    let left = &d.project.playlist[0].clips;
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].pattern, q);
}

// ---------------------------------------------------------------------------
// ids (17.1)

#[test]
fn new_id_kinds_come_from_the_counter_and_never_repeat() {
    let (d, t, p) = song();
    let (d, a) = ok(
        &d,
        Edit::AddClip {
            track: t,
            pattern: p,
            start: 0,
            len: 10,
        },
    );
    let (d, fx) = ok(
        &d,
        Edit::AddBuiltinInsert {
            track: TrackId::MASTER,
            index: 0,
            fx: BuiltinFxKind::Limiter,
        },
    );
    let all = [t.0, a[0], fx[0]];
    assert!(all.windows(2).all(|w| w[0] < w[1]));
    assert!(d.next_id > d.project.max_id());
    // Undo-like replacement keeps the counter monotonic.
    let earlier = Document::new().project;
    let back = d.with_project(earlier);
    assert_eq!(back.next_id, d.next_id);
    // A loaded project raises the counter above builtin instances and clips.
    let (loaded, next) =
        protocol::format::parse(&protocol::format::emit(&d.project, 1).unwrap()).unwrap();
    let from = Document::from_project(loaded, next);
    assert!(from.next_id > fx[0] && from.next_id > a[0]);
    // Removing the last entity does not free its id.
    let (d2, _) = ok(
        &d,
        Edit::RemoveClips {
            clips: vec![ClipId(a[0])],
        },
    );
    let (_, again) = ok(
        &d2,
        Edit::AddClip {
            track: t,
            pattern: p,
            start: 0,
            len: 10,
        },
    );
    assert!(again[0] > a[0]);
}

#[test]
fn a_whole_beat_session_round_trips_through_the_file_format() {
    let (d, kick, p) = base();
    let d = add_sample(&d, 1);
    let (d, s) = ok(
        &d,
        Edit::AddChannel {
            name: "Snare".into(),
            instrument: NewInstrument::Sampler {
                sample: Some(hash(1)),
                mode: SampleMode::OneShot,
            },
            root_key: 60,
            track: TrackId::MASTER,
        },
    );
    let s = ChannelId(s[0]);
    let (d, _) = ok(
        &d,
        Edit::SetChokeGroup {
            channel: s,
            group: 3,
        },
    );
    let d = step_on(&d, p, kick, 0);
    let d = lanes(&d, p, kick, 0, Some(-12), Some(2)).unwrap();
    let (d, _) = ok(
        &d,
        Edit::SetSwing {
            pattern: p,
            swing: 300,
        },
    );
    let (d, t) = ok(&d, Edit::AddTrack { name: "Rev".into() });
    let t = TrackId(t[0]);
    let (d, _) = builtin(&d, t, BuiltinFxKind::Reverb);
    let (d, src) = ok(
        &d,
        Edit::AddTrack {
            name: "Drums".into(),
        },
    );
    let (d, _) = ok(
        &d,
        Edit::SetSend {
            track: TrackId(src[0]),
            to: t,
            level_db: 0.0,
            pre_fader: false,
        },
    );
    assert_roundtrip(&d);
}

fn assert_roundtrip(d: &Document) {
    let text = protocol::format::emit(&d.project, d.next_id).expect("emits");
    let (back, next) = protocol::format::parse(&text).expect("parses");
    assert_eq!(back, *d.project);
    assert_eq!(protocol::format::emit(&back, next).unwrap(), text);
}

// ---------------------------------------------------------------------------
// Property tests

fn pick_hash(r: &mut Rng) -> String {
    hash(1 + r.below(5) as u32)
}

fn in_range(r: &mut Rng, (lo, hi): (f64, f64)) -> f64 {
    if r.chance(90) {
        lo + (hi - lo) * (r.below(1001) as f64 / 1000.0)
    } else {
        match r.below(4) {
            0 => f64::NAN,
            1 => hi + 1.0,
            2 => lo - 1.0,
            _ => f64::INFINITY,
        }
    }
}

/// Random Milestone B edits against existing entities, with some garbage.
pub(crate) fn random_edit_b(r: &mut Rng, d: &Document) -> Edit {
    let p = &d.project;
    let pat = |r: &mut Rng| {
        if r.chance(94) {
            r.pick(&p.patterns.iter().map(|x| x.id).collect::<Vec<_>>())
                .unwrap_or(PatternId(r.below(40) as u32))
        } else {
            PatternId(r.below(60) as u32)
        }
    };
    let chan = |r: &mut Rng| {
        if r.chance(94) {
            r.pick(&p.channels.iter().map(|x| x.id).collect::<Vec<_>>())
                .unwrap_or(ChannelId(r.below(40) as u32))
        } else {
            ChannelId(r.below(60) as u32)
        }
    };
    let trk = |r: &mut Rng| {
        if r.chance(94) {
            r.pick(&p.tracks.iter().map(|x| x.id).collect::<Vec<_>>())
                .unwrap_or(TrackId(r.below(40) as u32))
        } else {
            TrackId(r.below(60) as u32)
        }
    };
    let ptrk = |r: &mut Rng| {
        if r.chance(94) {
            r.pick(&p.playlist.iter().map(|x| x.id).collect::<Vec<_>>())
                .unwrap_or(PlaylistTrackId(r.below(40) as u32))
        } else {
            PlaylistTrackId(r.below(60) as u32)
        }
    };
    let fx_of = |r: &mut Rng| -> (TrackId, InstanceId) {
        let all: Vec<(TrackId, InstanceId)> = p
            .tracks
            .iter()
            .flat_map(|t| t.inserts.iter().map(move |i| (t.id, i.instance())))
            .collect();
        r.pick(&all)
            .unwrap_or((TrackId(r.below(40) as u32), InstanceId(r.below(60) as u32)))
    };
    let some_clips = |r: &mut Rng| -> Vec<ClipId> {
        let all: Vec<ClipId> = p
            .playlist
            .iter()
            .flat_map(|t| t.clips.iter().map(|c| c.id))
            .collect();
        let n = 1 + r.below(3) as usize;
        let mut v: Vec<ClipId> = (0..n).filter_map(|_| r.pick(&all)).collect();
        if r.chance(3) {
            v.push(ClipId(r.below(1000) as u32));
        }
        v
    };
    // Step notes that exist, so lanes and toggles hit something real.
    let mut steps: Vec<(PatternId, ChannelId, u8)> = Vec::new();
    for pt in &p.patterns {
        for cn in &pt.notes {
            if let Some(ch) = p.channel(cn.channel) {
                for n in &cn.notes {
                    if n.is_step_note(ch.root_key, pt) {
                        steps.push((pt.id, cn.channel, (n.start / pt.step_ticks) as u8));
                    }
                }
            }
        }
    }
    if r.chance(12) {
        return Edit::SetStep {
            pattern: pat(r),
            channel: chan(r),
            step: r.below(16) as u8,
            on: r.chance(85),
            vel: None,
        };
    }
    match r.below(30) {
        0..=2 => {
            let (pattern, channel, step) = match r.pick(&steps) {
                Some(s) if r.chance(85) => s,
                _ => (pat(r), chan(r), r.below(18) as u8),
            };
            Edit::SetStepLanes {
                pattern,
                channel,
                step,
                vel: if r.chance(50) {
                    None
                } else {
                    Some(r.below(130) as u8)
                },
                off: if r.chance(40) {
                    None
                } else {
                    Some(r.below(54) as i8 - 27)
                },
                repeat: if r.chance(30) {
                    None
                } else {
                    Some(r.below(10) as u8)
                },
            }
        }
        3 => {
            let pt = pat(r);
            let have: Vec<NoteId> = d
                .project
                .pattern(pt)
                .map(|x| {
                    x.notes
                        .iter()
                        .flat_map(|c| c.notes.iter().map(|n| n.id))
                        .collect()
                })
                .unwrap_or_default();
            let n = r.below(3) as usize;
            Edit::SetNoteRepeat {
                pattern: pt,
                notes: (0..n).filter_map(|_| r.pick(&have)).collect(),
                repeat: [1u8, 2, 3, 4, 5, 6, 8][r.below(7) as usize],
            }
        }
        4 => Edit::SetSwing {
            pattern: pat(r),
            swing: r.below(800) as u16,
        },
        5 => Edit::SetChokeGroup {
            channel: chan(r),
            group: r.below(19) as u8,
        },
        6 | 7 => Edit::AddSample {
            sample: SampleRef {
                hash: pick_hash(r),
                orig_name: if r.chance(95) {
                    "k.wav".into()
                } else {
                    String::new()
                },
                size: r.below(1 << 20),
                local_only: r.chance(40),
            },
        },
        8 => Edit::RemoveSample { hash: pick_hash(r) },
        9 => Edit::SetSamplerSample {
            channel: chan(r),
            sample: if r.chance(20) {
                None
            } else {
                Some(pick_hash(r))
            },
        },
        10 => Edit::SetSamplerMode {
            channel: chan(r),
            mode: if r.chance(50) {
                SampleMode::OneShot
            } else {
                SampleMode::Pitched
            },
            reverse: r.chance(50),
        },
        11 => {
            let param = SamplerParam::ALL[r.below(SamplerParam::ALL.len() as u64) as usize];
            Edit::SetSamplerParam {
                channel: chan(r),
                param,
                value: in_range(r, param.range()),
            }
        }
        12 => {
            if r.chance(30) {
                Edit::SetBass808Mono {
                    channel: chan(r),
                    mono: r.chance(50),
                }
            } else {
                let param = Bass808Param::ALL[r.below(Bass808Param::ALL.len() as u64) as usize];
                Edit::SetBass808Param {
                    channel: chan(r),
                    param,
                    value: in_range(r, param.range()),
                }
            }
        }
        13 | 14 => Edit::AddChannel {
            name: "c".into(),
            instrument: if r.chance(50) {
                NewInstrument::Sampler {
                    sample: if r.chance(40) {
                        None
                    } else {
                        Some(pick_hash(r))
                    },
                    mode: if r.chance(50) {
                        SampleMode::OneShot
                    } else {
                        SampleMode::Pitched
                    },
                }
            } else {
                NewInstrument::Bass808 { mono: r.chance(50) }
            },
            root_key: r.below(128) as u8,
            track: trk(r),
        },
        15 | 16 => Edit::AddBuiltinInsert {
            track: trk(r),
            index: r.below(10) as u8,
            fx: [
                BuiltinFxKind::Eq,
                BuiltinFxKind::Compressor,
                BuiltinFxKind::Saturator,
                BuiltinFxKind::Reverb,
                BuiltinFxKind::Delay,
                BuiltinFxKind::Limiter,
            ][r.below(6) as usize],
        },
        17 => {
            let (track, instance) = fx_of(r);
            let fx_range = p
                .track(track)
                .and_then(|t| t.inserts.iter().find(|i| i.instance() == instance))
                .and_then(|i| match i {
                    Insert::Builtin { fx, .. } => Some(fx),
                    _ => None,
                });
            let param = r.below(9) as u8;
            let range = fx_range
                .and_then(|f| f.param_range(param as usize))
                .unwrap_or((0.0, 1.0));
            Edit::SetFxParam {
                track,
                instance,
                param,
                value: in_range(r, range),
            }
        }
        18 => {
            let (track, instance) = fx_of(r);
            if r.chance(50) {
                Edit::SetSaturatorCurve {
                    track,
                    instance,
                    curve: [
                        SaturatorCurve::Soft,
                        SaturatorCurve::Hard,
                        SaturatorCurve::Fold,
                    ][r.below(3) as usize],
                }
            } else {
                Edit::SetDelayPingPong {
                    track,
                    instance,
                    ping_pong: r.chance(50),
                }
            }
        }
        19 | 20 => {
            let (track, instance) = fx_of(r);
            Edit::SetSidechain {
                track,
                instance,
                source: if r.chance(25) { None } else { Some(trk(r)) },
            }
        }
        21 => {
            let (track, instance) = fx_of(r);
            Edit::MoveInsert {
                track,
                instance,
                index: r.below(9) as u8,
            }
        }
        22 | 23 => {
            if r.chance(75) {
                Edit::SetSend {
                    track: trk(r),
                    to: trk(r),
                    level_db: in_range(r, (MIN_GAIN_DB, MAX_GAIN_DB)),
                    pre_fader: r.chance(50),
                }
            } else {
                Edit::RemoveSend {
                    track: trk(r),
                    to: trk(r),
                }
            }
        }
        24 => match r.below(4) {
            0 | 1 => Edit::AddPlaylistTrack {
                name: if r.chance(95) {
                    "pl".into()
                } else {
                    String::new()
                },
            },
            2 => Edit::RemovePlaylistTrack { track: ptrk(r) },
            _ => Edit::RenamePlaylistTrack {
                track: ptrk(r),
                name: format!("r{}", r.below(99)),
            },
        },
        25..=27 => Edit::AddClip {
            track: ptrk(r),
            pattern: pat(r),
            start: r.below(40) as u32 * 960,
            len: if r.chance(95) {
                960 * (1 + r.below(4)) as u32
            } else {
                0
            },
        },
        28 => Edit::RemoveClips {
            clips: some_clips(r),
        },
        _ => {
            if r.chance(50) {
                Edit::MoveClips {
                    clips: some_clips(r),
                    dt: r.below(4000) as i64 - 1500,
                    dtrack: r.below(5) as i32 - 2,
                }
            } else {
                Edit::ResizeClips {
                    clips: some_clips(r),
                    dlen: r.below(3000) as i64 - 1500,
                }
            }
        }
    }
}

#[test]
fn milestone_b_random_sequences_stay_valid_unique_and_round_trip() {
    let mut accepted_kinds: HashSet<String> = HashSet::new();
    for seed in 1..=60u64 {
        let mut r = Rng(0xC0FF_EE11_5EED_0001 ^ seed.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        let mut d = Document::new();
        let mut seen: HashSet<u32> = HashSet::new();
        for step in 0..500 {
            let e = if r.chance(55) {
                random_edit_b(&mut r, &d)
            } else {
                super::tests::random_edit(&mut r, &d)
            };
            let Ok((nd, created)) = apply(&d, &e) else {
                continue;
            };
            validate(&nd.project).unwrap_or_else(|err| {
                panic!("seed {seed} step {step}: invalid after {e:?}: {err}")
            });
            for id in created {
                assert!(seen.insert(id), "seed {seed}: id {id} repeated");
                assert!(id < nd.next_id);
            }
            assert!(nd.next_id > nd.project.max_id());
            // Canonical order is what the emitter writes: re-sorting is a no-op.
            let mut again = (*nd.project).clone();
            sort_canonical(&mut again);
            assert_eq!(
                again, *nd.project,
                "seed {seed} step {step}: {e:?} left it unsorted"
            );
            if step % 25 == 0 {
                assert_roundtrip(&nd);
            }
            accepted_kinds.insert(
                format!("{e:?}")
                    .split([' ', '{'])
                    .next()
                    .unwrap()
                    .to_string(),
            );
            d = nd;
        }
        assert_roundtrip(&d);
    }
    // The generator really exercises every new edit.
    for k in [
        "SetStepLanes",
        "SetNoteRepeat",
        "SetSwing",
        "SetChokeGroup",
        "AddSample",
        "RemoveSample",
        "SetSamplerSample",
        "SetSamplerMode",
        "SetSamplerParam",
        "SetBass808Mono",
        "SetBass808Param",
        "AddBuiltinInsert",
        "SetFxParam",
        "SetSaturatorCurve",
        "SetDelayPingPong",
        "SetSidechain",
        "MoveInsert",
        "SetSend",
        "RemoveSend",
        "AddPlaylistTrack",
        "RemovePlaylistTrack",
        "RenamePlaylistTrack",
        "AddClip",
        "RemoveClips",
        "MoveClips",
        "ResizeClips",
    ] {
        assert!(
            accepted_kinds.contains(k),
            "{k} was never accepted: {accepted_kinds:?}"
        );
    }
}
