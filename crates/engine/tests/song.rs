// SPDX-License-Identifier: GPL-3.0-or-later
//! Timeline playback (SPEC 20): clips compiled to one note timeline, exact
//! note positions across clip boundaries, the loop region, the arrangement
//! end, the playhead, seeking anywhere, and the range render.

mod common;

use common::*;
use engine::rt::{RtGuard, rt_events};
use engine::{RangeRequest, render_range};
use protocol::engine::{ChannelSlot, EngineCommand, EngineEvent};
use protocol::ids::{ClipId, PatternId};
use protocol::model::{Clip, LoopRegion, Project};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32};

const SR: f64 = 48000.0;
/// Arrangement length in ticks: the last clip ends at 9600 + 100.
const SONG_LEN: i128 = 9700;

fn clip(id: u32, instrument: u32, pat: u32, start: u32, len: u32) -> Clip {
    Clip {
        id: ClipId(id),
        instrument: protocol::ids::ChannelId(instrument),
        pattern: PatternId(pat),
        start,
        len,
        offset: 0,
        muted: false,
        audio: None,
        group: None,
    }
}

fn song_project(bpm: f64) -> Project {
    let mut p = project(
        bpm,
        vec![],
        vec![
            synth_channel(1, 0, tone_params()),
            synth_channel(2, 0, tone_params()),
        ],
        vec![
            // P1: 16 steps = 3840 ticks.
            pattern(
                1,
                16,
                &[(1, vec![(1, 0, 240, 60, 100), (2, 3600, 240, 62, 100)])],
            ),
            // P2: 8 steps = 1920 ticks.
            pattern(2, 8, &[(1, vec![(3, 480, 240, 64, 100)])]),
            // P3: instrument 2's content, 8 steps.
            pattern(3, 8, &[(2, vec![(4, 480, 240, 64, 100)])]),
        ],
    );
    p.clips = vec![
        clip(51, 1, 1, 0, 3840),    // exactly one P1
        clip(52, 1, 2, 3840, 5760), // three P2 (repeats)
        clip(53, 1, 1, 9600, 100),  // P1 cut after 100 ticks
        // P3's note starts at 480, past this clip's 480 ticks: cut away.
        clip(61, 2, 3, 960, 480),
    ];
    p.loop_region = LoopRegion {
        start: 0,
        end: SONG_LEN as u32,
        enabled: false,
    };
    p
}

/// `(tick, key, id, on)` for one pass of the song, in closed form.
fn expected_pass() -> Vec<(i128, u8, u32, bool)> {
    let mut v = vec![
        (0, 60, 1, true),
        (240, 60, 1, false),
        (3600, 62, 2, true),
        (3840, 62, 2, false),
    ];
    for k in 0..3 {
        let t = 3840 + 480 + 1920 * k;
        v.push((t, 64, 3, true));
        v.push((t + 240, 64, 3, false));
    }
    // The cut clip: the note on at 9600, off at the clip end 9700.
    v.push((9600, 60, 1, true));
    v.push((9700, 60, 1, false));
    v
}

/// A rig playing from tick 0 with the loop region over the whole song when
/// `looped`, else off.
fn song_rig(p: &Project, looped: bool) -> Rig {
    let mut p = p.clone();
    p.loop_region = LoopRegion {
        start: 0,
        end: SONG_LEN as u32,
        enabled: looped,
    };
    let mut r = rig(&p, SR, false);
    r.rt.command(EngineCommand::Play);
    r
}

fn traced(r: &Rig) -> Vec<(u64, u8, u32, bool)> {
    let mut v: Vec<_> =
        r.rt.trace()
            .iter()
            .map(|e| (e.sample, e.key, e.id, e.on))
            .collect();
    v.sort();
    v
}

fn to_samples(
    pass: &[(i128, u8, u32, bool)],
    passes: i128,
    num: i128,
    den: i128,
    total: u64,
) -> Vec<(u64, u8, u32, bool)> {
    let mut out = Vec::new();
    for k in 0..passes {
        for &(t, key, id, on) in pass {
            let s = ideal_sample(k * SONG_LEN + t, SR as i128, num, den);
            if s < total {
                out.push((s, key, id, on));
            }
        }
    }
    out.sort();
    out
}

fn stopped(r: &mut Rig) -> Option<u64> {
    let mut out = None;
    while let Ok(e) = r.ui.events.pop() {
        if let EngineEvent::Stopped { tick } = e {
            out = Some(tick);
        }
    }
    out
}

#[test]
fn note_positions_across_clips_match_the_closed_form() {
    for &(num, den) in &[(120i128, 1i128), (13333, 100), (999, 1)] {
        let bpm = num as f64 / den as f64;
        let p = song_project(bpm);
        let total = ideal_sample(SONG_LEN + 960, SR as i128, num, den);
        let want = to_samples(&expected_pass(), 1, num, den, total);
        for cb in [1usize, 37, 256, 1000] {
            if cb == 1 && num != 13333 {
                continue;
            }
            let mut r = song_rig(&p, false);
            r.run(total as usize, cb);
            assert_eq!(traced(&r), want, "bpm {bpm} callback {cb}");
        }
    }
}

