// SPDX-License-Identifier: GPL-3.0-or-later
//! Tests for the timeline edits (SPEC 20.2, 20.3).

use super::tests::{base, ok, synth};
use super::*;
use protocol::ids::ClipId;
use protocol::model::Clip;

const BAR: u32 = 3840;

fn fails(d: &Document, e: Edit) -> EditError {
    apply(d, &e).expect_err(&format!("should fail: {e:?}"))
}

fn add_channel(d: &Document, name: &str, root_key: u8) -> (Document, ChannelId) {
    let (d, c) = ok(
        d,
        Edit::AddChannel {
            name: name.into(),
            instrument: synth(),
            root_key,
            track: TrackId::MASTER,
        },
    );
    (d, ChannelId(c[0]))
}

/// A clip with new content for an instrument. Returns (doc, content, clip).
fn new_clip(d: &Document, inst: ChannelId, start: u32, len: u32) -> (Document, PatternId, ClipId) {
    let (d, ids) = ok(
        d,
        Edit::AddClip {
            instrument: inst,
            pattern: None,
            start,
            len,
        },
    );
    (d, PatternId(ids[0]), ClipId(ids[1]))
}

fn clip(d: &Document, id: ClipId) -> Clip {
    *d.project.clips.iter().find(|c| c.id == id).expect("clip")
}

fn step(d: &Document, p: PatternId, s: u8) -> Document {
    ok(
        d,
        Edit::SetStep {
            pattern: p,
            step: s,
            on: true,
            vel: None,
        },
    )
    .0
}

fn assert_saves(d: &Document) {
    validate(&d.project).expect("validates");
    let text = protocol::format::emit(&d.project, d.next_id).expect("emits");
    let (back, _) = protocol::format::parse(&text).expect("parses");
    assert_eq!(back, *d.project);
}

#[test]
fn add_clip_without_content_makes_one_bar_of_sixteen_steps() {
    let (d, kick) = add_channel(&Document::new(), "Kick", 36);
    let (d, content, c) = new_clip(&d, kick, 0, 2 * BAR);
    let pat = d.project.pattern(content).unwrap();
    assert_eq!(pat.name, "Kick 1");
    assert_eq!((pat.length_steps, pat.length_ticks()), (16, BAR));
    assert_eq!(pat.instrument, kick);
    let k = clip(&d, c);
    assert_eq!((k.start, k.len, k.offset, k.muted), (0, 2 * BAR, 0, false));
    assert_eq!(k.pattern, content);
    // Created: content id first, then the clip id.
    assert!(content.0 < c.0);
    // The second content gets the next free number; other instruments
    // count on their own.
    let (d, snare) = add_channel(&d, "Snare", 38);
    let (d, p2, _) = new_clip(&d, kick, 4 * BAR, BAR);
    let (d, p3, _) = new_clip(&d, snare, 0, BAR);
    assert_eq!(d.project.pattern(p2).unwrap().name, "Kick 2");
    assert_eq!(d.project.pattern(p3).unwrap().name, "Snare 1");
    assert_saves(&d);
}

#[test]
fn add_clip_names_reuse_the_smallest_free_number() {
    let (d, kick) = add_channel(&Document::new(), "Kick", 36);
    let (d, p1, c1) = new_clip(&d, kick, 0, BAR);
    let (d, _, _) = new_clip(&d, kick, BAR, BAR);
    let (d, _) = ok(&d, Edit::RemoveClips { clips: vec![c1] });
    assert!(d.project.pattern(p1).is_none());
    let (d, p3, _) = new_clip(&d, kick, 2 * BAR, BAR);
    assert_eq!(d.project.pattern(p3).unwrap().name, "Kick 1");
}

#[test]
fn add_clip_with_content_needs_the_same_instrument() {
    let (d, c, p) = base();
    let (d, other) = add_channel(&d, "Other", 40);
    let (d, ids) = ok(
        &d,
        Edit::AddClip {
            instrument: c,
            pattern: Some(p),
            start: 0,
            len: BAR,
        },
    );
    assert_eq!(ids.len(), 1);
    assert_eq!(clip(&d, ClipId(ids[0])).pattern, p);
    assert!(matches!(
        fails(
            &d,
            Edit::AddClip {
                instrument: other,
                pattern: Some(p),
                start: 0,
                len: BAR,
            }
        ),
        EditError::BadArgument { .. }
    ));
    for (inst, start, len) in [(ChannelId(999), 0, BAR), (c, 10 * BAR, 0), (c, u32::MAX, 5)] {
        assert!(
            apply(
                &d,
                &Edit::AddClip {
                    instrument: inst,
                    pattern: None,
                    start,
                    len
                }
            )
            .is_err()
        );
    }
}

