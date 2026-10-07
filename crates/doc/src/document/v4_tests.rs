// SPDX-License-Identifier: GPL-3.0-or-later
//! Tests for audio clips, patterns on the timeline, shapes and insert
//! bypass (SPEC 20.7, 21.1, 24.2-1).

use std::collections::HashSet;

use super::beats_tests::{random_clip_edit, random_edit_b};
use super::tests::{Rng, ok, random_edit, synth};
use super::*;
use protocol::beats::BuiltinFxKind;
use protocol::ids::{ClipId, GroupId, ShapeId};
use protocol::model::{Curve, SampleHash, SampleRef, ShapePoint};

const BAR: u32 = 3840;

fn sample_hash() -> SampleHash {
    SampleHash([9; 32])
}

fn audio_doc() -> (Document, ChannelId) {
    let d = Document::new();
    let (d, _) = ok(
        &d,
        Edit::AddSample {
            sample: SampleRef {
                hash: sample_hash().to_hex(),
                orig_name: "loop.wav".into(),
                size: 100,
                local_only: false,
            },
        },
    );
    let (d, c) = ok(
        &d,
        Edit::AddChannel {
            name: "Vocal".into(),
            instrument: NewInstrument::Audio,
            root_key: 60,
            track: TrackId::MASTER,
        },
    );
    (d, ChannelId(c[0]))
}

fn add_audio(d: &Document, row: ChannelId, start: u32, len: u32, offset: u32) -> (Document, ClipId) {
    let (d, c) = ok(
        d,
        Edit::AddAudioClip {
            instrument: row,
            sample: sample_hash(),
            start,
            len,
            offset,
        },
    );
    (d, ClipId(c[0]))
}

fn clip(d: &Document, id: ClipId) -> protocol::model::Clip {
    *d.project.clips.iter().find(|c| c.id == id).expect("clip")
}

fn saves(d: &Document) {
    let text = protocol::format::emit(&d.project, d.next_id).expect("emits");
    let (back, _) = protocol::format::parse(&text).expect("parses");
    assert_eq!(back, *d.project);
}

fn note_row(d: &Document, name: &str) -> (Document, ChannelId) {
    let (d, c) = ok(
        d,
        Edit::AddChannel {
            name: name.into(),
            instrument: synth(),
            root_key: 36,
            track: TrackId::MASTER,
        },
    );
    (d, ChannelId(c[0]))
}

fn note_clip(d: &Document, row: ChannelId, start: u32, len: u32) -> (Document, ClipId) {
    let (d, ids) = ok(
        d,
        Edit::AddClip {
            instrument: row,
            pattern: None,
            start,
            len,
        },
    );
    (d, ClipId(ids[1]))
}

#[test]
fn audio_clips_add_edit_move_resize_split_and_remove() {
    let (d, row) = audio_doc();
    let (d, c) = add_audio(&d, row, BAR, 2 * BAR, 100);
    let k = clip(&d, c);
    assert_eq!((k.pattern, k.offset), (PatternId::NONE, 100));
    assert_eq!(k.audio.unwrap().gain_mdb, 0);
    saves(&d);

    let (d, _) = ok(
        &d,
        Edit::SetClipAudio {
            clip: c,
            gain_mdb: -6000,
            fade_in: 100,
            fade_out: 200,
        },
    );
    let (d, _) = ok(&d, Edit::MoveClips { clips: vec![c], dt: 960 });
    assert_eq!(clip(&d, c).start, BAR + 960);

    // Growing at the start uncovers earlier audio: offset goes down.
    let (d, _) = ok(
        &d,
        Edit::ResizeClips {
            clips: vec![c],
            dlen: 60,
            from_start: true,
        },
    );
    let k = clip(&d, c);
    assert_eq!((k.start, k.len, k.offset), (BAR + 900, 2 * BAR + 60, 40));
    // Not past the start of the sample.
    assert!(
        apply(
            &d,
            &Edit::ResizeClips {
                clips: vec![c],
                dlen: 100,
                from_start: true
            }
        )
        .is_err()
    );

    // Split keeps the source and moves the offset.
    let at = k.start + 1000;
    let (d, created) = ok(&d, Edit::SplitClip { clip: c, at });
    let right = clip(&d, ClipId(created[0]));
    let left = clip(&d, c);
    assert_eq!(left.len, 1000);
    assert_eq!(right.offset, 40 + 1000);
    assert_eq!(right.audio.unwrap().sample, sample_hash());
    assert_eq!((left.audio.unwrap().fade_out, right.audio.unwrap().fade_in), (0, 0));
    assert_eq!(right.audio.unwrap().fade_out, 200);
    assert_eq!(left.audio.unwrap().gain_mdb, -6000);
    saves(&d);

    // Duplicate (copy or linked) keeps the source.
    let (d, dup) = ok(
        &d,
        Edit::DuplicateClips {
            clips: vec![ClipId(created[0])],
            dt: 10 * BAR as i64,
            linked: false,
        },
    );
    assert_eq!(clip(&d, ClipId(dup[0])).audio, right.audio);
    assert_eq!(d.project.patterns.len(), 0);
    let (d, _) = ok(&d, Edit::RemoveClips { clips: vec![c, ClipId(created[0])] });
    assert_eq!(d.project.clips.len(), 1);
    saves(&d);
}

