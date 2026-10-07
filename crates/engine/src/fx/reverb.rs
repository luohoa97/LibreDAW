// SPDX-License-Identifier: GPL-3.0-or-later
//! Algorithmic reverb (SPEC 15.5): our own implementation of the classic
//! Schroeder/Moorer layout that Freeverb popularized: per channel eight
//! damped feedback combs in parallel into four series allpasses, with the
//! right channel's delay lengths offset for width, and a pre-delay.
//!
//! The delay lines are sized once for the stream rate. Reusing an entry
//! does not clear them: each line counts how many frames it has been
//! written since the reset and reads silence until it has wrapped once.

use super::{FADE_IN_FRAMES, fade_in};
use protocol::beats::ReverbParam;

const COMB_LEN: [usize; 8] = [1116, 1188, 1277, 1356, 1422, 1491, 1557, 1617];
const ALLPASS_LEN: [usize; 4] = [556, 441, 341, 225];
/// Right channel offset in frames at 44.1 kHz.
const SPREAD: usize = 23;
const FIXED_GAIN: f32 = 0.015;
const WET_SCALE: f32 = 3.0;
const DAMP_SCALE: f32 = 0.4;
const ROOM_SCALE: f32 = 0.28;
const ROOM_OFFSET: f32 = 0.7;
const ALLPASS_FEEDBACK: f32 = 0.5;
const DC: f32 = 1e-18;
/// Longest pre-delay, seconds (the parameter range is 200 ms).
const MAX_PREDELAY_SECONDS: f64 = 0.2;

/// A circular buffer that reads silence until it has been written once
/// around since the last reset.
#[derive(Clone)]
struct Line {
    buf: Vec<f32>,
    idx: usize,
    filled: usize,
}

impl Line {
    fn new(len: usize) -> Line {
        Line {
            buf: vec![0.0; len.max(1)],
            idx: 0,
            filled: 0,
        }
    }

    fn reset(&mut self) {
        self.idx = 0;
        self.filled = 0;
    }

    /// The oldest sample, `len` frames back.
    #[inline]
    fn oldest(&self) -> f32 {
        if self.filled < self.buf.len() {
            0.0
        } else {
            self.buf[self.idx]
        }
    }

    #[inline]
    fn push(&mut self, v: f32) {
        self.buf[self.idx] = v;
        self.idx += 1;
        if self.idx == self.buf.len() {
            self.idx = 0;
        }
        if self.filled < self.buf.len() {
            self.filled += 1;
        }
    }
}

#[derive(Clone)]
struct Comb {
    line: Line,
    store: f32,
}

#[derive(Clone)]
struct Side {
    combs: Vec<Comb>,
    allpass: Vec<Line>,
}

impl Side {
    fn new(sr: f64, extra: usize) -> Side {
        let scale = |n: usize| ((n + extra) as f64 * sr / 44100.0).round() as usize;
        Side {
            combs: COMB_LEN
                .iter()
                .map(|&n| Comb {
                    line: Line::new(scale(n)),
                    store: 0.0,
                })
                .collect(),
            allpass: ALLPASS_LEN.iter().map(|&n| Line::new(scale(n))).collect(),
        }
    }

    fn reset(&mut self) {
        for c in &mut self.combs {
            c.line.reset();
            c.store = 0.0;
        }
        for a in &mut self.allpass {
            a.reset();
        }
    }

    #[inline]
    fn run(&mut self, input: f32, feedback: f32, damp1: f32, damp2: f32) -> f32 {
        let mut out = 0.0;
        for c in &mut self.combs {
            let o = c.line.oldest();
            c.store = o * damp2 + c.store * damp1 + DC;
            c.line.push(input + c.store * feedback);
            out += o;
        }
        for a in &mut self.allpass {
            let b = a.oldest();
            let inp = out;
            a.push(inp + b * ALLPASS_FEEDBACK);
            out = b - inp;
        }
        out
    }
}

#[derive(Clone)]
pub struct Reverb {
    sr: f64,
    left: Side,
    right: Side,
    pre: Vec<f32>,
    pre_w: usize,
    pre_filled: usize,
    fade: u32,
}

impl Reverb {
    pub fn new(sr: f64) -> Reverb {
        Reverb {
            sr,
            left: Side::new(sr, 0),
            right: Side::new(sr, SPREAD),
            pre: vec![0.0; (MAX_PREDELAY_SECONDS * sr).ceil() as usize + 2],
            pre_w: 0,
            pre_filled: 0,
            fade: 0,
        }
    }

    pub fn reset(&mut self) {
        self.left.reset();
        self.right.reset();
        self.pre_w = 0;
        self.pre_filled = 0;
        self.fade = FADE_IN_FRAMES;
    }

