// SPDX-License-Identifier: GPL-3.0-or-later
//! Compressor and limiter (SPEC 15.5, 17.2).
//!
//! The compressor is feed-forward with a stereo-linked peak detector, a soft
//! knee and gain smoothing in dB. Its key is the input unless a sidechain
//! key is passed. The limiter has zero latency: instant attack on the
//! sample peak, exponential release, no lookahead.

use super::{FADE_IN_FRAMES, fade_in};
use protocol::beats::{CompressorParam, LimiterParam};

const DC: f32 = 1e-18;
/// Peak detector fall time constant.
const PEAK_DECAY_SECONDS: f64 = 0.005;
const SILENCE_DB: f32 = -120.0;

/// Static compressor curve: output level in dB for an input level in dB
/// (threshold, ratio, soft knee width). Pure; the tests compare against it.
pub fn compressor_curve_db(x: f32, threshold: f32, ratio: f32, knee: f32) -> f32 {
    let over = x - threshold;
    if knee <= 0.0 {
        return if over <= 0.0 {
            x
        } else {
            threshold + over / ratio
        };
    }
    if 2.0 * over < -knee {
        x
    } else if 2.0 * over.abs() <= knee {
        let t = over + knee / 2.0;
        x + (1.0 / ratio - 1.0) * t * t / (2.0 * knee)
    } else {
        threshold + over / ratio
    }
}

#[inline]
fn to_db(x: f32) -> f32 {
    if x <= 1e-6 {
        SILENCE_DB
    } else {
        20.0 * x.log10()
    }
}

#[inline]
fn from_db(x: f32) -> f32 {
    10f32.powf(x / 20.0)
}

fn time_coef(ms: f32, sr: f64) -> f32 {
    (-1.0 / (ms.max(0.001) as f64 * 0.001 * sr)).exp() as f32
}

#[derive(Clone)]
pub struct Compressor {
    sr: f64,
    peak: f32,
    /// Smoothed gain reduction in dB, 0 or negative.
    gr_db: f32,
    fade: u32,
}

impl Compressor {
    pub fn new(sr: f64) -> Compressor {
        Compressor {
            sr,
            peak: 0.0,
            gr_db: 0.0,
            fade: 0,
        }
    }

    pub fn reset(&mut self) {
        self.peak = 0.0;
        self.gr_db = 0.0;
        self.fade = FADE_IN_FRAMES;
    }

    /// Current gain reduction in dB (0 or negative), for meters and tests.
    pub fn gain_reduction_db(&self) -> f32 {
        self.gr_db
    }

    /// Processes `l`/`r` in place. `key` is the sidechain signal (same
    /// length) or `None` to key from the input.
    pub fn process(
        &mut self,
        p: &[f32],
        l: &mut [f32],
        r: &mut [f32],
        key: Option<(&[f32], &[f32])>,
    ) {
        use CompressorParam::*;
        let thr = p[ThresholdDb.index()];
        let ratio = p[Ratio.index()].max(1.0);
        let knee = p[KneeDb.index()].max(0.0);
        let makeup = p[MakeupDb.index()];
        let mix = p[Mix.index()].clamp(0.0, 1.0);
        let att = time_coef(p[AttackMs.index()], self.sr);
        let rel = time_coef(p[ReleaseMs.index()], self.sr);
        let decay = (-1.0 / (PEAK_DECAY_SECONDS * self.sr)).exp() as f32;
        fade_in(&mut self.fade, l, r);
        for i in 0..l.len() {
            let (kl, kr) = match key {
                Some((a, b)) => (a[i], b[i]),
                None => (l[i], r[i]),
            };
            let level = kl.abs().max(kr.abs());
            self.peak = level.max(self.peak * decay);
            let x = to_db(self.peak);
            let target = compressor_curve_db(x, thr, ratio, knee) - x;
            let c = if target < self.gr_db { att } else { rel };
            self.gr_db = target + (self.gr_db - target) * c + DC;
            let g = from_db(self.gr_db + makeup);
            let (dl, dr) = (l[i], r[i]);
            l[i] = dl * (1.0 - mix) + dl * g * mix;
            r[i] = dr * (1.0 - mix) + dr * g * mix;
        }
    }
}

#[derive(Clone)]
pub struct Limiter {
    sr: f64,
    gain: f32,
    fade: u32,
}

impl Limiter {
    pub fn new(sr: f64) -> Limiter {
        Limiter {
            sr,
            gain: 1.0,
            fade: 0,
        }
    }

    pub fn reset(&mut self) {
        self.gain = 1.0;
        self.fade = FADE_IN_FRAMES;
    }