#[test]
fn audio_clips_need_an_audio_row_a_known_sample_and_room() {
    let (d, row) = audio_doc();
    let (d, k) = note_row(&d, "Kick");
    let add = |instrument, sample| Edit::AddAudioClip {
        instrument,
        sample,
        start: 0,
        len: BAR,
        offset: 0,
    };
    assert!(apply(&d, &add(k, sample_hash())).is_err());
    assert!(apply(&d, &add(row, SampleHash([1; 32]))).is_err());
    let (d, _) = add_audio(&d, row, 0, BAR, 0);
    assert!(apply(&d, &add(row, sample_hash())).is_err(), "overlap");
    // A note clip cannot go on an audio row, nor an audio clip on a note row.
    assert!(
        apply(
            &d,
            &Edit::AddClip {
                instrument: row,
                pattern: None,
                start: 4 * BAR,
                len: BAR
            }
        )
        .is_err()
    );
    let c = d.project.clips[0].id;
    assert!(apply(&d, &Edit::MoveClipToInstrument { clip: c, instrument: k }).is_err());
    assert!(
        apply(
            &d,
            &Edit::SetClipAudio {
                clip: c,
                gain_mdb: 0,
                fade_in: 2 * BAR,
                fade_out: 0
            }
        )
        .is_err()
    );
}

#[test]
fn make_pattern_groups_clips_at_the_earliest_start() {
    let d = Document::new();
    let (d, kick) = note_row(&d, "Kick");
    let (d, snare) = note_row(&d, "Snare");
    let (d, a) = note_clip(&d, kick, BAR, BAR);
    let (d, b) = note_clip(&d, snare, 2 * BAR, BAR);
    let (d, created) = ok(
        &d,
        Edit::MakePattern {
            clips: vec![a, b],
            name: "Beat A".into(),
        },
    );
    let gid = GroupId(created[0]);
    assert_eq!(d.project.groups.len(), 1);
    assert_eq!(d.project.groups[0].name, "Beat A");
    for id in [a, b] {
        let c = clip(&d, id);
        assert_eq!(c.start, BAR);
        assert_eq!(c.group.unwrap().group, gid);
        assert_eq!(c.group.unwrap().instance, 1);
    }
    saves(&d);
    // One clip per row, and not twice.
    assert!(
        apply(
            &d,
            &Edit::MakePattern {
                clips: vec![a],
                name: "Again".into()
            }
        )
        .is_err()
    );

    // Place another instance: linked copies, a new instance number.
    let (d2, placed) = ok(&d, Edit::PlacePattern { group: gid, start: 4 * BAR });
    assert_eq!(placed.len(), 2);
    for id in &placed {
        let c = clip(&d2, ClipId(*id));
        assert_eq!(c.start, 4 * BAR);
        assert_eq!(c.group.unwrap().instance, 2);
    }
    let (ka, kb) = (clip(&d2, a), clip(&d2, ClipId(placed[0])));
    assert_eq!(ka.pattern, kb.pattern, "linked content");
    saves(&d2);
    // Overlap is refused.
    assert!(apply(&d, &Edit::PlacePattern { group: gid, start: BAR }).is_err());
    assert!(apply(&d, &Edit::PlacePattern { group: GroupId(9999), start: 0 }).is_err());

    // Duplicating grouped clips makes a new instance.
    let (d3, dup) = ok(
        &d2,
        Edit::DuplicateClips {
            clips: vec![a, b],
            dt: 8 * BAR as i64,
            linked: true,
        },
    );
    let inst: HashSet<u32> = dup
        .iter()
        .map(|id| clip(&d3, ClipId(*id)).group.unwrap().instance)
        .collect();
    assert_eq!(inst, HashSet::from([3]));

    // Rename, ungroup, and an empty pattern goes away.
    let (d4, _) = ok(
        &d,
        Edit::RenameGroup {
            group: gid,
            name: "Verse".into(),
        },
    );
    assert_eq!(d4.project.groups[0].name, "Verse");
    let (d5, _) = ok(&d4, Edit::Ungroup { clips: vec![a, b] });
    assert!(d5.project.groups.is_empty());
    assert!(clip(&d5, a).group.is_none());
    let (d6, _) = ok(&d, Edit::RemoveClips { clips: vec![a, b] });
    assert!(d6.project.groups.is_empty());
}