    pub fn process(&mut self, p: &[f32], l: &mut [f32], r: &mut [f32]) {
        use ReverbParam::*;
        let size = p[Size.index()].clamp(0.0, 1.0);
        let feedback = size * ROOM_SCALE + ROOM_OFFSET;
        let damp1 = p[Damping.index()].clamp(0.0, 1.0) * DAMP_SCALE;
        let damp2 = 1.0 - damp1;
        let width = p[Width.index()].clamp(0.0, 1.0);
        let wet1 = width / 2.0 + 0.5;
        let wet2 = (1.0 - width) / 2.0;
        let mix = p[Mix.index()].clamp(0.0, 1.0);
        let pre = ((p[PredelayMs.index()].max(0.0) as f64 * 0.001 * self.sr).round() as usize)
            .min(self.pre.len() - 1);
        let plen = self.pre.len();
        fade_in(&mut self.fade, l, r);
        for i in 0..l.len() {
            let (dl, dr) = (l[i], r[i]);
            let mono = (dl + dr) * FIXED_GAIN;
            self.pre[self.pre_w] = mono;
            // Frame written `pre` frames ago (this frame when pre is 0).
            let delayed = if pre == 0 {
                mono
            } else if pre > self.pre_filled {
                0.0
            } else {
                self.pre[(self.pre_w + plen - pre) % plen]
            };
            self.pre_w = (self.pre_w + 1) % plen;
            self.pre_filled = (self.pre_filled + 1).min(plen - 1);
            let wl = self.left.run(delayed, feedback, damp1, damp2);
            let wr = self.right.run(delayed, feedback, damp1, damp2);
            let ol = (wl * wet1 + wr * wet2) * WET_SCALE;
            let or = (wr * wet1 + wl * wet2) * WET_SCALE;
            l[i] = dl * (1.0 - mix) + ol * mix;
            r[i] = dr * (1.0 - mix) + or * mix;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f64 = 48000.0;

    /// size, damping, width, predelay_ms, mix
    fn rp(size: f32, damp: f32, width: f32, pre: f32, mix: f32) -> [f32; 5] {
        [size, damp, width, pre, mix]
    }

    fn run_impulse(p: &[f32; 5], n: usize) -> (Vec<f32>, Vec<f32>) {
        let mut rv = Reverb::new(SR);
        // Let the fade-in pass on silence, then fire the impulse.
        let mut z = vec![0.0; 300];
        let mut z2 = z.clone();
        rv.process(p, &mut z, &mut z2);
        let mut l = vec![0.0; n];
        l[0] = 1.0;
        let mut r = l.clone();
        rv.process(p, &mut l, &mut r);
        (l, r)
    }

    fn energy(x: &[f32]) -> f64 {
        x.iter().map(|v| (*v as f64) * (*v as f64)).sum()
    }

    #[test]
    fn the_tail_is_finite_and_decays() {
        let (l, _) = run_impulse(&rp(0.5, 0.5, 1.0, 0.0, 1.0), 48000 * 6);
        assert!(l.iter().all(|x| x.is_finite()));
        let e0 = energy(&l[..24000]);
        let e1 = energy(&l[24000 * 3..24000 * 4]);
        let e2 = energy(&l[24000 * 9..24000 * 10]);
        assert!(e0 > 1e-6, "it rings");
        assert!(e1 < e0 * 0.5, "decays: {e1} vs {e0}");
        assert!(e2 < e1 * 0.5);
        // The largest, least damped room still decays.
        let (l, _) = run_impulse(&rp(1.0, 0.0, 1.0, 0.0, 1.0), 48000 * 12);
        assert!(l.iter().all(|x| x.is_finite() && x.abs() < 10.0));
        assert!(energy(&l[48000 * 11..]) < energy(&l[..48000]));
    }

    #[test]
    fn a_bigger_room_rings_longer() {
        let (small, _) = run_impulse(&rp(0.1, 0.5, 1.0, 0.0, 1.0), 96000);
        let (big, _) = run_impulse(&rp(0.9, 0.5, 1.0, 0.0, 1.0), 96000);
        assert!(energy(&big[48000..]) > energy(&small[48000..]) * 5.0);
    }

    #[test]
    fn predelay_and_mix_behave() {
        // 20 ms = 960 frames before anything arrives.
        let (l, _) = run_impulse(&rp(0.5, 0.5, 1.0, 20.0, 1.0), 4800);
        let first = l.iter().position(|x| x.abs() > 1e-9).unwrap();
        // The first comb (1116 frames at 44.1 kHz) is the earliest sound.
        let comb = (1116.0 * SR / 44100.0).round() as usize;
        assert!(
            first >= 960 + comb - 1 && first <= 960 + comb + 600,
            "{first}"
        );
        // mix 0 returns the dry impulse only.
        let (l, r) = run_impulse(&rp(0.5, 0.5, 1.0, 0.0, 0.0), 2000);
        assert_eq!(l[0], 1.0);
        assert!(l[1..].iter().all(|x| *x == 0.0));
        assert_eq!(l, r);
    }

    #[test]
    fn width_zero_is_mono_and_full_width_is_not() {
        let (l, r) = run_impulse(&rp(0.5, 0.5, 0.0, 0.0, 1.0), 6000);
        for (a, b) in l.iter().zip(&r) {
            assert!((a - b).abs() < 1e-6);
        }
        let (l, r) = run_impulse(&rp(0.5, 0.5, 1.0, 0.0, 1.0), 6000);
        assert!(l.iter().zip(&r).any(|(a, b)| (a - b).abs() > 1e-4));
    }

    #[test]
    fn reuse_is_silent_without_clearing_the_buffers() {
        let mut rv = Reverb::new(SR);
        let p = rp(0.9, 0.2, 1.0, 0.0, 1.0);
        let mut l = vec![0.5; 20000];
        let mut r = l.clone();
        rv.process(&p, &mut l, &mut r);
        rv.reset();
        let mut l = vec![0.0; 20000];
        let mut r = l.clone();
        rv.process(&p, &mut l, &mut r);
        assert!(l.iter().chain(&r).all(|x| x.abs() < 1e-12));
    }

    #[test]
    fn works_at_other_sample_rates() {
        for sr in [22050.0, 44100.0, 96000.0, 192000.0] {
            let mut rv = Reverb::new(sr);
            let mut l = vec![0.0; 4096];
            l[0] = 1.0;
            let mut r = l.clone();
            rv.process(&rp(0.5, 0.5, 1.0, 200.0, 0.5), &mut l, &mut r);
            assert!(l.iter().all(|x| x.is_finite()));
        }
    }
}