#[test]
fn overlap_on_a_row_names_the_clips() {
    let (d, c, _) = base();
    let (d, _, a) = new_clip(&d, c, BAR, BAR);
    for (start, len) in [(0, BAR + 1), (BAR, 1), (2 * BAR - 1, 10), (0, 3 * BAR)] {
        match fails(
            &d,
            Edit::AddClip {
                instrument: c,
                pattern: None,
                start,
                len,
            },
        ) {
            EditError::Invalid {
                reason: ValidationError::Overlap { a: new, b },
            } => {
                assert_eq!((new, b), (0, a.0), "0 is the clip being added");
            }
            e => panic!("{e:?}"),
        }
    }
    // Touching is fine, and another row is independent.
    let (d, _, _) = new_clip(&d, c, 2 * BAR, BAR);
    let (d, o) = add_channel(&d, "O", 50);
    let (d, _, _) = new_clip(&d, o, BAR, BAR);
    assert_saves(&d);
}

#[test]
fn duplicate_linked_shares_content_and_copy_does_not() {
    let (d, c, _) = base();
    let (d, p, a) = new_clip(&d, c, 0, BAR);
    let d = step(&d, p, 0);
    let (d1, ids) = ok(
        &d,
        Edit::DuplicateClips {
            clips: vec![a],
            dt: BAR as i64,
            linked: true,
        },
    );
    assert_eq!(ids.len(), 1, "no new content for a linked copy");
    let b = clip(&d1, ClipId(ids[0]));
    assert_eq!((b.pattern, b.start, b.len), (p, BAR, BAR));
    assert_eq!(d1.project.patterns.len(), d.project.patterns.len());

    let (d2, ids) = ok(
        &d,
        Edit::DuplicateClips {
            clips: vec![a],
            dt: BAR as i64,
            linked: false,
        },
    );
    assert_eq!(ids.len(), 2, "content, then clip");
    let b = clip(&d2, ClipId(ids[1]));
    assert_eq!(b.pattern.0, ids[0]);
    assert_ne!(b.pattern, p);
    let (orig, copy) = (
        d2.project.pattern(p).unwrap(),
        d2.project.pattern(b.pattern).unwrap(),
    );
    assert_eq!(copy.name, "Lead 1 copy");
    assert_eq!(orig.notes.len(), copy.notes.len());
    assert_ne!(orig.notes[0].id, copy.notes[0].id);
    assert_eq!(
        (orig.notes[0].start, orig.notes[0].key),
        (copy.notes[0].start, copy.notes[0].key)
    );
    assert_saves(&d2);

    // Overlap and negative targets fail.
    for dt in [0i64, 100, -(BAR as i64)] {
        assert!(
            apply(
                &d,
                &Edit::DuplicateClips {
                    clips: vec![a],
                    dt,
                    linked: true
                }
            )
            .is_err(),
            "{dt}"
        );
    }
    assert!(matches!(
        fails(
            &d,
            Edit::DuplicateClips {
                clips: vec![ClipId(999)],
                dt: 1,
                linked: true
            }
        ),
        EditError::NotFound { .. }
    ));
}

#[test]
fn copying_linked_clips_keeps_the_copies_linked() {
    let (d, c, _) = base();
    let (d, p, a) = new_clip(&d, c, 0, BAR);
    let (d, ids) = ok(
        &d,
        Edit::DuplicateClips {
            clips: vec![a],
            dt: BAR as i64,
            linked: true,
        },
    );
    let b = ClipId(ids[0]);
    let (d, ids) = ok(
        &d,
        Edit::DuplicateClips {
            clips: vec![a, b],
            dt: 2 * BAR as i64,
            linked: false,
        },
    );
    assert_eq!(ids.len(), 3, "one content, two clips");
    let (x, y) = (clip(&d, ClipId(ids[1])), clip(&d, ClipId(ids[2])));
    assert_eq!(x.pattern, y.pattern);
    assert_ne!(x.pattern, p);
    assert_eq!((x.start, y.start), (2 * BAR, 3 * BAR));
}

