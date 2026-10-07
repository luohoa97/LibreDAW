// SPDX-License-Identifier: GPL-3.0-or-later
//! Audio clips on Audio rows (SPEC 21.1): sample playback pinned to the
//! timeline, without stretching.
//!
//! A clip plays the sample from `offset` ticks in, starting at `start`, for
//! `len` ticks. Ticks become samples with the tempo: the sample frame heard
//! at musical position `t` is `round((t_seg - start + offset) * spt) + k`
//! where `t_seg` is the position at the start of an uninterrupted stretch of
//! the block (`Sequencer::segments`), `spt` the samples per tick and `k` the
//! frames since the stretch began. Nothing accumulates between blocks, so the
//! output does not depend on the callback size. After the sample's end the
//! clip is silent.

use crate::samples::SampleData;

/// A clip as the audio thread plays it.
#[derive(Clone, Debug, PartialEq)]
pub struct AudioClipC {
    pub start: u32,
    pub end: u32,
    pub offset: u32,
    /// Linear gain from `gain_mdb`.
    pub gain: f32,
    pub fade_in: u32,
    pub fade_out: u32,
    pub data: SampleData,
}

/// Linear gain of a clip's `gain_mdb` (thousandths of a dB).
pub fn mdb_to_lin(mdb: i32) -> f32 {
    10f32.powf(mdb as f32 / 20000.0)
}

/// Whether any clip overlaps the ticks `lo..hi`.
fn overlaps(clips: &[AudioClipC], lo: f64, hi: f64) -> bool {
    clips
        .iter()
        .any(|c| (c.start as f64) < hi && (c.end as f64) > lo)
}

/// Adds the clips to `l` and `r` (`n` frames). `segs` and `play_until` come
/// from the sequencer. Zeroes the buffers when it writes; returns whether
/// it did (false: untouched, nothing to hear).
pub fn render(
    clips: &[AudioClipC],
    segs: &[(u32, f64)],
    play_until: u32,
    spt: f64,
    l: &mut [f32],
    r: &mut [f32],
) -> bool {
    let n = l.len();
    let until = (play_until as usize).min(n);
    let mut touched = false;
    for (k, &(i0, t0)) in segs.iter().enumerate() {
        let i0 = i0 as usize;
        let i1 = segs.get(k + 1).map_or(until, |s| s.0 as usize).min(until);
        if i1 <= i0 {
            continue;
        }
        let t_end = t0 + (i1 - i0) as f64 / spt;
        if !overlaps(clips, t0, t_end) {
            continue;
        }
        if !touched {
            l.fill(0.0);
            r.fill(0.0);
            touched = true;
        }
        for c in clips {
            let (cs, ce) = (c.start as f64, c.end as f64);
            if cs >= t_end || ce <= t0 {
                continue;
            }
            // First and one-past-last frame (relative to i0) whose position
            // is inside the clip; the epsilon keeps a clip that starts on a
            // frame from losing it to rounding.
            let a = (((cs - t0) * spt - 1e-6).ceil().max(0.0)) as usize;
            let b = ((((ce - t0) * spt - 1e-6).ceil().max(0.0)) as usize).min(i1 - i0);
            if a >= b {
                continue;
            }
            let base = ((t0 - cs + c.offset as f64) * spt).round() as i64;
            let ch = c.data.channels.max(1) as usize;
            let frames = c.data.frames() as i64;
            let fades = c.fade_in > 0 || c.fade_out > 0;
            for j in a..b {
                let src = base + j as i64;
                if src < 0 {
                    continue;
                }
                if src >= frames {
                    break;
                }
                let mut g = c.gain;
                if fades {
                    let tick = t0 + j as f64 / spt;
                    if c.fade_in > 0 {
                        g *= ((tick - cs) / c.fade_in as f64).clamp(0.0, 1.0) as f32;
                    }
                    if c.fade_out > 0 {
                        g *= ((ce - tick) / c.fade_out as f64).clamp(0.0, 1.0) as f32;
                    }
                }
                let at = src as usize * ch;
                let (xl, xr) = if ch == 1 {
                    let m = c.data.data[at];
                    (m, m)
                } else {
                    (c.data.data[at], c.data.data[at + 1])
                };
                l[i0 + j] += xl * g;
                r[i0 + j] += xr * g;
            }
        }
    }
    touched
}