#[test]
fn shapes_add_replace_remove_and_follow_their_targets() {
    let d = Document::new();
    let (d, tr) = ok(&d, Edit::AddTrack { name: "Drums".into() });
    let tr = TrackId(tr[0]);
    let pt = |tick, value| ShapePoint {
        tick,
        value,
        curve: Curve::Linear,
    };
    let (d, s) = ok(
        &d,
        Edit::AddShape {
            target: protocol::model::ShapeTarget::Volume { track: tr },
            points: vec![pt(960, 0.0), pt(0, -12.0)],
        },
    );
    let sid = ShapeId(s[0]);
    assert_eq!(d.project.shapes[0].points[0].tick, 0, "sorted");
    saves(&d);
    // Out of range and missing targets are refused.
    assert!(
        apply(
            &d,
            &Edit::SetShapePoints {
                shape: sid,
                points: vec![pt(0, 40.0)]
            }
        )
        .is_err()
    );
    assert!(
        apply(
            &d,
            &Edit::AddShape {
                target: protocol::model::ShapeTarget::Pan { track: TrackId(777) },
                points: vec![]
            }
        )
        .is_err()
    );
    let (d2, _) = ok(
        &d,
        Edit::SetShapePoints {
            shape: sid,
            points: vec![pt(0, -3.0)],
        },
    );
    assert_eq!(d2.project.shapes[0].points.len(), 1);
    // Removing the track removes its shapes.
    let (d3, _) = ok(&d2, Edit::RemoveTrack { track: tr });
    assert!(d3.project.shapes.is_empty());
    let (d4, _) = ok(&d2, Edit::RemoveShape { shape: sid });
    assert!(d4.project.shapes.is_empty());
    assert!(apply(&d4, &Edit::RemoveShape { shape: sid }).is_err());
}

#[test]
fn insert_bypass_toggles_and_effect_shapes_follow_the_insert() {
    let d = Document::new();
    let (d, ids) = ok(
        &d,
        Edit::AddBuiltinInsert {
            track: TrackId::MASTER,
            index: 0,
            fx: BuiltinFxKind::Reverb,
        },
    );
    let inst = InstanceId(ids[0]);
    let set = |d: &Document, bypass| {
        ok(
            d,
            Edit::SetInsertBypass {
                track: TrackId::MASTER,
                instance: inst,
                bypass,
            },
        )
        .0
    };
    let d = set(&d, true);
    let bypassed = |d: &Document| {
        matches!(
            d.project.tracks[0].inserts[0],
            protocol::model::Insert::Builtin { bypass: true, .. }
        )
    };
    assert!(bypassed(&d));
    saves(&d);
    let (d, _) = ok(
        &d,
        Edit::AddShape {
            target: protocol::model::ShapeTarget::FxParam {
                track: TrackId::MASTER,
                instance: inst,
                param: 0,
            },
            points: vec![],
        },
    );
    let d2 = set(&d, false);
    assert!(!bypassed(&d2));
    let (d3, _) = ok(
        &d,
        Edit::RemoveInsert {
            track: TrackId::MASTER,
            instance: inst,
        },
    );
    assert!(d3.project.shapes.is_empty());
    assert!(
        apply(
            &d,
            &Edit::SetInsertBypass {
                track: TrackId::MASTER,
                instance: InstanceId(4242),
                bypass: true
            }
        )
        .is_err()
    );
}

#[test]
fn diff_describes_v4_changes_in_plain_words() {
    let (d, row) = audio_doc();
    let (d2, _) = add_audio(&d, row, 0, BAR, 0);
    let lines = crate::diff::diff_projects(&d.project, &d2.project);
    assert!(lines.iter().any(|l| l.contains("audio clip added")), "{lines:?}");
}

