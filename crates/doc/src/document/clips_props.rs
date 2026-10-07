// SPDX-License-Identifier: GPL-3.0-or-later
//! Property tests for the timeline model (SPEC 20.3): random edit
//! sequences never produce an invalid document, ids never repeat, every
//! accepted document round-trips, and linked clips share their content.

use std::collections::HashSet;

use super::beats_tests::{random_clip_edit, random_edit_b};
use super::tests::{Rng, random_edit};
use super::*;

fn assert_saves(d: &Document) {
    validate(&d.project).expect("validates");
    let text = protocol::format::emit(&d.project, d.next_id).expect("emits");
    let (back, next) = protocol::format::parse(&text).expect("parses");
    assert_eq!(back, *d.project);
    assert!(next >= d.next_id || next > d.project.max_id());
}

/// Every clip names existing content of its own instrument, with an
/// offset inside it.
fn assert_clip_invariants(d: &Document) {
    for c in d.project.clips.iter().filter(|c| c.audio.is_none()) {
        let pat = d
            .project
            .pattern(c.pattern)
            .unwrap_or_else(|| panic!("clip {} has no content", c.id.0));
        assert_eq!(pat.instrument, c.instrument);
        assert!(c.offset < pat.length_ticks().max(1));
    }
}

#[test]
fn random_timeline_sequences_stay_valid_unique_and_round_trip() {
    let mut accepted = HashSet::new();
    for seed in 1..=80u64 {
        let mut r = Rng(0x7131_C11B_0000_0001 ^ seed.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        // Half the runs start from the starter beat, half from nothing.
        let mut d = if seed % 2 == 0 {
            crate::starter::starter_project()
        } else {
            Document::new()
        };
        let mut seen: HashSet<u32> = HashSet::new();
        for i in 0..400 {
            let e = match r.below(10) {
                0..=5 => random_clip_edit(&mut r, &d),
                6 | 7 => random_edit_b(&mut r, &d),
                _ => random_edit(&mut r, &d),
            };
            let Ok((nd, created)) = apply(&d, &e) else {
                continue;
            };
            validate(&nd.project)
                .unwrap_or_else(|err| panic!("seed {seed} step {i}: {e:?} left it invalid: {err}"));
            assert_clip_invariants(&nd);
            for id in &created {
                assert!(seen.insert(*id), "id {id} handed out twice ({e:?})");
                assert!(*id >= d.next_id && *id < nd.next_id);
            }
            if i % 20 == 0 {
                assert_saves(&nd);
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
        assert_saves(&d);
    }
    for k in [
        "AddClip",
        "DuplicateClips",
        "RemoveClips",
        "MoveClips",
        "MoveClipToInstrument",
        "ResizeClips",
        "SplitClip",
        "MakeUnique",
        "SetClipMuted",
        "SetLoopRegion",
    ] {
        assert!(accepted.contains(k), "{k} never accepted: {accepted:?}");
    }
}

#[test]
fn random_content_edits_reach_every_linked_clip() {
    for seed in 1..=40u64 {
        let mut r = Rng(0xABCD_0000_1111 ^ seed.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        let mut d = crate::starter::starter_project();
        for _ in 0..60 {
            let e = random_clip_edit(&mut r, &d);
            if let Ok((nd, _)) = apply(&d, &e) {
                d = nd;
            }
            // Edit through a random clip's content: every clip with the same
            // content id sees it, and no other content changes.
            let Some(c) = r.pick(&d.project.clips.clone()) else {
                continue;
            };
            let before = d.project.pattern(c.pattern).unwrap().notes.len();
            let Ok((nd, _)) = apply(
                &d,
                &Edit::AddNotes {
                    pattern: c.pattern,
                    notes: vec![NewNote {
                        start: 1,
                        len: 5,
                        key: 61,
                        vel: 90,
                    }],
                },
            ) else {
                continue;
            };
            for k in nd.project.clips.iter().filter(|k| k.pattern == c.pattern) {
                assert_eq!(
                    nd.project.pattern(k.pattern).unwrap().notes.len(),
                    before + 1
                );
            }
            for p in &nd.project.patterns {
                if p.id != c.pattern {
                    assert_eq!(p, d.project.pattern(p.id).unwrap());
                }
            }
            d = nd;
        }
    }
}