#[test]
fn playback_stops_at_the_arrangement_end_and_reports_it() {
    let p = song_project(120.0);
    let mut r = song_rig(&p, false);
    let end = ideal_sample(SONG_LEN, SR as i128, 120, 1) as usize;
    r.run(end - 100, 256);
    assert!(r.rt.is_playing());
    r.run(2000, 256);
    assert!(!r.rt.is_playing());
    assert_eq!(stopped(&mut r), Some(SONG_LEN as u64));
    assert_eq!(r.rt.live_notes(ChannelSlot(0)), 0);
    // Nothing more happens.
    let n = r.rt.trace().len();
    r.run(48000, 256);
    assert_eq!(r.rt.trace().len(), n);
}

#[test]
fn a_loop_region_over_the_song_repeats_with_exact_positions() {
    let p = song_project(120.0);
    let total = ideal_sample(3 * SONG_LEN + 100, SR as i128, 120, 1);
    let want = to_samples(&expected_pass(), 4, 120, 1, total);
    for cb in [37usize, 512] {
        let mut r = song_rig(&p, true);
        r.run(total as usize, cb);
        assert!(r.rt.is_playing());
        assert_eq!(traced(&r), want, "callback {cb}");
    }
}

#[test]
fn a_loop_region_inside_the_song_wraps_to_its_start_exactly() {
    // Region [3840, 7680): two repeats of P2, note id 3 at 4320 and 6240.
    let mut p = song_project(133.33);
    p.loop_region = LoopRegion {
        start: 3840,
        end: 7680,
        enabled: true,
    };
    let mut r = rig(&p, SR, false);
    r.rt.command(EngineCommand::Seek { tick: 3840 });
    r.rt.command(EngineCommand::Play);
    let (num, den) = (13333i128, 100i128);
    let total = ideal_sample(3 * 3840 + 1000, SR as i128, num, den);
    r.run(total as usize, 100);
    let mut want = Vec::new();
    for pass in 0..4i128 {
        for rel in [480i128, 2400] {
            for (dt, on) in [(0, true), (240, false)] {
                let s = ideal_sample(pass * 3840 + rel + dt, SR as i128, num, den);
                if s < total {
                    want.push((s, 64u8, 3u32, on));
                }
            }
        }
    }
    want.sort();
    assert_eq!(traced(&r), want);
    assert!(r.rt.is_playing());
}

#[test]
fn starting_past_the_loop_end_plays_on_to_the_arrangement_end() {
    let mut p = song_project(120.0);
    p.loop_region = LoopRegion {
        start: 0,
        end: 3840,
        enabled: true,
    };
    let mut r = rig(&p, SR, false);
    // Seek works anywhere, also past the loop end and past the song.
    r.rt.command(EngineCommand::Seek { tick: 7000 });
    r.rt.command(EngineCommand::Play);
    r.run(ideal_sample(5000, SR as i128, 120, 1) as usize, 256);
    assert!(!r.rt.is_playing(), "ran past the loop to the song end");
    assert_eq!(stopped(&mut r), Some(SONG_LEN as u64));
    assert!(r.rt.trace().iter().any(|e| e.on && e.id == 3));

    // Past the song: no clip, no stop; the playhead just advances.
    let mut r = rig(&song_project(120.0), SR, false);
    r.rt.command(EngineCommand::Seek { tick: 20000 });
    r.rt.command(EngineCommand::Play);
    r.run(48000, 256);
    assert!(r.rt.is_playing());
    assert!(r.rt.trace().is_empty());
    assert_eq!(stopped(&mut r), None);
    let tick = r
        .shared
        .status
        .playhead_tick
        .load(std::sync::atomic::Ordering::Relaxed);
    assert!((20000 + 1900..20000 + 1925).contains(&tick), "{tick}");
}

#[test]
fn seeking_while_playing_moves_the_playhead_and_notes() {
    let p = song_project(120.0);
    let mut r = song_rig(&p, false);
    r.run(4800, 256);
    r.rt.command(EngineCommand::Seek { tick: 3840 + 1920 });
    let before = r.rt.trace().len();
    // Next P2 note on is at 3840 + 1920 + 480 = 6240.
    r.run(ideal_sample(480 + 100, SR as i128, 120, 1) as usize, 256);
    let after = &r.rt.trace()[before..];
    assert!(after.iter().any(|e| e.on && e.id == 3), "{after:?}");
}

#[test]
fn the_playhead_counts_timeline_ticks() {
    let p = song_project(120.0);
    let mut r = song_rig(&p, false);
    // 120 BPM at 48 kHz: 25 frames per tick; 5000 ticks in.
    r.run(5000 * 25, 256);
    let tick = r
        .shared
        .status
        .playhead_tick
        .load(std::sync::atomic::Ordering::Relaxed);
    assert!((5000..=5010).contains(&tick), "{tick}");
}