#[test]
fn remove_clips_collects_content_nobody_uses() {
    let (d, c, p) = base();
    // Content made by AddPattern and never placed stays.
    let (d, ids) = ok(
        &d,
        Edit::AddClip {
            instrument: c,
            pattern: Some(p),
            start: 0,
            len: BAR,
        },
    );
    let a = ClipId(ids[0]);
    let (d, ids) = ok(
        &d,
        Edit::DuplicateClips {
            clips: vec![a],
            dt: BAR as i64,
            linked: true,
        },
    );
    let b = ClipId(ids[0]);
    let (d_a, _) = ok(&d, Edit::RemoveClips { clips: vec![a] });
    assert!(d_a.project.pattern(p).is_some(), "b still plays it");
    let (d_ab, _) = ok(
        &d,
        Edit::RemoveClips {
            clips: vec![a, b, a],
        },
    );
    assert!(d_ab.project.pattern(p).is_none());
    assert!(d_ab.project.clips.is_empty());
    assert_saves(&d_ab);
    // Unplaced content is left alone.
    let (d, q) = ok(
        &d_ab,
        Edit::AddPattern {
            instrument: c,
            name: "Spare".into(),
            length_steps: 8,
        },
    );
    let (d, ids) = ok(
        &d,
        Edit::AddClip {
            instrument: c,
            pattern: None,
            start: 0,
            len: BAR,
        },
    );
    let (d, _) = ok(
        &d,
        Edit::RemoveClips {
            clips: vec![ClipId(ids[1])],
        },
    );
    assert!(d.project.pattern(PatternId(q[0])).is_some());
    assert!(matches!(
        fails(
            &d,
            Edit::RemoveClips {
                clips: vec![ClipId(7777)]
            }
        ),
        EditError::NotFound { .. }
    ));
}

#[test]
fn move_clips_checks_overlap_range_and_moves_together() {
    let (d, c, _) = base();
    let (d, _, a) = new_clip(&d, c, 0, BAR);
    let (d, _, b) = new_clip(&d, c, BAR, BAR);
    let (d, _, z) = new_clip(&d, c, 5 * BAR, BAR);
    // Moving both by one bar is fine: they only touch each other's old place.
    let (m, _) = ok(
        &d,
        Edit::MoveClips {
            clips: vec![a, b],
            dt: BAR as i64,
        },
    );
    assert_eq!((clip(&m, a).start, clip(&m, b).start), (BAR, 2 * BAR));
    assert_saves(&m);
    // One alone would hit the other.
    assert!(matches!(
        fails(
            &d,
            Edit::MoveClips {
                clips: vec![a],
                dt: BAR as i64
            }
        ),
        EditError::Invalid {
            reason: ValidationError::Overlap { .. }
        }
    ));
    // Out of range and failed batches change nothing.
    for dt in [-1i64, i64::MIN, i64::MAX, MAX_TICK as i64] {
        assert!(
            apply(
                &d,
                &Edit::MoveClips {
                    clips: vec![a, z],
                    dt
                }
            )
            .is_err(),
            "{dt}"
        );
    }
    assert_eq!(clip(&d, z).start, 5 * BAR);
}

