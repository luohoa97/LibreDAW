// SPDX-License-Identifier: GPL-3.0-or-later
//! Sequencer playback: exact timing against the closed-form grid, the
//! owner-id rule, stop, seek, loop wrap and swap behavior.

mod common;

use common::*;
use engine::rt::{RtGuard, rt_events};
use engine::sequencer::TraceEvent;
use protocol::engine::{ChannelSlot, EngineCommand, EngineEvent};

const SIZES: [usize; 5] = [1, 37, 256, 300, 1024];

fn one_synth(bpm: f64, notes: Vec<N>, steps: u8) -> protocol::model::Project {
    project(
        bpm,
        vec![],
        vec![synth_channel(1, 0, tone_params())],
        vec![pattern(1, steps, &[(1, notes)])],
    )
}

/// `(sample, key, id, on)` sorted, from the runtime trace.
fn traced(t: &[TraceEvent]) -> Vec<(u64, u8, u32, bool)> {
    let mut v: Vec<_> = t.iter().map(|e| (e.sample, e.key, e.id, e.on)).collect();
    v.sort();
    v
}

#[test]
fn note_events_match_the_closed_form_grid() {
    // (bpm numerator, denominator)
    let tempos: [(i128, i128); 4] = [(60, 1), (120, 1), (13333, 100), (999, 1)];
    let notes: Vec<N> = vec![
        (1, 0, 240, 60, 100),
        (2, 240, 480, 62, 90),
        (3, 250, 10, 64, 80),
        (4, 960, 1000, 65, 70),
        (5, 3333, 300, 67, 60),
        (6, 3600, 500, 69, 50), // crosses the loop end: off at the wrap
        (7, 1, 1, 71, 40),
    ];
    let len = 3840i128;
    let loops = 3i128;
    for rate in [44100i128, 48000] {
        for &(num, den) in &tempos {
            let bpm = num as f64 / den as f64;
            let pr = one_synth(bpm, notes.clone(), 16);
            let total = ideal_sample(loops * len, rate, num, den);
            let mut expected = Vec::new();
            for k in 0..loops {
                for &(id, start, l, key, _) in &notes {
                    let on = ideal_sample(k * len + start as i128, rate, num, den);
                    let end = (start as i128 + l as i128).min(len);
                    let off = ideal_sample(k * len + end, rate, num, den);
                    if on < total {
                        expected.push((on, key, id, true));
                    }
                    if off < total {
                        expected.push((off, key, id, false));
                    }
                }
            }
            expected.sort();
            for cb in SIZES {
                // single-frame callbacks are slow in debug: one tempo and rate
                if cb == 1 && !(rate == 44100 && num == 13333) {
                    continue;
                }
                let mut r = rig(&pr, rate as f64, true);
                r.run(total as usize, cb);
                assert_eq!(
                    traced(r.rt.trace()),
                    expected,
                    "rate {rate} bpm {bpm} callback {cb}"
                );
            }
        }
    }
}

#[test]
fn overlapping_same_key_notes_keep_closed_form_gate_lengths() {
    // A: [0, 480), B: [240, 720), same key. A's voice ends at B's start (the
    // retrigger); A's own note-off at 480 must not cut B.
    let pr = one_synth(
        120.0,
        vec![(1, 0, 480, 60, 100), (2, 240, 480, 60, 100)],
        16,
    );
    let rate = 48000;
    for cb in SIZES {
        let mut r = rig(&pr, rate as f64, true);
        r.run(ideal_sample(960, rate, 120, 1) as usize, cb);
        let t = traced(r.rt.trace());
        let s = |ticks| ideal_sample(ticks, rate, 120, 1);
        assert_eq!(
            t,
            vec![
                (s(0), 60, 1, true),
                (s(240), 60, 1, false),
                (s(240), 60, 2, true),
                (s(720), 60, 2, false),
            ],
            "callback {cb}"
        );
        // gate lengths in samples
        assert_eq!(t[1].0 - t[0].0, s(240));
        assert_eq!(t[3].0 - t[2].0, s(720) - s(240));
    }
    // B is still sounding at tick 600, after A's nominal end.
    let mut r = rig(&pr, rate as f64, true);
    r.run(ideal_sample(600, rate, 120, 1) as usize, 256);
    assert_ne!(r.rt.live_notes(ChannelSlot(0)) & (1 << 60), 0);
    let (l, _) = r.run(256, 256);
    assert!(peak(&l) > 0.1, "B must still be audible");
}