#[test]
fn clip_offset_starts_into_the_content_and_muted_clips_are_skipped() {
    let mut p = song_project(120.0);
    // P1 from tick 3600 of its content: note 2 first, then the content
    // repeats and note 1 follows at 240.
    p.clips = vec![Clip {
        offset: 3600,
        ..clip(51, 1, 1, 0, 3840)
    }];
    let r = rig(&p, SR, false);
    let c = r.rt.compiled().unwrap();
    let s = r.slots.channel_slot(protocol::ids::ChannelId(1)).unwrap().0 as usize;
    let notes: Vec<_> = c.song.notes[s]
        .iter()
        .map(|n| (n.start, n.end, n.id))
        .collect();
    assert_eq!(notes, vec![(0, 240, 2), (240, 480, 1)]);

    // Muted: nothing plays, but the clip still counts toward the end.
    p.clips[0].muted = true;
    let r = rig(&p, SR, false);
    let c = r.rt.compiled().unwrap();
    assert!(c.song.notes.iter().all(Vec::is_empty));
    assert_eq!(c.song_len_ticks, 3840);
}

#[test]
fn an_empty_timeline_and_missing_content_are_harmless() {
    let mut p = song_project(120.0);
    p.clips.push(clip(70, 1, 999, 20000, 480));
    let r = rig(&p, SR, false);
    // The missing content's clip is skipped, so it does not extend the song.
    assert_eq!(r.rt.compiled().unwrap().song_len_ticks, SONG_LEN as u32);
    let empty = project(
        120.0,
        vec![],
        vec![synth_channel(1, 0, tone_params())],
        vec![pattern(1, 16, &[])],
    );
    let mut empty = empty;
    empty.loop_region.enabled = false;
    let mut r = rig(&empty, SR, true);
    r.run(4800, 256);
    assert!(r.rt.is_playing(), "an empty timeline plays on");
    assert_eq!(stopped(&mut r), None);
}

#[test]
fn a_huge_clip_over_tiny_content_is_bounded() {
    let mut p = song_project(120.0);
    let mut one = pattern(4, 1, &[(1, vec![(9, 0, 1, 60, 100)])]);
    one.contents[0].step_ticks = 1;
    p.patterns.push(Arc::new(one.contents.remove(0)));
    p.clips.push(clip(80, 1, 4, 0, 1 << 30));
    let r = rig(&p, SR, false);
    let c = r.rt.compiled().unwrap();
    let n: usize = c.song.notes.iter().map(Vec::len).sum();
    assert!(n <= engine::compiled::MAX_SONG_NOTES);
}

#[test]
fn timeline_playback_makes_no_allocations() {
    let p = song_project(120.0);
    let mut r = song_rig(&p, true);
    let mut l = vec![0.0f32; 300];
    let mut rr = vec![0.0f32; 300];
    let before = rt_events();
    for _ in 0..2000 {
        let g = RtGuard::enter_counting();
        r.rt.process_planar(&mut l, &mut rr);
        drop(g);
    }
    assert_eq!(rt_events(), before);
}

#[test]
fn range_render_matches_live_playback_and_has_the_right_length() {
    let mut q = song_project(120.0);
    q.loop_region.enabled = false;
    let p = Arc::new(q);
    let mut slots = engine::Slots::new();
    slots.sync(&p).unwrap();
    let progress = AtomicU32::new(0);
    let cancel = AtomicBool::new(false);
    let req = RangeRequest {
        project: p.clone(),
        range: None,
        tail_seconds: 0.5,
        sample_rate: 48000,
        store: None,
    };
    let out = render_range(&req, &slots, &[], &progress, &cancel)
        .unwrap()
        .audio;
    let main = ideal_sample(SONG_LEN, SR as i128, 120, 1) as usize;
    assert_eq!(out.len(), main + 24000);
    assert_eq!(progress.load(std::sync::atomic::Ordering::Relaxed), 100);
    assert!(out.iter().any(|f| f[0].abs() > 0.05));
    // The same runtime path: identical to a live-style run in 512 frames.
    let mut r = song_rig(&p, false);
    let (l, rr) = r.run(out.len(), 512);
    for (i, f) in out.iter().enumerate() {
        assert_eq!((f[0], f[1]), (l[i], rr[i]), "frame {i}");
    }
    // No clips and no loop is an error, and cancel works.
    let mut empty = (*p).clone();
    empty.clips.clear();
    assert!(
        render_range(
            &RangeRequest {
                project: Arc::new(empty),
                range: None,
                tail_seconds: 0.0,
                sample_rate: 48000,
                store: None
            },
            &slots,
            &[],
            &progress,
            &cancel
        )
        .is_err()
    );
    cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    assert!(matches!(
        render_range(&req, &slots, &[], &progress, &cancel),
        Err(engine::EngineError::Cancelled)
    ));
}