#[test]
fn move_to_instrument_copies_content_and_rewrites_step_keys() {
    let (d, kick) = add_channel(&Document::new(), "Kick", 36);
    let (d, snare) = add_channel(&d, "Snare", 38);
    let (d, p, a) = new_clip(&d, kick, BAR, BAR);
    let d = step(&d, p, 0);
    let d = step(&d, p, 4);
    // A pitch offset on a step note stays, and a piano-roll note keeps its key.
    let (d, _) = ok(
        &d,
        Edit::SetStepLanes {
            pattern: p,
            step: 4,
            vel: None,
            off: Some(3),
            repeat: None,
        },
    );
    let (d, _) = ok(
        &d,
        Edit::AddNotes {
            pattern: p,
            notes: vec![NewNote {
                start: 100,
                len: 50,
                key: 90,
                vel: 80,
            }],
        },
    );
    // A second clip shares the content, so the old content stays.
    let (d, ids) = ok(
        &d,
        Edit::DuplicateClips {
            clips: vec![a],
            dt: BAR as i64,
            linked: true,
        },
    );
    let b = ClipId(ids[0]);
    let (m, ids) = ok(
        &d,
        Edit::MoveClipToInstrument {
            clip: a,
            instrument: snare,
        },
    );
    assert_eq!(ids.len(), 1, "one new content");
    let k = clip(&m, a);
    assert_eq!((k.instrument, k.start, k.len), (snare, BAR, BAR));
    assert_ne!(k.pattern, p);
    assert_eq!(clip(&m, b).pattern, p, "the linked clip is unaffected");
    let new = m.project.pattern(k.pattern).unwrap();
    assert_eq!(new.instrument, snare);
    assert_eq!(new.name, "Snare 1");
    let keys: Vec<(u32, u8, i8)> = new.notes.iter().map(|n| (n.start, n.key, n.off)).collect();
    assert_eq!(keys, vec![(0, 38, 0), (100, 90, 0), (960, 41, 3)]);
    let old = m.project.pattern(p).unwrap();
    assert_eq!(old.notes.iter().filter(|n| n.key == 36).count(), 1);
    assert_saves(&m);

    // Alone on its content: the old content goes away.
    let (m2, _) = ok(&m, Edit::RemoveClips { clips: vec![b] });
    assert!(m2.project.pattern(p).is_none());
    // Same instrument is a no-op; an occupied destination fails.
    let (same, ids) = ok(
        &m,
        Edit::MoveClipToInstrument {
            clip: a,
            instrument: snare,
        },
    );
    assert!(ids.is_empty());
    assert_eq!(same.project, m.project);
    let (m3, _, _) = new_clip(&m, snare, 2 * BAR, BAR);
    assert!(matches!(
        fails(
            &m3,
            Edit::MoveClipToInstrument {
                clip: b,
                instrument: snare
            }
        ),
        EditError::Invalid { .. }
    ));
}

#[test]
fn move_to_instrument_fails_if_a_key_would_leave_the_range() {
    let (d, low) = add_channel(&Document::new(), "Low", 120);
    let (d, hi) = add_channel(&d, "Hi", 5);
    let (d, p, a) = new_clip(&d, low, 0, BAR);
    let d = step(&d, p, 0);
    let (d, _) = ok(
        &d,
        Edit::SetStepLanes {
            pattern: p,
            step: 0,
            vel: None,
            off: Some(7),
            repeat: None,
        },
    );
    // 120 + 7 = 127 is the highest legal key; moving to root 5 is fine.
    assert!(
        apply(
            &d,
            &Edit::MoveClipToInstrument {
                clip: a,
                instrument: hi
            }
        )
        .is_ok()
    );
    let (d, hi2) = add_channel(&d, "Hi2", 125);
    assert!(
        apply(
            &d,
            &Edit::MoveClipToInstrument {
                clip: a,
                instrument: hi2
            }
        )
        .is_err()
    );
}

#[test]
fn resize_from_the_end_and_from_the_start() {
    let (d, c, _) = base();
    let (d, _, a) = new_clip(&d, c, BAR, BAR);
    let (d, _, other) = new_clip(&d, c, 4 * BAR, BAR);
    let (e, _) = ok(
        &d,
        Edit::ResizeClips {
            clips: vec![a],
            dlen: BAR as i64,
            from_start: false,
        },
    );
    let k = clip(&e, a);
    assert_eq!((k.start, k.len, k.offset), (BAR, 2 * BAR, 0));

    // Growing at the start moves start earlier and wraps offset backwards,
    // so the same content stays under the same tick.
    let (s, _) = ok(
        &d,
        Edit::ResizeClips {
            clips: vec![a],
            dlen: 960,
            from_start: true,
        },
    );
    let k = clip(&s, a);
    assert_eq!(
        (k.start, k.len, k.offset),
        (BAR - 960, BAR + 960, BAR - 960)
    );
    // Shrinking at the start moves start later and offset forward.
    let (s2, _) = ok(
        &s,
        Edit::ResizeClips {
            clips: vec![a],
            dlen: -960,
            from_start: true,
        },
    );
    let k = clip(&s2, a);
    assert_eq!((k.start, k.len, k.offset), (BAR, BAR, 0));
    let (s3, _) = ok(
        &d,
        Edit::ResizeClips {
            clips: vec![a],
            dlen: -960,
            from_start: true,
        },
    );
    let k = clip(&s3, a);
    assert_eq!((k.start, k.len, k.offset), (BAR + 960, BAR - 960, 960));
    assert_saves(&s3);

    // Failures: empty clip, before zero, into a neighbour, past the range.
    for (dlen, from_start) in [
        (-(BAR as i64), false),
        (-(BAR as i64), true),
        (BAR as i64 + 1, true),
        (3 * BAR as i64, false),
        (i64::MAX, false),
        (i64::MIN, true),
    ] {
        assert!(
            apply(
                &d,
                &Edit::ResizeClips {
                    clips: vec![a],
                    dlen,
                    from_start
                }
            )
            .is_err(),
            "{dlen} {from_start}"
        );
    }
    assert_eq!(clip(&d, other).start, 4 * BAR);
}

