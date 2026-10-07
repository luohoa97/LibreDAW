// SPDX-License-Identifier: GPL-3.0-or-later
//! Song mode (SPEC 15.6): the playlist compiled to a timeline, exact note
//! positions across clip boundaries, looping and ending, the playhead in
//! song ticks, and the offline song render.

mod common;

use common::*;
use engine::rt::{RtGuard, rt_events};
use engine::{SongRequest, render_song};
use protocol::engine::{EngineCommand, EngineEvent, TransportMode};
use protocol::ids::{ClipId, PatternId, PlaylistTrackId};
use protocol::model::{Clip, PlaylistTrack, Project};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32};

const SR: f64 = 48000.0;
/// Song length in ticks: the last clip ends at 9600 + 100.
const SONG_LEN: i128 = 9700;

fn song_project(bpm: f64) -> Project {
    let mut p = project(
        bpm,
        vec![],
        vec![synth_channel(1, 0, tone_params())],
        vec![
            // P1: 16 steps = 3840 ticks.
            pattern(
                1,
                16,
                &[(1, vec![(1, 0, 240, 60, 100), (2, 3600, 240, 62, 100)])],
            ),
            // P2: 8 steps = 1920 ticks.
            pattern(2, 8, &[(1, vec![(3, 480, 240, 64, 100)])]),
        ],
    );
    let clip = |id, pat, start, len| Clip {
        id: ClipId(id),
        pattern: PatternId(pat),
        start,
        len,
    };
    p.playlist.push(Arc::new(PlaylistTrack {
        id: PlaylistTrackId(50),
        name: "A".into(),
        clips: vec![
            clip(51, 1, 0, 3840),    // exactly one P1
            clip(52, 2, 3840, 5760), // three P2 (repeats)
            clip(53, 1, 9600, 100),  // P1 cut after 100 ticks
        ],
    }));
    p.playlist.push(Arc::new(PlaylistTrack {
        id: PlaylistTrackId(60),
        name: "B".into(),
        // P2's note starts at 480, past this clip's 480 ticks: cut away.
        clips: vec![clip(61, 2, 960, 480)],
    }));
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

fn song_rig(p: &Project, loop_song: bool) -> Rig {
    let mut r = rig(p, SR, false);
    r.rt.command(EngineCommand::SetTransportMode {
        mode: TransportMode::Song,
        loop_song,
    });
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
fn the_song_stops_at_its_end_and_reports_it() {
    let p = song_project(120.0);
    let mut r = song_rig(&p, false);
    let end = ideal_sample(SONG_LEN, SR as i128, 120, 1) as usize;
    r.run(end - 100, 256);
    assert!(r.rt.is_playing());
    r.run(2000, 256);
    assert!(!r.rt.is_playing());
    let mut stopped = None;
    while let Ok(e) = r.ui.events.pop() {
        if let EngineEvent::Stopped { tick } = e {
            stopped = Some(tick);
        }
    }
    assert_eq!(stopped, Some(SONG_LEN as u64));
    assert_eq!(r.rt.live_notes(protocol::engine::ChannelSlot(0)), 0);
    // Nothing more happens.
    let n = r.rt.trace().len();
    r.run(48000, 256);
    assert_eq!(r.rt.trace().len(), n);
}

#[test]
fn a_looping_song_repeats_with_exact_positions() {
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
fn the_playhead_counts_song_ticks() {
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
fn switching_modes_restarts_and_pattern_mode_is_unchanged() {
    let p = song_project(120.0);
    let mut r = rig(&p, SR, true);
    // Pattern mode plays P1 and loops it at 3840 ticks.
    r.run(ideal_sample(3840 + 960, SR as i128, 120, 1) as usize, 256);
    assert!(r.rt.trace().iter().any(|e| e.id == 2));
    r.rt.command(EngineCommand::SetTransportMode {
        mode: TransportMode::Song,
        loop_song: false,
    });
    let before = r.rt.trace().len();
    r.run(4800, 256);
    // Song mode restarted from tick 0: note 1 starts again right away.
    let again = &r.rt.trace()[before..];
    assert!(again.iter().any(|e| e.on && e.id == 1));
    // And back to the pattern.
    r.rt.command(EngineCommand::SetTransportMode {
        mode: TransportMode::Pattern,
        loop_song: false,
    });
    r.run(ideal_sample(3840, SR as i128, 120, 1) as usize, 256);
    assert!(r.rt.is_playing());
}

#[test]
fn an_empty_playlist_and_missing_patterns_are_harmless() {
    let mut p = song_project(120.0);
    {
        let pt = Arc::make_mut(&mut p.playlist[0]);
        pt.clips.push(Clip {
            id: ClipId(70),
            pattern: PatternId(999),
            start: 20000,
            len: 480,
        });
    }
    let r = rig(&p, SR, false);
    // The missing pattern's clip is skipped, so it does not extend the song.
    assert_eq!(r.rt.compiled().unwrap().song_len_ticks, SONG_LEN as u32);
    let empty = project(
        120.0,
        vec![],
        vec![synth_channel(1, 0, tone_params())],
        vec![pattern(1, 16, &[])],
    );
    let mut r = song_rig(&empty, false);
    r.run(4800, 256);
    assert!(!r.rt.is_playing(), "an empty song ends at once");
}

#[test]
fn a_huge_clip_over_a_tiny_pattern_is_bounded() {
    let mut p = song_project(120.0);
    {
        let mut one = pattern(3, 1, &[(1, vec![(9, 0, 1, 60, 100)])]);
        one.step_ticks = 1;
        p.patterns.push(Arc::new(one));
        Arc::make_mut(&mut p.playlist[1]).clips.push(Clip {
            id: ClipId(80),
            pattern: PatternId(3),
            start: 0,
            len: 1 << 30,
        });
    }
    let r = rig(&p, SR, false);
    let c = r.rt.compiled().unwrap();
    let n: usize = c.song.notes.iter().map(Vec::len).sum();
    assert!(n <= engine::compiled::MAX_SONG_NOTES);
}

#[test]
fn song_playback_makes_no_allocations() {
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
fn render_song_matches_live_song_playback_and_has_the_right_length() {
    let p = Arc::new(song_project(120.0));
    let mut slots = engine::Slots::new();
    slots.sync(&p).unwrap();
    let progress = AtomicU32::new(0);
    let cancel = AtomicBool::new(false);
    let req = SongRequest {
        project: p.clone(),
        tail_seconds: 0.5,
        sample_rate: 48000,
        store: None,
    };
    let out = render_song(&req, &slots, &[], &progress, &cancel)
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
    // An empty playlist is an error, and cancel works.
    let mut q = (*p).clone();
    q.playlist.clear();
    assert!(
        render_song(
            &SongRequest {
                project: Arc::new(q),
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
        render_song(&req, &slots, &[], &progress, &cancel),
        Err(engine::EngineError::Cancelled)
    ));
}