#[test]
fn stop_releases_notes_and_reports_the_tick() {
    let pr = one_synth(120.0, vec![(1, 0, 3000, 60, 100)], 16);
    let mut r = rig(&pr, 48000.0, true);
    r.run(4800, 256);
    assert_ne!(r.rt.live_notes(ChannelSlot(0)), 0);
    r.rt.command(EngineCommand::Stop);
    assert_eq!(r.rt.live_notes(ChannelSlot(0)), 0);
    r.run(256, 256);
    let off: Vec<_> = r.rt.trace().iter().filter(|e| !e.on).collect();
    assert_eq!(off.len(), 1);
    let mut got = None;
    while let Ok(e) = r.ui.events.pop() {
        if let EngineEvent::Stopped { tick } = e {
            got = Some(tick);
        }
    }
    // 4800 frames at 120 BPM / 48 kHz = 0.1 s = 0.2 beat = 192 ticks.
    assert_eq!(got, Some(192));
    // voices decay and the transport stays stopped
    r.run(48000, 1024);
    assert_eq!(r.rt.active_voices(ChannelSlot(0)), 0);
    assert!(!r.rt.is_playing());
}

#[test]
fn seek_uses_the_binary_search_cursor() {
    let notes: Vec<N> = (0..16)
        .map(|i| (i + 1, i * 240, 100, 60 + i as u8, 100))
        .collect();
    let pr = one_synth(120.0, notes, 16);
    let rate = 48000;
    let mut r = rig(&pr, rate as f64, true);
    r.run(1000, 256);
    let pos = r.rt.position();
    r.rt.command(EngineCommand::Seek { tick: 2400 });
    r.run(48000, 300);
    let on: Vec<_> =
        r.rt.trace()
            .iter()
            .filter(|e| e.on && e.sample >= pos)
            .map(|e| (e.sample, e.id))
            .collect();
    // from tick 2400 the first note is step 10 (id 11) at the seek position
    assert_eq!(on[0], (pos, 11));
    assert_eq!(on[1].1, 12);
    assert_eq!(on[1].0, pos + ideal_sample(240, rate, 120, 1));
}

#[test]
fn deleting_a_sounding_note_by_swap_never_leaves_it_hanging() {
    let with = one_synth(120.0, vec![(1, 0, 1920, 60, 100)], 16);
    let without = one_synth(120.0, vec![], 16);
    let mut r = rig(&with, 48000.0, true);
    r.run(2400, 256);
    assert_ne!(r.rt.live_notes(ChannelSlot(0)), 0);
    let c = engine::compile(&without, &r.slots, 48000.0);
    assert!(r.ui.state.push(c).is_ok());
    r.run(2400, 256);
    // still scheduled to end at tick 1920 from the active-note table
    r.run(48000, 256);
    let offs: Vec<_> = r.rt.trace().iter().filter(|e| !e.on).collect();
    assert_eq!(offs.len(), 1);
    assert_eq!(offs[0].sample, ideal_sample(1920, 48000, 120, 1));
    assert_eq!(r.rt.live_notes(ChannelSlot(0)), 0);
}

#[test]
fn a_swap_that_adds_a_note_ahead_plays_it_and_ignores_the_past() {
    let before = one_synth(120.0, vec![(1, 0, 100, 60, 100)], 16);
    // adds one note behind the playhead (tick 100) and one ahead (tick 960)
    let after = one_synth(
        120.0,
        vec![
            (1, 0, 100, 60, 100),
            (2, 100, 100, 62, 100),
            (3, 960, 100, 64, 100),
        ],
        16,
    );
    let rate = 48000;
    let mut r = rig(&before, rate as f64, true);
    r.run(ideal_sample(500, rate, 120, 1) as usize, 256);
    let c = engine::compile(&after, &r.slots, rate as f64);
    assert!(r.ui.state.push(c).is_ok());
    r.run(ideal_sample(1500, rate, 120, 1) as usize, 256);
    let ids: Vec<u32> = r.rt.trace().iter().filter(|e| e.on).map(|e| e.id).collect();
    assert_eq!(ids, vec![1, 3]);
}

#[test]
fn sequencer_paths_make_no_allocations() {
    let notes: Vec<N> = (0..16).map(|i| (i + 1, i * 240, 480, 60, 100)).collect();
    let pr = one_synth(133.33, notes, 16);
    let mut r = rig(&pr, 44100.0, true);
    r.shared
        .controls
        .set(protocol::engine::CTL_METRONOME_ENABLED, 1.0);
    let c2 = engine::compile(&pr, &r.slots, 44100.0);
    let mut l = vec![0.0f32; 300];
    let mut rr = vec![0.0f32; 300];
    let before = rt_events();
    for i in 0..400 {
        if i == 100 {
            // the push itself happens on the "GTK" side but is allocation
            // free; the audio side only moves boxes
            assert!(r.ui.state.push(c2.clone()).is_ok());
        }
        if i == 150 {
            r.shared.controls.set_tempo(160.0);
        }
        if i == 200 {
            let _ = r.ui.commands.push(EngineCommand::Seek { tick: 1000 });
        }
        if i == 300 {
            let _ = r.ui.commands.push(EngineCommand::Stop);
        }
        let g = RtGuard::enter_counting();
        r.rt.process_planar(&mut l, &mut rr);
        drop(g);
        // drain what the disposal thread would free, outside the audio path
        while r.ui.retired.pop().is_ok() {}
    }
    assert_eq!(rt_events(), before, "audio thread allocated or freed");
}
