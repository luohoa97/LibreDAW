// SPDX-License-Identifier: GPL-3.0-or-later
//! Swing and ratchets (SPEC 15.4, 17.2). Pure integer functions: the
//! compiler calls them for playback and export, and the step grid calls the
//! same functions for what it draws, so all three agree to the tick.

/// Ticks by which swing delays an odd step: `step_ticks * swing / 1000`,
/// rounded half up. `swing` is in 1/1000 of a step (0 to 750).
pub fn swing_delay_ticks(step_ticks: u32, swing: u16) -> u32 {
    ((step_ticks as u64 * swing as u64 + 500) / 1000) as u32
}

/// Start tick of a step note after swing. Only a note that starts exactly on
/// an odd step of the grid moves; everything else is returned unchanged.
/// The caller decides whether the note is a step note (`Note::is_step_note`).
pub fn swung_start(start: u32, step_ticks: u32, swing: u16) -> u32 {
    if step_ticks == 0 || swing == 0 {
        return start;
    }
    if start.is_multiple_of(step_ticks) && (start / step_ticks) % 2 == 1 {
        start + swing_delay_ticks(step_ticks, swing)
    } else {
        start
    }
}

/// Start offset from the note start and length of ratchet sub-note `i` of
/// `repeat`, for a note of `len` ticks: it starts at `floor(i * len /
/// repeat)` and lasts `floor(len / repeat * 0.9)`, at least 1 tick. The 10
/// percent gap lets an envelope close between hits. `repeat <= 1` is the
/// note itself and keeps its full length.
pub fn ratchet_part(len: u32, repeat: u8, i: u8) -> (u32, u32) {
    let r = repeat.max(1) as u64;
    if r == 1 {
        return (0, len.max(1));
    }
    let off = (i as u64 * len as u64 / r) as u32;
    let sub = (len as u64 / r) as u32;
    let l = (sub as u64 * 9 / 10) as u32;
    (off, l.max(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delay_is_the_rounded_fraction_of_a_step() {
        assert_eq!(swing_delay_ticks(240, 0), 0);
        assert_eq!(swing_delay_ticks(240, 500), 120);
        assert_eq!(swing_delay_ticks(240, 750), 180);
        // 240 * 333 / 1000 = 79.92
        assert_eq!(swing_delay_ticks(240, 333), 80);
        // 240 * 1 / 1000 = 0.24 rounds down; 3 / 1000 * 240 = 0.72 rounds up.
        assert_eq!(swing_delay_ticks(240, 1), 0);
        assert_eq!(swing_delay_ticks(240, 3), 1);
        // Exactly one half rounds up: 100 * 5 / 1000 = 0.5.
        assert_eq!(swing_delay_ticks(100, 5), 1);
    }

    #[test]
    fn only_odd_grid_steps_move() {
        for step in 0..16u32 {
            let s = step * 240;
            let want = if step % 2 == 1 { s + 90 } else { s };
            assert_eq!(swung_start(s, 240, 375), want, "step {step}");
        }
        // Off-grid notes never move.
        assert_eq!(swung_start(240 + 1, 240, 375), 241);
        assert_eq!(swung_start(240, 240, 0), 240);
        assert_eq!(swung_start(240, 0, 500), 240);
    }

    #[test]
    fn ratchet_positions_match_the_closed_form() {
        for &r in &[1u8, 2, 3, 4, 6, 8] {
            for i in 0..r {
                let (off, len) = ratchet_part(240, r, i);
                assert_eq!(off as u64, i as u64 * 240 / r as u64, "r={r} i={i}");
                if r == 1 {
                    assert_eq!(len, 240);
                } else {
                    let sub = 240 / r as u32;
                    assert_eq!(len, sub * 9 / 10, "r={r}");
                }
            }
        }
        assert_eq!(ratchet_part(240, 8, 3), (90, 27));
        assert_eq!(ratchet_part(240, 3, 2), (160, 72));
        assert_eq!(ratchet_part(240, 6, 5), (200, 36));
        // Never shorter than one tick.
        assert_eq!(ratchet_part(8, 8, 1), (1, 1));
    }
}