    pub fn gain(&self) -> f32 {
        self.gain
    }

    pub fn process(&mut self, p: &[f32], l: &mut [f32], r: &mut [f32]) {
        let ceil = from_db(p[LimiterParam::CeilingDb.index()].min(0.0));
        let rel = time_coef(p[LimiterParam::ReleaseMs.index()], self.sr);
        fade_in(&mut self.fade, l, r);
        for i in 0..l.len() {
            let peak = l[i].abs().max(r[i].abs());
            let need = if peak > ceil { ceil / peak } else { 1.0 };
            self.gain = if need < self.gain {
                need
            } else {
                // Recover toward 1 with the release time constant.
                (1.0 - (1.0 - self.gain) * rel + DC).min(1.0).min(need)
            };
            l[i] *= self.gain;
            r[i] *= self.gain;
            // Rounding in the product must never poke through the ceiling.
            l[i] = l[i].clamp(-ceil, ceil);
            r[i] = r[i].clamp(-ceil, ceil);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::tone_level;

    const SR: f64 = 48000.0;

    /// threshold, ratio, attack, release, knee, makeup, mix
    fn cp(thr: f32, ratio: f32, knee: f32) -> [f32; 7] {
        [thr, ratio, 1.0, 100.0, knee, 0.0, 1.0]
    }

    #[test]
    fn the_static_curve_matches_hand_computed_points() {
        // Hard knee, -20 dB threshold, 4:1.
        assert_eq!(compressor_curve_db(-30.0, -20.0, 4.0, 0.0), -30.0);
        assert_eq!(compressor_curve_db(-20.0, -20.0, 4.0, 0.0), -20.0);
        assert_eq!(compressor_curve_db(-8.0, -20.0, 4.0, 0.0), -17.0);
        assert_eq!(compressor_curve_db(0.0, -20.0, 10.0, 0.0), -18.0);
        // 6 dB soft knee: at the threshold the curve is 0.75 dB below the
        // line: (1/4 - 1) * 3^2 / (2*6) = -0.5625.
        let y = compressor_curve_db(-20.0, -20.0, 4.0, 6.0);
        assert!((y - (-20.5625)).abs() < 1e-5, "{y}");
        // The knee meets the straight parts continuously.
        let lo = compressor_curve_db(-23.0, -20.0, 4.0, 6.0);
        assert!((lo - -23.0).abs() < 1e-5);
        let hi = compressor_curve_db(-17.0, -20.0, 4.0, 6.0);
        assert!((hi - (-20.0 + 3.0 / 4.0)).abs() < 1e-5);
    }

    fn square(amp: f32, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| if (i / 240) % 2 == 0 { amp } else { -amp })
            .collect()
    }

    #[test]
    fn steady_gain_reduction_equals_the_curve() {
        let mut c = Compressor::new(SR);
        let amp = from_db(-8.0);
        let mut l = square(amp, 48000);
        let mut r = l.clone();
        c.process(&cp(-20.0, 4.0, 0.0), &mut l, &mut r, None);
        // -8 dB in, -17 dB out: 9 dB of gain reduction.
        assert!(
            (c.gain_reduction_db() + 9.0).abs() < 0.02,
            "{}",
            c.gain_reduction_db()
        );
        let out = l[47000..].iter().fold(0f32, |m, v| m.max(v.abs()));
        assert!((to_db(out) + 17.0).abs() < 0.02, "{}", to_db(out));
        // Below threshold nothing changes.
        let mut c = Compressor::new(SR);
        let x = square(from_db(-30.0), 4800);
        let (mut l, mut r) = (x.clone(), x.clone());
        c.process(&cp(-20.0, 4.0, 0.0), &mut l, &mut r, None);
        assert_eq!(l, x);
    }

    #[test]
    fn makeup_and_mix_apply() {
        let mut c = Compressor::new(SR);
        let x = square(from_db(-30.0), 4800);
        let (mut l, mut r) = (x.clone(), x.clone());
        let mut p = cp(-20.0, 4.0, 0.0);
        p[5] = 6.0;
        c.process(&p, &mut l, &mut r, None);
        assert!((to_db(l[4000].abs()) - to_db(x[4000].abs()) - 6.0).abs() < 0.01);
        // 50 percent mix of a 6 dB boost: (1 + 1.995) / 2.
        let mut c = Compressor::new(SR);
        let (mut l, mut r) = (x.clone(), x.clone());
        p[6] = 0.5;
        c.process(&p, &mut l, &mut r, None);
        let want = x[4000] * (1.0 + from_db(6.0)) / 2.0;
        assert!((l[4000] - want).abs() < 1e-5);
    }

    #[test]
    fn attack_and_release_follow_their_time_constants() {
        let mut c = Compressor::new(SR);
        let mut p = cp(-20.0, 20.0, 0.0);
        p[2] = 10.0; // attack
        p[3] = 200.0; // release
        let loud = square(from_db(-2.0), 24000);
        let (mut l, mut r) = (loud.clone(), loud);
        c.process(&p, &mut l, &mut r, None);
        let full = c.gain_reduction_db();
        assert!(full < -15.0);
        // Silence: after one release time constant 1/e of the reduction left.
        let n = (0.2 * SR) as usize;
        let mut l = vec![0.0; n];
        let mut r = vec![0.0; n];
        c.process(&p, &mut l, &mut r, None);
        // The peak detector still holds for 5 ms per e-fold, so allow a
        // little: 63 percent of the way back after 200 ms.
        let back = 1.0 - c.gain_reduction_db() / full;
        assert!(back > 0.55 && back < 0.75, "recovered {back}");
    }

    #[test]
    fn a_sidechain_key_drives_the_reduction() {
        // The program is quiet (-30 dB, under the threshold); the key is loud.
        let prog = square(from_db(-30.0), 24000);
        let key = square(from_db(-2.0), 24000);
        let mut c = Compressor::new(SR);
        let (mut l, mut r) = (prog.clone(), prog.clone());
        c.process(&cp(-20.0, 4.0, 0.0), &mut l, &mut r, Some((&key, &key)));
        assert!(c.gain_reduction_db() < -10.0);
        assert!(l[23000].abs() < prog[23000].abs() * 0.4);
        // Without the key the same program is untouched.
        let mut c = Compressor::new(SR);
        let (mut l, mut r) = (prog.clone(), prog.clone());
        c.process(&cp(-20.0, 4.0, 0.0), &mut l, &mut r, None);
        assert_eq!(l, prog);
    }

    #[test]
    fn the_limiter_holds_the_ceiling_with_zero_latency() {
        let mut lim = Limiter::new(SR);
        let ceil_db = -1.0;
        let ceil = from_db(ceil_db);
        // 1 ms of silence, then a +6 dB burst starting at frame 48.
        let n = 4800;
        let mut l: Vec<f32> = (0..n)
            .map(|i| {
                if i < 48 {
                    0.0
                } else {
                    2.0 * (i as f32 * 0.3).sin()
                }
            })
            .collect();
        let mut r = l.clone();
        let src = l.clone();
        lim.process(&[ceil_db, 80.0], &mut l, &mut r);
        assert!(l.iter().all(|v| v.abs() <= ceil), "never above the ceiling");
        // Zero latency: the first loud sample is already limited, in place.
        let first = (48..n).find(|&i| src[i].abs() > ceil).unwrap();
        assert!(l[first].abs() <= ceil && l[first] != 0.0);
        assert_eq!(l[..48], src[..48]);
        let peak = l.iter().fold(0f32, |m, v| m.max(v.abs()));
        assert!(
            (peak - ceil).abs() < 1e-6,
            "peaks sit on the ceiling: {peak}"
        );
        // Quiet material passes bit-exact.
        let mut lim = Limiter::new(SR);
        let x: Vec<f32> = (0..2000).map(|i| 0.3 * (i as f32 * 0.05).sin()).collect();
        let (mut l, mut r) = (x.clone(), x.clone());
        lim.process(&[ceil_db, 80.0], &mut l, &mut r);
        assert_eq!(l, x);
    }

    #[test]
    fn the_limiter_releases_exponentially() {
        let mut lim = Limiter::new(SR);
        let mut l = vec![2.0f32];
        let mut r = vec![2.0f32];
        lim.process(&[0.0, 100.0], &mut l, &mut r);
        assert!((lim.gain() - 0.5).abs() < 1e-6);
        // 100 ms of near silence: gain returns 1 - 0.5/e.
        let n = (0.1 * SR) as usize;
        let mut l = vec![0.01f32; n];
        let mut r = l.clone();
        lim.process(&[0.0, 100.0], &mut l, &mut r);
        let want = 1.0 - 0.5 / std::f32::consts::E.powf(1.0 / 1.0);
        assert!((lim.gain() - want).abs() < 0.01, "{} vs {want}", lim.gain());
    }

    #[test]
    fn a_tone_level_helper_sanity() {
        let x: Vec<f32> = (0..4800)
            .map(|i| (std::f64::consts::TAU * 1000.0 * i as f64 / SR).sin() as f32)
            .collect();
        assert!((tone_level(&x, SR, 1000.0) - 1.0).abs() < 1e-3);
    }
}
