// SPDX-License-Identifier: GPL-3.0-or-later
//! Pure math of the per-step lane editor (docs/ui-design.md 3.3, SPEC
//! 15.4): which lane is open, how a pointer height maps to a velocity or a
//! pitch offset, which ratchet counts a step accepts, and how the cursor
//! moves. No GTK here.

use protocol::consts::{MAX_STEP_OFFSET, RATCHETS};

/// Height of the lane row.
pub const LANE_H: f64 = 56.0;

/// Space above and below the bars inside the lane row.
const PAD: f64 = 6.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lane {
    Velocity,
    Pitch,
    Ratchet,
}

impl Lane {
    pub const ALL: [Lane; 3] = [Lane::Velocity, Lane::Pitch, Lane::Ratchet];

    pub fn label(self) -> &'static str {
        match self {
            Lane::Velocity => "Volume",
            Lane::Pitch => "Pitch",
            Lane::Ratchet => "Repeats",
        }
    }

    pub fn tooltip(self) -> &'static str {
        match self {
            Lane::Velocity => "Change how loud each hit is",
            Lane::Pitch => "Change the pitch of each hit",
            Lane::Ratchet => "Play a hit 2, 3, 4, 6 or 8 times in a row, for rolls",
        }
    }
}

/// Pointer height (within the lane row) to a velocity, 1 to 127: the top is
/// loudest.
pub fn vel_from_y(y: f64, h: f64) -> u8 {
    let span = (h - 2.0 * PAD).max(1.0);
    let t = 1.0 - ((y - PAD) / span).clamp(0.0, 1.0);
    (1.0 + t * 126.0).round().clamp(1.0, 127.0) as u8
}

/// Height of the velocity bar, from the baseline up.
pub fn vel_bar_h(vel: u8, h: f64) -> f64 {
    let span = (h - 2.0 * PAD).max(1.0);
    (vel.clamp(1, 127) as f64 - 1.0) / 126.0 * span + 2.0
}

/// Pointer height to a whole-semitone offset, -24 to 24, zero at the middle.
pub fn off_from_y(y: f64, h: f64) -> i8 {
    let half = ((h - 2.0 * PAD) / 2.0).max(1.0);
    let v = ((h / 2.0 - y) / half * MAX_STEP_OFFSET as f64).round();
    v.clamp(-(MAX_STEP_OFFSET as f64), MAX_STEP_OFFSET as f64) as i8
}

/// The vertical extent `(top, height)` of a pitch bar: it grows from the
/// center line up for positive offsets and down for negative ones; zero is
/// a 2 px mark on the line.
pub fn off_bar(off: i8, h: f64) -> (f64, f64) {
    let half = ((h - 2.0 * PAD) / 2.0).max(1.0);
    let len = off.unsigned_abs() as f64 / MAX_STEP_OFFSET as f64 * half;
    let mid = h / 2.0;
    match off {
        0 => (mid - 1.0, 2.0),
        o if o > 0 => (mid - len, len),
        _ => (mid, len),
    }
}

/// The ratchet counts a step of `step_ticks` accepts (17.2: the step length
/// must divide evenly).
pub fn ratchet_choices(step_ticks: u32) -> Vec<u8> {
    RATCHETS
        .iter()
        .copied()
        .filter(|r| step_ticks.is_multiple_of(*r as u32))
        .collect()
}

/// The next count after `cur` in `choices`, wrapping; `up` false goes down.
pub fn cycle_ratchet(cur: u8, up: bool, choices: &[u8]) -> u8 {
    if choices.is_empty() {
        return 1;
    }
    let i = choices.iter().position(|c| *c == cur).unwrap_or(0);
    let n = choices.len();
    let j = if up { (i + 1) % n } else { (i + n - 1) % n };
    choices[j]
}

/// The next count after `cur` without wrapping (for the scroll wheel).
pub fn step_ratchet(cur: u8, up: bool, choices: &[u8]) -> u8 {
    let i = choices.iter().position(|c| *c == cur).unwrap_or(0);
    let j = if up {
        (i + 1).min(choices.len().saturating_sub(1))
    } else {
        i.saturating_sub(1)
    };
    choices.get(j).copied().unwrap_or(1)
}