#[test]
fn split_makes_two_clips_with_the_right_offset() {
    let (d, c, _) = base();
    let (d, p, a) = new_clip(&d, c, BAR, 3 * BAR);
    let (s, ids) = ok(
        &d,
        Edit::SplitClip {
            clip: a,
            at: BAR + 1000,
        },
    );
    assert_eq!(ids.len(), 1);
    let (l, r) = (clip(&s, a), clip(&s, ClipId(ids[0])));
    assert_eq!((l.start, l.len, l.offset), (BAR, 1000, 0));
    assert_eq!(
        (r.start, r.len, r.offset),
        (BAR + 1000, 3 * BAR - 1000, 1000)
    );
    assert_eq!((l.pattern, r.pattern), (p, p));
    assert_saves(&s);
    // A second split wraps the offset around the content length.
    let (s2, ids2) = ok(
        &s,
        Edit::SplitClip {
            clip: ClipId(ids[0]),
            at: BAR + 3000,
        },
    );
    let r2 = clip(&s2, ClipId(ids2[0]));
    assert_eq!((r2.start, r2.offset), (BAR + 3000, 3000));
    let (s3, ids3) = ok(
        &s2,
        Edit::SplitClip {
            clip: ClipId(ids2[0]),
            at: 2 * BAR + 500,
        },
    );
    assert_eq!(clip(&s3, ClipId(ids3[0])).offset, 500);
    // The split point must be strictly inside.
    for at in [BAR, 4 * BAR, 0, 10 * BAR] {
        assert!(matches!(
            fails(&d, Edit::SplitClip { clip: a, at }),
            EditError::BadArgument { .. }
        ));
    }
}

#[test]
fn make_unique_copies_only_when_shared() {
    let (d, c, _) = base();
    let (d, p, a) = new_clip(&d, c, 0, BAR);
    let d = step(&d, p, 2);
    // Alone: nothing to do.
    let (same, ids) = ok(&d, Edit::MakeUnique { clip: a });
    assert!(ids.is_empty());
    assert_eq!(same.project, d.project);
    let (d, ids) = ok(
        &d,
        Edit::DuplicateClips {
            clips: vec![a],
            dt: BAR as i64,
            linked: true,
        },
    );
    let b = ClipId(ids[0]);
    let (u, ids) = ok(&d, Edit::MakeUnique { clip: b });
    assert_eq!(ids.len(), 1);
    assert_eq!(clip(&u, b).pattern.0, ids[0]);
    assert_eq!(clip(&u, a).pattern, p);
    assert_eq!(u.project.pattern(PatternId(ids[0])).unwrap().notes.len(), 1);
    // Now editing one no longer changes the other.
    let u = step(&u, clip(&u, b).pattern, 5);
    assert_eq!(u.project.pattern(p).unwrap().notes.len(), 1);
    assert_saves(&u);
}

#[test]
fn editing_content_through_a_linked_clip_changes_every_linked_clip() {
    let (d, c, _) = base();
    let (d, p, a) = new_clip(&d, c, 0, BAR);
    let (d, ids) = ok(
        &d,
        Edit::DuplicateClips {
            clips: vec![a],
            dt: BAR as i64,
            linked: true,
        },
    );
    let b = ClipId(ids[0]);
    let before = clip(&d, b).pattern;
    let d = step(&d, clip(&d, a).pattern, 3);
    assert_eq!(clip(&d, a).pattern, clip(&d, b).pattern);
    assert_eq!(clip(&d, b).pattern, before);
    assert_eq!(
        d.project.pattern(clip(&d, b).pattern).unwrap().notes.len(),
        1
    );
    assert_eq!(clip(&d, b).pattern, p);
}

