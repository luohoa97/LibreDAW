// SPDX-License-Identifier: GPL-3.0-or-later
//! Swing and ratchets: compiled positions against closed-form values, and
//! the sequencer's event samples for a ratchet of 8 (SPEC 17.2).

mod common;

use common::*;
use engine::groove::{ratchet_part, swing_delay_ticks, swung_start};
use protocol::ids::{ChannelId, NoteId, PatternId};
use protocol::model::{ChannelNotes, Note, Pattern};

const STEP: u32 = 240;

fn step_note(id: u32, step: u32, repeat: u8, off: i8) -> Note {
    Note {
        id: NoteId(id),
        start: step * STEP,
        len: STEP,
        key: (60 + off as i16) as u8,
        vel: 100,
        off,
        repeat,
    }
}

fn pat(swing: u16, notes: Vec<Note>) -> Pattern {
    let mut p = Pattern::new(PatternId(1), "p".into());
    p.length_steps = 16;
    p.swing = swing;
    p.notes.push(ChannelNotes {
        channel: ChannelId(1),
        notes,
    });
    p
}

fn compiled_notes(p: Pattern) -> Vec<(u32, u32, u32, u8)> {
    let pr = project(
        120.0,
        vec![],
        vec![synth_channel(1, 0, tone_params())],
        vec![p],
    );
    let r = rig(&pr, 48000.0, false);
    let c = r.rt.compiled().unwrap();
    let slot = r.slots.channel_slot(ChannelId(1)).unwrap().0 as usize;
    c.patterns[0].notes[slot]
        .iter()
        .map(|n| (n.start, n.end, n.id, n.key))
        .collect()
}

#[test]
fn swing_moves_only_step_notes_on_odd_steps() {
    // 62.5 percent: 150 ticks.
    let mut notes: Vec<Note> = (0..16).map(|s| step_note(s + 1, s, 1, 0)).collect();
    // A piano-roll note on an odd step (longer than a step) does not swing.
    notes.push(Note {
        id: NoteId(100),
        start: STEP,
        len: 2 * STEP,
        key: 64,
        vel: 90,
        off: 0,
        repeat: 1,
    });
    let got = compiled_notes(pat(625, notes));
    for s in 0..16u32 {
        let id = s + 1;
        let n = got.iter().find(|n| n.2 == id).unwrap();
        let want = s * STEP + if s % 2 == 1 { 150 } else { 0 };
        assert_eq!(n.0, want, "step {s}");
        assert_eq!(n.1, (want + STEP).min(16 * STEP), "length at step {s}");
        assert_eq!(n.0, swung_start(s * STEP, STEP, 625));
    }
    let roll = got.iter().find(|n| n.2 == 100).unwrap();
    assert_eq!((roll.0, roll.1), (STEP, 3 * STEP));
}

#[test]
fn swing_at_the_last_odd_step_stays_inside_the_pattern() {
    let got = compiled_notes(pat(750, vec![step_note(1, 15, 1, 0)]));
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].0, 15 * STEP + 180);
    assert_eq!(got[0].1, 16 * STEP, "end is clamped to the pattern end");
}

#[test]
fn ratchets_expand_into_ordinary_notes_at_closed_form_positions() {
    for &r in &[2u8, 3, 4, 6, 8] {
        let got = compiled_notes(pat(0, vec![step_note(7, 2, r, 0)]));
        assert_eq!(got.len(), r as usize, "ratchet {r}");
        for (i, n) in got.iter().enumerate() {
            let start = 2 * STEP + (i as u32 * STEP) / r as u32;
            let sub = STEP / r as u32;
            assert_eq!(n.0, start, "r={r} i={i}");
            assert_eq!(n.1, start + sub * 9 / 10, "r={r} i={i}");
            assert_eq!(n.2, 7);
            assert_eq!((n.0 - 2 * STEP, n.1 - n.0), ratchet_part(STEP, r, i as u8));
        }
    }
}

#[test]
fn ratchets_follow_the_swing_of_their_step() {
    let delay = swing_delay_ticks(STEP, 500);
    assert_eq!(delay, 120);
    let got = compiled_notes(pat(500, vec![step_note(1, 1, 4, 0)]));
    let starts: Vec<u32> = got.iter().map(|n| n.0).collect();
    assert_eq!(
        starts,
        vec![STEP + 120, STEP + 120 + 60, STEP + 120 + 120, STEP + 120 + 180]
    );
}

#[test]
fn a_pitch_lane_offset_keeps_the_note_a_step_note_for_swing() {
    let got = compiled_notes(pat(500, vec![step_note(1, 1, 1, 5), step_note(2, 3, 1, -7)]));
    assert_eq!(got[0], (STEP + 120, 2 * STEP + 120, 1, 65));
    assert_eq!(got[1], (3 * STEP + 120, 4 * STEP + 120, 2, 53));
}

#[test]
fn ratchet_8_plays_at_exact_samples_through_the_sequencer() {
    // 120 BPM, 48 kHz: 25 samples per 1 tick... 48000*60/(120*960) = 25.
    let p = pat(0, vec![step_note(9, 0, 8, 0)]);
    let pr = project(
        120.0,
        vec![],
        vec![synth_channel(1, 0, tone_params())],
        vec![p],
    );
    for cb in [37usize, 256, 1000] {
        let mut r = rig(&pr, 48000.0, true);
        r.run(48000, cb);
        let mut ons: Vec<u64> = r
            .rt
            .trace()
            .iter()
            .filter(|e| e.on)
            .map(|e| e.sample)
            .collect();
        ons.sort();
        let want: Vec<u64> = (0..8).map(|i| ideal_sample(i * 30, 48000, 120, 1)).collect();
        assert_eq!(&ons[..8], &want[..], "callback {cb}");
        let mut offs: Vec<u64> = r
            .rt
            .trace()
            .iter()
            .filter(|e| !e.on)
            .map(|e| e.sample)
            .collect();
        offs.sort();
        let want_off: Vec<u64> = (0..8)
            .map(|i| ideal_sample(i * 30 + 27, 48000, 120, 1))
            .collect();
        assert_eq!(&offs[..8], &want_off[..], "callback {cb}");
    }
}
