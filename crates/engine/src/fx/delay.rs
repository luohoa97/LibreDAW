// SPDX-License-Identifier: GPL-3.0-or-later
//! Tempo-synced stereo delay (SPEC 15.5, 17.2): up to 4 seconds, feedback
//! with a tone lowpass, optional ping-pong, smoothed delay time.
//!
//! The buffers are sized for the stream rate when the `Runtime` is built.
//! Reusing an entry never clears them: `filled` counts the frames written
//! since the reset, and a read from further back than that returns silence,
//! so the clear costs nothing.

use super::{FADE_IN_FRAMES, fade_in};
use protocol::beats::DelayParam;

const TAU: f64 = std::f64::consts::TAU;
const DC: f32 = 1e-18;
/// Longest delay (17.2).
pub const MAX_DELAY_SECONDS: f64 = 4.0;
/// Time the delay length takes to slide to a new value.
const SLEW_SECONDS: f64 = 0.05;

#[derive(Clone)]
pub struct Delay {
    sr: f64,
    buf: [Vec<f32>; 2],
    /// Next write position.
    w: usize,
    /// Frames written since the reset, saturating at the buffer length.
    filled: usize,
    /// Current delay in frames (slides toward the target).
    cur: f64,
    have_cur: bool,
    lp: [f32; 2],
    fade: u32,
}

impl Delay {
    pub fn new(sr: f64) -> Delay {
        let len = (MAX_DELAY_SECONDS * sr).ceil() as usize + 4;
        Delay {
            sr,
            buf: [vec![0.0; len], vec![0.0; len]],
            w: 0,
            filled: 0,
            cur: 0.0,
            have_cur: false,
            lp: [0.0; 2],
            fade: 0,
        }
    }

    pub fn reset(&mut self) {
        self.w = 0;
        self.filled = 0;
        self.have_cur = false;
        self.lp = [0.0; 2];
        self.fade = FADE_IN_FRAMES;
    }

    /// Delay in frames for a time in beats at `bpm`, capped at 4 s.
    pub fn target_frames(sr: f64, beats: f64, bpm: f64) -> f64 {
        (beats * 60.0 / bpm.max(1.0)).min(MAX_DELAY_SECONDS) * sr
    }

    #[inline]
    fn read(&self, ch: usize, d: f64) -> f32 {
        let len = self.buf[ch].len();
        // Linear interpolation between the two frames around `w - d`.
        let i = d.floor();
        let frac = (d - i) as f32;
        let i = i as usize;
        let at = |k: usize| -> f32 {
            if k > self.filled || k == 0 || k >= len {
                0.0
            } else {
                self.buf[ch][(self.w + len - k) % len]
            }
        };
        let a = at(i.max(1));
        let b = at(i + 1);
        a + (b - a) * frac
    }