#[test]
fn muting_and_the_loop_region() {
    let (d, c, _) = base();
    let (d, _, a) = new_clip(&d, c, 0, BAR);
    let (d, _) = ok(
        &d,
        Edit::SetClipMuted {
            clips: vec![a, a],
            muted: true,
        },
    );
    assert!(clip(&d, a).muted);
    assert!(matches!(
        fails(
            &d,
            Edit::SetClipMuted {
                clips: vec![ClipId(5000)],
                muted: false
            }
        ),
        EditError::NotFound { .. }
    ));
    let (d, _) = ok(
        &d,
        Edit::SetLoopRegion {
            start: BAR,
            end: 5 * BAR,
            enabled: true,
        },
    );
    let lr = d.project.loop_region;
    assert_eq!((lr.start, lr.end, lr.enabled), (BAR, 5 * BAR, true));
    for (start, end, enabled) in [
        (BAR, BAR, true),
        (5, 3, false),
        (0, 0, true),
        (0, MAX_TICK + 1, true),
    ] {
        assert!(
            apply(
                &d,
                &Edit::SetLoopRegion {
                    start,
                    end,
                    enabled
                }
            )
            .is_err(),
            "{start} {end}"
        );
    }
    let (d, _) = ok(
        &d,
        Edit::SetLoopRegion {
            start: 0,
            end: 0,
            enabled: false,
        },
    );
    assert_eq!(d.project.loop_region, Default::default());
    assert_saves(&d);
}

#[test]
fn removing_a_channel_or_content_takes_its_clips() {
    let (d, kick) = add_channel(&Document::new(), "Kick", 36);
    let (d, snare) = add_channel(&d, "Snare", 38);
    let (d, _, _) = new_clip(&d, kick, 0, BAR);
    let (d, sp, _) = new_clip(&d, snare, 0, BAR);
    let (d, _) = ok(&d, Edit::RemoveChannel { channel: kick });
    assert_eq!(d.project.clips.len(), 1);
    assert_eq!(d.project.patterns.len(), 1);
    assert_saves(&d);
    let (d, _) = ok(&d, Edit::RemovePattern { pattern: sp });
    assert!(d.project.clips.is_empty() && d.project.patterns.is_empty());
    assert_saves(&d);
}

#[test]
fn content_length_changes_keep_clip_offsets_inside() {
    let (d, c, _) = base();
    let (d, p, a) = new_clip(&d, c, 0, 4 * BAR);
    let (d, _) = ok(
        &d,
        Edit::SplitClip {
            clip: a,
            at: 3 * 960,
        },
    );
    let r = d.project.clips.iter().find(|x| x.id != a).unwrap().id;
    assert_eq!(clip(&d, r).offset, 3 * 960);
    // 16 steps -> 8 steps: 1920 ticks; offset 2880 wraps to 960.
    let (d, _) = ok(
        &d,
        Edit::SetPatternLength {
            pattern: p,
            length_steps: 8,
        },
    );
    assert_eq!(clip(&d, r).offset, 960);
    assert_saves(&d);
    let (d, _) = ok(
        &d,
        Edit::SetStepTicks {
            pattern: p,
            step_ticks: 60,
        },
    );
    assert_eq!(clip(&d, r).offset, 960 % (8 * 60));
    assert_saves(&d);
}

#[test]
fn clip_batches_are_atomic_and_one_group() {
    let (d, c, _) = base();
    let (d, _, a) = new_clip(&d, c, 0, BAR);
    let rev = d.revision;
    let r = apply_batch(
        &d,
        &[
            Edit::MoveClips {
                clips: vec![a],
                dt: BAR as i64,
            },
            Edit::MoveClips {
                clips: vec![ClipId(404)],
                dt: 1,
            },
        ],
    );
    assert!(r.is_err());
    assert_eq!(clip(&d, a).start, 0);
    let (d2, _) = apply_batch(
        &d,
        &[
            Edit::MoveClips {
                clips: vec![a],
                dt: BAR as i64,
            },
            Edit::RemoveClips { clips: vec![a] },
        ],
    )
    .unwrap();
    assert_eq!(d2.revision, rev + 1);
    assert!(d2.project.clips.is_empty() && d2.project.patterns.len() == 1);
}