/// Random edits of the v4 kinds, on top of the usual ones.
fn random_v4_edit(r: &mut Rng, d: &Document) -> Edit {
    let p = &d.project;
    let rows: Vec<ChannelId> = p.channels.iter().map(|c| c.id).collect();
    let clips: Vec<ClipId> = p.clips.iter().map(|c| c.id).collect();
    let pick_clips = |r: &mut Rng| -> Vec<ClipId> {
        (0..1 + r.below(3)).filter_map(|_| r.pick(&clips)).collect()
    };
    match r.below(12) {
        0 | 1 => Edit::AddAudioClip {
            instrument: r.pick(&rows).unwrap_or(ChannelId(1)),
            sample: sample_hash(),
            start: r.below(24) as u32 * 960,
            len: 960 * (1 + r.below(4)) as u32,
            offset: r.below(2000) as u32,
        },
        2 => Edit::SetClipAudio {
            clip: r.pick(&clips).unwrap_or(ClipId(1)),
            gain_mdb: r.below(30_000) as i32 - 20_000,
            fade_in: r.below(1500) as u32,
            fade_out: r.below(1500) as u32,
        },
        3 | 4 => Edit::MakePattern {
            clips: pick_clips(r),
            name: "Pat".into(),
        },
        5 | 6 => Edit::PlacePattern {
            group: r
                .pick(&p.groups.iter().map(|g| g.id).collect::<Vec<_>>())
                .unwrap_or(GroupId(1)),
            start: r.below(40) as u32 * 960,
        },
        7 => Edit::Ungroup {
            clips: pick_clips(r),
        },
        8 => Edit::AddShape {
            target: protocol::model::ShapeTarget::Pan { track: TrackId::MASTER },
            points: (0..r.below(4))
                .map(|i| ShapePoint {
                    tick: (r.below(4) as u32) * 100 + i as u32 * 1000,
                    value: (r.below(200) as f32 - 100.0) / 100.0,
                    curve: Curve::Smooth,
                })
                .collect(),
        },
        9 => Edit::RemoveShape {
            shape: r
                .pick(&p.shapes.iter().map(|s| s.id).collect::<Vec<_>>())
                .unwrap_or(ShapeId(1)),
        },
        10 => Edit::RenameGroup {
            group: r
                .pick(&p.groups.iter().map(|g| g.id).collect::<Vec<_>>())
                .unwrap_or(GroupId(1)),
            name: "Renamed".into(),
        },
        _ => Edit::SetInsertBypass {
            track: TrackId::MASTER,
            instance: InstanceId(r.below(60) as u32),
            bypass: r.chance(50),
        },
    }
}

#[test]
fn random_v4_sequences_stay_valid_unique_and_round_trip() {
    let mut accepted = HashSet::new();
    for seed in 1..=60u64 {
        let mut r = Rng(0x4444_AAAA_0000_0001 ^ seed.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        let (mut d, _) = audio_doc();
        let (d0, _) = note_row(&d, "Kick");
        d = ok(
            &d0,
            Edit::AddBuiltinInsert {
                track: TrackId::MASTER,
                index: 0,
                fx: BuiltinFxKind::Delay,
            },
        )
        .0;
        let mut seen: HashSet<u32> = HashSet::new();
        for i in 0..300 {
            let e = match r.below(10) {
                0..=4 => random_v4_edit(&mut r, &d),
                5 | 6 => random_clip_edit(&mut r, &d),
                7 => random_edit_b(&mut r, &d),
                _ => random_edit(&mut r, &d),
            };
            let Ok((nd, created)) = apply(&d, &e) else {
                continue;
            };
            validate(&nd.project)
                .unwrap_or_else(|err| panic!("seed {seed} step {i}: {e:?} left it invalid: {err}"));
            for id in &created {
                assert!(seen.insert(*id), "id {id} handed out twice ({e:?})");
            }
            // Every grouped clip names an existing pattern.
            for c in &nd.project.clips {
                if let Some(g) = c.group {
                    assert!(nd.project.groups.iter().any(|x| x.id == g.group));
                }
            }
            if i % 15 == 0 {
                saves(&nd);
            }
            accepted.insert(
                format!("{e:?}")
                    .split([' ', '{'])
                    .next()
                    .unwrap()
                    .to_string(),
            );
            d = nd;
        }
        saves(&d);
    }
    for k in [
        "AddAudioClip",
        "SetClipAudio",
        "MakePattern",
        "PlacePattern",
        "Ungroup",
        "AddShape",
        "RemoveShape",
        "SetInsertBypass",
    ] {
        assert!(accepted.contains(k), "{k} never accepted: {accepted:?}");
    }
}
