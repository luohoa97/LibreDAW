// SPDX-License-Identifier: GPL-3.0-or-later
//! The per-channel low-pass a Filter shape drives (SPEC 24.2-1): a 12 dB per
//! octave Butterworth biquad on the channel's output, before its fader.
//! The shape value `v` in `0..1` maps to `20 * 1000^v` Hz (20 Hz to 20 kHz);
//! at `v >= OPEN` the filter is bypassed and its state cleared.

use std::f64::consts::PI;

/// Values from here up are treated as fully open.
pub const OPEN: f32 = 0.999;

#[derive(Clone, Copy, Debug, Default)]
pub struct ChanFilter {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z: [[f32; 2]; 2],
    last: f32,
}

impl ChanFilter {
    pub fn reset(&mut self) {
        *self = ChanFilter::default();
    }

    /// Sets the cutoff from the shape value (recomputes only on a change).
    pub fn set(&mut self, v: f32, sample_rate: f64) {
        if v == self.last && self.b0 != 0.0 {
            return;
        }
        self.last = v;
        let hz = (20.0 * 1000f64.powf(v.clamp(0.0, 1.0) as f64)).min(sample_rate * 0.45);
        let w = 2.0 * PI * hz / sample_rate;
        let (sn, cs) = w.sin_cos();
        let alpha = sn / (2.0 * std::f64::consts::FRAC_1_SQRT_2);
        let a0 = 1.0 + alpha;
        self.b0 = ((1.0 - cs) / 2.0 / a0) as f32;
        self.b1 = ((1.0 - cs) / a0) as f32;
        self.b2 = self.b0;
        self.a1 = (-2.0 * cs / a0) as f32;
        self.a2 = ((1.0 - alpha) / a0) as f32;
    }

    /// Filters one channel (`ch` 0 or 1) in place.
    pub fn process(&mut self, ch: usize, x: &mut [f32]) {
        let [mut z1, mut z2] = self.z[ch];
        for s in x.iter_mut() {
            let y = self.b0 * *s + z1;
            z1 = self.b1 * *s - self.a1 * y + z2;
            z2 = self.b2 * *s - self.a2 * y;
            *s = y;
        }
        self.z[ch] = [z1, z2];
    }
}