    pub fn process(&mut self, ping_pong: bool, bpm: f64, p: &[f32], l: &mut [f32], r: &mut [f32]) {
        use DelayParam::*;
        let target = Delay::target_frames(self.sr, p[TimeBeats.index()] as f64, bpm).max(2.0);
        if !self.have_cur {
            self.cur = target;
            self.have_cur = true;
        }
        let slew = (target - self.cur).abs() / (SLEW_SECONDS * self.sr).max(1.0);
        let fb = p[Feedback.index()].clamp(0.0, 0.95);
        let a = (1.0
            - (-TAU * (p[ToneHz.index()] as f64).clamp(20.0, self.sr * 0.45) / self.sr).exp())
            as f32;
        let mix = p[Mix.index()].clamp(0.0, 1.0);
        let len = self.buf[0].len();
        fade_in(&mut self.fade, l, r);
        for i in 0..l.len() {
            // Slide the delay length toward the target.
            if self.cur < target {
                self.cur = (self.cur + slew).min(target);
            } else if self.cur > target {
                self.cur = (self.cur - slew).max(target);
            }
            let d = self.cur;
            let (dl, dr) = (self.read(0, d), self.read(1, d));
            self.lp[0] += a * (dl - self.lp[0]) + DC;
            self.lp[1] += a * (dr - self.lp[1]) + DC;
            let (il, ir) = (l[i], r[i]);
            let (wl, wr) = if ping_pong {
                (0.5 * (il + ir) + fb * self.lp[1], fb * self.lp[0])
            } else {
                (il + fb * self.lp[0], ir + fb * self.lp[1])
            };
            self.buf[0][self.w] = wl;
            self.buf[1][self.w] = wr;
            self.w = (self.w + 1) % len;
            self.filled = (self.filled + 1).min(len - 1);
            l[i] = il * (1.0 - mix) + dl * mix;
            r[i] = ir * (1.0 - mix) + dr * mix;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f64 = 48000.0;
    const BPM: f64 = 120.0;

    /// time_beats, feedback, tone_hz, mix
    fn dp(beats: f32, fb: f32, mix: f32) -> [f32; 4] {
        [beats, fb, 20000.0, mix]
    }

    fn impulse(n: usize) -> (Vec<f32>, Vec<f32>) {
        let mut l = vec![0.0; n];
        l[0] = 1.0;
        (l.clone(), l)
    }

    fn first_above(x: &[f32], from: usize, thr: f32) -> Option<usize> {
        (from..x.len()).find(|&i| x[i].abs() > thr)
    }

    #[test]
    fn echoes_land_on_the_tempo_grid() {
        // 0.5 beat at 120 BPM = 250 ms = 12000 frames.
        let mut d = Delay::new(SR);
        d.reset();
        let (mut l, mut r) = impulse(48000);
        // Wait out the 256 frame fade-in: start the impulse after it.
        let mut warm_l = vec![0.0; 300];
        let mut warm_r = vec![0.0; 300];
        d.process(false, BPM, &dp(0.5, 0.5, 1.0), &mut warm_l, &mut warm_r);
        d.process(false, BPM, &dp(0.5, 0.5, 1.0), &mut l, &mut r);
        let first = first_above(&l, 1, 0.1).unwrap();
        assert_eq!(first, 12000);
        assert!((l[first] - 1.0).abs() < 1e-6, "mix 1 returns only the echo");
        let second = first_above(&l, first + 1, 0.1).unwrap();
        assert_eq!(second, 24000);
        // The feedback path has the tone lowpass: 20 kHz at 48 kHz passes
        // 1 - exp(-2 pi 20000 / 48000) of a single-frame echo at once.
        let a = 1.0 - (-TAU * 20000.0 / SR).exp();
        assert!(
            (l[second] as f64 - 0.5 * a).abs() < 1e-5,
            "feedback 0.5: {}",
            l[second]
        );
        assert_eq!(l, r);
    }

    #[test]
    fn ping_pong_alternates_sides() {
        let mut d = Delay::new(SR);
        let (mut l, mut r) = impulse(48000);
        d.process(true, BPM, &dp(0.25, 0.6, 1.0), &mut l, &mut r);
        // 0.25 beat = 6000 frames: left, then right, then left.
        let e1 = first_above(&l, 1, 0.05).unwrap();
        assert_eq!(e1, 6000);
        assert!(r[e1].abs() < 1e-6);
        let e2 = first_above(&r, 1, 0.05).unwrap();
        assert_eq!(e2, 12000);
        assert!(l[e2].abs() < 1e-6);
        let e3 = first_above(&l, e1 + 1, 0.05).unwrap();
        assert_eq!(e3, 18000);
    }

    #[test]
    fn dry_mix_zero_and_tempo_cap() {
        let mut d = Delay::new(SR);
        let (mut l, mut r) = impulse(1000);
        let x = l.clone();
        d.process(false, BPM, &dp(1.0, 0.0, 0.0), &mut l, &mut r);
        assert_eq!(l[1..], x[1..]);
        // 4 beats at 20 BPM is 12 s; the delay is capped at 4 s.
        assert_eq!(Delay::target_frames(SR, 4.0, 20.0), 4.0 * SR);
        assert_eq!(Delay::target_frames(SR, 1.0, 120.0), 0.5 * SR);
    }

    #[test]
    fn a_time_change_slides_without_a_jump() {
        let mut d = Delay::new(SR);
        // A steady sine through a 0.25 beat delay, then 0.5 beat.
        let n = 24000;
        let s: Vec<f32> = (0..n)
            .map(|i| (TAU * 220.0 * i as f64 / SR).sin() as f32)
            .collect();
        let (mut l, mut r) = (s.clone(), s.clone());
        d.process(false, BPM, &dp(0.25, 0.0, 1.0), &mut l, &mut r);
        let (mut l2, mut r2) = (s.clone(), s.clone());
        d.process(false, BPM, &dp(0.5, 0.0, 1.0), &mut l2, &mut r2);
        // The output stays continuous: no sample-to-sample step above what
        // the sine and the pitch glide of the slide allow.
        let worst = l2
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0f32, f32::max);
        assert!(worst < 0.1, "{worst}");
    }

    #[test]
    fn reuse_starts_silent_without_clearing() {
        let mut d = Delay::new(SR);
        let (mut l, mut r) = impulse(2000);
        d.process(false, BPM, &dp(0.0625, 0.9, 1.0), &mut l, &mut r);
        d.reset();
        let mut l = vec![0.0; 24000];
        let mut r = vec![0.0; 24000];
        d.process(false, BPM, &dp(0.0625, 0.9, 1.0), &mut l, &mut r);
        let bad = l.iter().position(|x| x.abs() > 1e-12);
        assert!(
            bad.is_none(),
            "old echoes are gone: {:?} {:?}",
            bad,
            bad.map(|i| l[i])
        );
    }

    #[test]
    fn filled_saturates_and_feedback_is_stable() {
        let mut d = Delay::new(SR);
        let (mut l, mut r) = impulse(48000 * 6);
        d.process(false, BPM, &dp(0.0625, 0.95, 1.0), &mut l, &mut r);
        assert!(l.iter().all(|x| x.is_finite() && x.abs() <= 1.0));
        assert!(l[48000 * 6 - 1000..].iter().all(|x| x.abs() < 0.01));
    }
}