/// A velocity changed by `delta`, kept in 1 to 127.
pub fn nudge_vel(vel: u8, delta: i32) -> u8 {
    (vel as i32 + delta).clamp(1, 127) as u8
}

/// A pitch offset changed by `delta`, kept in range and in the MIDI keys
/// of a channel whose root key is `root`.
pub fn nudge_off(off: i8, delta: i32, root: u8) -> i8 {
    let max = MAX_STEP_OFFSET as i32;
    let lo = (-max).max(-(root as i32));
    let hi = max.min(127 - root as i32);
    (off as i32 + delta).clamp(lo, hi) as i8
}

/// Accessible text of the cursor step in `lane`.
pub fn value_text(lane: Lane, step: u32, vel: u8, off: i8, repeat: u8) -> String {
    match lane {
        Lane::Velocity => format!("Hit {}, volume {vel}", step + 1),
        Lane::Pitch => format!("Hit {}, pitch {off:+} semitones", step + 1),
        Lane::Ratchet => format!("Hit {}, repeats {repeat}", step + 1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn velocity_maps_top_to_loud_and_bottom_to_soft() {
        assert_eq!(vel_from_y(0.0, LANE_H), 127);
        assert_eq!(vel_from_y(LANE_H, LANE_H), 1);
        let mid = vel_from_y(LANE_H / 2.0, LANE_H);
        assert!((60..=68).contains(&mid), "{mid}");
        // The bar and the pointer agree: a bar drawn for v is hit at v.
        for v in [1u8, 32, 64, 96, 127] {
            let top = LANE_H - PAD - (vel_bar_h(v, LANE_H) - 2.0);
            assert_eq!(vel_from_y(top, LANE_H), v);
        }
    }

    #[test]
    fn pitch_is_centered_and_snapped() {
        assert_eq!(off_from_y(LANE_H / 2.0, LANE_H), 0);
        assert_eq!(off_from_y(-100.0, LANE_H), 24);
        assert_eq!(off_from_y(1000.0, LANE_H), -24);
        let (top, len) = off_bar(12, LANE_H);
        assert!(top < LANE_H / 2.0 && (top + len - LANE_H / 2.0).abs() < 1e-9);
        let (top, len) = off_bar(-12, LANE_H);
        assert!(top == LANE_H / 2.0 && len > 0.0);
        assert_eq!(off_bar(0, LANE_H).1, 2.0);
    }

    #[test]
    fn ratchets_must_divide_the_step() {
        assert_eq!(ratchet_choices(240), vec![1, 2, 3, 4, 6, 8]);
        assert_eq!(ratchet_choices(120), vec![1, 2, 3, 4, 6, 8]);
        // 100 ticks: 3, 6 and 8 do not divide it.
        assert_eq!(ratchet_choices(100), vec![1, 2, 4]);
    }

    #[test]
    fn ratchet_cycles_and_steps() {
        let c = ratchet_choices(240);
        assert_eq!(cycle_ratchet(1, true, &c), 2);
        assert_eq!(cycle_ratchet(8, true, &c), 1);
        assert_eq!(cycle_ratchet(1, false, &c), 8);
        assert_eq!(step_ratchet(8, true, &c), 8);
        assert_eq!(step_ratchet(1, false, &c), 1);
        assert_eq!(step_ratchet(3, true, &c), 4);
        assert_eq!(cycle_ratchet(5, true, &[]), 1);
    }

    #[test]
    fn nudges_stay_in_range() {
        assert_eq!(nudge_vel(120, 8), 127);
        assert_eq!(nudge_vel(4, -8), 1);
        assert_eq!(nudge_off(23, 5, 60), 24);
        assert_eq!(nudge_off(-23, -5, 60), -24);
        // Never leaves the keyboard: root 10 cannot go below -10.
        assert_eq!(nudge_off(-9, -5, 10), -10);
        assert_eq!(nudge_off(0, 50, 120), 7);
    }

    #[test]
    fn value_texts() {
        assert_eq!(value_text(Lane::Velocity, 4, 96, 0, 1), "Hit 5, volume 96");
        assert_eq!(
            value_text(Lane::Pitch, 0, 96, -3, 1),
            "Hit 1, pitch -3 semitones"
        );
        assert_eq!(value_text(Lane::Ratchet, 1, 96, 0, 4), "Hit 2, repeats 4");
    }
}
