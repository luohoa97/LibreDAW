// SPDX-License-Identifier: GPL-3.0-or-later
//! The project a new user starts with (SPEC 20.3): Kick, Snare, Hat, and
//! 808 rows, each on its own mixer track with a 1-bar content repeated as
//! four linked clips, and a loop region over the four bars, so Space plays a
//! beat at once. The UI swaps in pack samples when it has them.

use protocol::edit::{Edit, NewInstrument};
use protocol::ids::{ChannelId, TrackId};

use crate::document::{Document, apply, apply_batch};
use crate::presets::presets;

/// Bars the starter beat spans.
pub const STARTER_BARS: u32 = 4;

/// Steps (of 16) each starter row plays: kick on beats 1 and 3, snare on 2
/// and 4, hats on eighths, 808 on 1.
fn steps_for(name: &str) -> &'static [u8] {
    match name {
        "Kick" => &[0, 8],
        "Snare" => &[4, 12],
        "Hat" => &[0, 2, 4, 6, 8, 10, 12, 14],
        _ => &[0],
    }
}

/// The new-project document. Built only from edits, so it validates, and
/// ids come from the normal counter.
pub fn starter_project() -> Document {
    let mut d = Document::new();
    let bar = d.project.time_sig_num as u32 * protocol::consts::PPQ;
    let all = presets();
    let preset = |name: &str| all.iter().find(|p| p.name == name).expect("preset exists");
    // (row name, instrument, root key)
    let rows: [(&str, NewInstrument, u8); 4] = [
        (
            "Kick",
            NewInstrument::Synth {
                params: preset("Kick").params,
            },
            preset("Kick").root_key,
        ),
        (
            "Snare",
            NewInstrument::Synth {
                params: preset("Snare").params,
            },
            preset("Snare").root_key,
        ),
        (
            "Hat",
            NewInstrument::Synth {
                params: preset("Closed hat").params,
            },
            preset("Closed hat").root_key,
        ),
        ("808", NewInstrument::Bass808 { mono: true }, 36),
    ];
    for (name, instrument, root_key) in rows {
        let step = |d: &Document, e: Edit| apply(d, &e).expect("starter edit").0;
        let (nd, t) = apply(&d, &Edit::AddTrack { name: name.into() }).expect("track");
        d = nd;
        let track = TrackId(t[0]);
        let (nd, c) = apply(
            &d,
            &Edit::AddChannel {
                name: name.into(),
                instrument,
                root_key,
                track,
            },
        )
        .expect("channel");
        d = nd;
        let channel = ChannelId(c[0]);
        let (nd, ids) = apply(
            &d,
            &Edit::AddClip {
                instrument: channel,
                pattern: None,
                start: 0,
                len: bar,
            },
        )
        .expect("clip");
        d = nd;
        let content = protocol::ids::PatternId(ids[0]);
        for s in steps_for(name) {
            d = step(
                &d,
                Edit::SetStep {
                    pattern: content,
                    step: *s,
                    on: true,
                    vel: None,
                },
            );
        }
        let first = protocol::ids::ClipId(ids[1]);
        let mut edits = Vec::new();
        for i in 1..STARTER_BARS {
            edits.push(Edit::DuplicateClips {
                clips: vec![first],
                dt: (i * bar) as i64,
                linked: true,
            });
        }
        d = apply_batch(&d, &edits).expect("linked clips").0;
    }
    apply(
        &d,
        &Edit::SetLoopRegion {
            start: 0,
            end: STARTER_BARS * bar,
            enabled: true,
        },
    )
    .expect("loop")
    .0
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::validate::validate;

    #[test]
    fn starter_validates_round_trips_and_has_notes() {
        let d = starter_project();
        validate(&d.project).unwrap();
        let text = protocol::format::emit(&d.project, d.next_id).unwrap();
        let (back, next) = protocol::format::parse(&text).unwrap();
        assert_eq!(back, *d.project);
        assert_eq!(next, d.next_id);
        assert!(d.project.note_count() > 0);
    }

    #[test]
    fn starter_shape_matches_spec_20_3() {
        let d = starter_project();
        let p = &d.project;
        let names: Vec<&str> = p.channels.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["Kick", "Snare", "Hat", "808"]);
        // Each on its own mixer track, not the master.
        let mut tracks: Vec<_> = p.channels.iter().map(|c| c.track).collect();
        tracks.sort();
        tracks.dedup();
        assert_eq!(tracks.len(), 4);
        assert!(!tracks.contains(&TrackId::MASTER));
        assert_eq!(p.patterns.len(), 4);
        assert_eq!(p.clips.len(), 16);
        let bar = 3840;
        for c in &p.channels {
            let clips: Vec<_> = p.clips.iter().filter(|k| k.instrument == c.id).collect();
            assert_eq!(clips.len(), 4);
            assert!(
                clips
                    .iter()
                    .all(|k| k.pattern == clips[0].pattern && k.len == bar)
            );
            let starts: Vec<u32> = clips.iter().map(|k| k.start).collect();
            assert_eq!(starts, [0, bar, 2 * bar, 3 * bar]);
        }
        let lr = p.loop_region;
        assert_eq!((lr.start, lr.end, lr.enabled), (0, 4 * bar, true));
        let steps = |name: &str| {
            let c = p.channels.iter().find(|c| c.name == name).unwrap();
            let pat = p.patterns.iter().find(|x| x.instrument == c.id).unwrap();
            pat.notes
                .iter()
                .map(|n| (n.start / pat.step_ticks) as u8)
                .collect::<Vec<_>>()
        };
        assert_eq!(steps("Kick"), [0, 8]);
        assert_eq!(steps("Snare"), [4, 12]);
        assert_eq!(steps("Hat"), [0, 2, 4, 6, 8, 10, 12, 14]);
        assert_eq!(steps("808"), [0]);
    }
}
