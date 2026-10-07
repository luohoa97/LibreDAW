// SPDX-License-Identifier: GPL-3.0-or-later
//! Offline loudness and balance analysis for the `analyze` job (16.3).
//!
//! Pure functions, our own code. Loudness follows ITU-R BS.1770-4:
//! K-weighting, 400 ms blocks with 75 % overlap, absolute gate at -70 LUFS
//! and relative gate 10 LU below the gated mean. True peak uses 4x
//! oversampling with a windowed-sinc interpolator.
//!
//! Silence has no logarithm: every level that would be minus infinity is
//! reported as `FLOOR_DB` so the JSON stays finite.

use std::f64::consts::PI;

use protocol::control::{Analysis, TrackLevels};

/// Reported for silence in any dB value.
pub const FLOOR_DB: f64 = -120.0;
const ABSOLUTE_GATE_LUFS: f64 = -70.0;
const RELATIVE_GATE_LU: f64 = -10.0;

/// Loudness, true peak, clipping, per-track levels and band balance.
/// `frames` is the stereo mix; `per_track` pairs a track id with its own
/// rendered frames. `revision` in the result is 0; the caller sets it.
pub fn analyze(frames: &[[f32; 2]], rate: u32, per_track: &[(u32, Vec<[f32; 2]>)]) -> Analysis {
    Analysis {
        revision: 0,
        integrated_lufs: integrated_loudness(frames, rate),
        true_peak_dbtp: true_peak_dbtp(frames),
        clipped_samples: clipped_samples(frames),
        tracks: per_track
            .iter()
            .map(|(track, f)| track_levels(*track, f))
            .collect(),
        band_balance: band_balance(frames, rate),
        bars: Vec::new(),
        sections: Vec::new(),
    }
}

fn to_db(power_or_amp_ratio: f64, is_power: bool) -> f64 {
    if power_or_amp_ratio <= 0.0 || !power_or_amp_ratio.is_finite() {
        return FLOOR_DB;
    }
    let k = if is_power { 10.0 } else { 20.0 };
    (k * power_or_amp_ratio.log10()).max(FLOOR_DB)
}

/// Direct form I biquad, `a0` already divided out.
#[derive(Clone, Copy)]
struct Biquad {
    b: [f64; 3],
    a: [f64; 2],
    x: [f64; 2],
    y: [f64; 2],
}

impl Biquad {
    fn new(b: [f64; 3], a: [f64; 2]) -> Biquad {
        Biquad {
            b,
            a,
            x: [0.0; 2],
            y: [0.0; 2],
        }
    }

    fn process(&mut self, x: f64) -> f64 {
        let y = self.b[0] * x + self.b[1] * self.x[0] + self.b[2] * self.x[1]
            - self.a[0] * self.y[0]
            - self.a[1] * self.y[1];
        self.x = [x, self.x[0]];
        self.y = [y, self.y[0]];
        y
    }

    /// Second-order Butterworth low-pass (Q = 1/sqrt 2).
    fn low_pass(f0: f64, rate: f64) -> Biquad {
        let (k, q) = ((PI * f0 / rate).tan(), std::f64::consts::FRAC_1_SQRT_2);
        let n = 1.0 + k / q + k * k;
        let b0 = k * k / n;
        Biquad::new(
            [b0, 2.0 * b0, b0],
            [2.0 * (k * k - 1.0) / n, (1.0 - k / q + k * k) / n],
        )
    }

    fn high_pass(f0: f64, rate: f64) -> Biquad {
        let (k, q) = ((PI * f0 / rate).tan(), std::f64::consts::FRAC_1_SQRT_2);
        let n = 1.0 + k / q + k * k;
        Biquad::new(
            [1.0 / n, -2.0 / n, 1.0 / n],
            [2.0 * (k * k - 1.0) / n, (1.0 - k / q + k * k) / n],
        )
    }
}

/// BS.1770 K-weighting: the high shelf (stage 1) and the RLB high-pass
/// (stage 2), derived for any sample rate from the analog prototype.
struct KWeight {
    shelf: Biquad,
    rlb: Biquad,
}

impl KWeight {
    fn new(rate: f64) -> KWeight {
        let (shelf, rlb) = k_weight_sections(rate);
        KWeight { shelf, rlb }
    }

    fn process(&mut self, x: f64) -> f64 {
        self.rlb.process(self.shelf.process(x))
    }
}

/// Shelf coefficients `(b, a1 a2)` for tests against the published
/// 48 kHz values in BS.1770-4.
#[cfg(test)]
pub(crate) fn shelf_coefficients(rate: f64) -> ([f64; 3], [f64; 2]) {
    let (shelf, _) = k_weight_sections(rate);
    (shelf.b, shelf.a)
}

fn k_weight_sections(rate: f64) -> (Biquad, Biquad) {
    {
        let (f0, gain_db, q) = (1681.974450955533, 3.999843853973347, 0.7071752369554196);
        let k = (PI * f0 / rate).tan();
        let vh = 10f64.powf(gain_db / 20.0);
        let vb = vh.powf(0.4996667741545416);
        let a0 = 1.0 + k / q + k * k;
        let shelf = Biquad::new(
            [
                (vh + vb * k / q + k * k) / a0,
                2.0 * (k * k - vh) / a0,
                (vh - vb * k / q + k * k) / a0,
            ],
            [2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
        );
        let (f0, q) = (38.13547087602444, 0.5003270373238773);
        let k = (PI * f0 / rate).tan();
        let a0 = 1.0 + k / q + k * k;
        let rlb = Biquad::new(
            [1.0, -2.0, 1.0],
            [2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
        );
        (shelf, rlb)
    }
}

fn block_loudness(z_sum: f64) -> f64 {
    if z_sum <= 0.0 {
        return f64::NEG_INFINITY;
    }
    -0.691 + 10.0 * z_sum.log10()
}

/// BS.1770-4 integrated loudness in LUFS (stereo, both channels weight 1).
/// Shorter than one 400 ms block: the whole signal counts as one block.
pub fn integrated_loudness(frames: &[[f32; 2]], rate: u32) -> f64 {
    if frames.is_empty() || rate == 0 {
        return FLOOR_DB;
    }
    let rate_f = f64::from(rate);
    let hop = ((0.1 * rate_f).round() as usize).max(1);
    let mut filters = [KWeight::new(rate_f), KWeight::new(rate_f)];
    // Energy per 100 ms hop and channel.
    let mut hops: Vec<[f64; 2]> = Vec::with_capacity(frames.len() / hop + 1);
    let mut acc = [0.0f64; 2];
    let mut in_hop = 0usize;
    for f in frames {
        for ch in 0..2 {
            let y = filters[ch].process(f64::from(f[ch]));
            acc[ch] += y * y;
        }
        in_hop += 1;
        if in_hop == hop {
            hops.push(acc);
            acc = [0.0; 2];
            in_hop = 0;
        }
    }
    // Mean square per block, summed over channels.
    let block_z: Vec<f64> = if hops.len() >= 4 {
        let len = (4 * hop) as f64;
        hops.windows(4)
            .map(|w| {
                let e: f64 = w.iter().map(|h| h[0] + h[1]).sum();
                e / len
            })
            .collect()
    } else {
        let mut total = [0.0f64; 2];
        for h in &hops {
            total[0] += h[0];
            total[1] += h[1];
        }
        total[0] += acc[0];
        total[1] += acc[1];
        vec![(total[0] + total[1]) / frames.len() as f64]
    };
    let above_abs: Vec<f64> = block_z
        .iter()
        .copied()
        .filter(|&z| block_loudness(z) > ABSOLUTE_GATE_LUFS)
        .collect();
    if above_abs.is_empty() {
        return FLOOR_DB;
    }
    let mean_abs = above_abs.iter().sum::<f64>() / above_abs.len() as f64;
    let relative_gate = block_loudness(mean_abs) + RELATIVE_GATE_LU;
    let gated: Vec<f64> = above_abs
        .into_iter()
        .filter(|&z| block_loudness(z) > relative_gate)
        .collect();
    if gated.is_empty() {
        return FLOOR_DB;
    }
    let mean = gated.iter().sum::<f64>() / gated.len() as f64;
    block_loudness(mean).max(FLOOR_DB)
}

const OVERSAMPLE: usize = 4;
const TAPS_PER_PHASE: usize = 24;

/// Interpolation filter for 4x oversampling: a Blackman-windowed sinc with
/// cutoff at the original Nyquist, `OVERSAMPLE * TAPS_PER_PHASE + 1` taps,
/// DC gain `OVERSAMPLE`. Phase 0 reproduces the input samples.
fn interpolation_filter() -> Vec<f64> {
    let n = OVERSAMPLE * TAPS_PER_PHASE;
    let c = n as f64 / 2.0;
    let mut h: Vec<f64> = (0..=n)
        .map(|i| {
            let t = (i as f64 - c) / OVERSAMPLE as f64;
            let sinc = if t == 0.0 {
                1.0
            } else {
                (PI * t).sin() / (PI * t)
            };
            let w = 0.42 - 0.5 * (2.0 * PI * i as f64 / n as f64).cos()
                + 0.08 * (4.0 * PI * i as f64 / n as f64).cos();
            sinc * w
        })
        .collect();
    // Each phase must have unity DC gain so a constant stays constant.
    for p in 0..OVERSAMPLE {
        let sum: f64 = h.iter().skip(p).step_by(OVERSAMPLE).sum();
        for v in h.iter_mut().skip(p).step_by(OVERSAMPLE) {
            *v /= sum;
        }
    }
    h
}

/// True peak in dBTP: the largest absolute value of the 4x oversampled
/// signal over both channels (BS.1770-4 annex 2).
pub fn true_peak_dbtp(frames: &[[f32; 2]]) -> f64 {
    let h = interpolation_filter();
    let c = (h.len() - 1) / 2;
    let reach = (c / OVERSAMPLE) as isize;
    let step = OVERSAMPLE as isize;
    let mut peak = 0.0f64;
    for ch in 0..2 {
        let x: Vec<f64> = frames.iter().map(|f| f64::from(f[ch])).collect();
        for &v in &x {
            peak = peak.max(v.abs());
        }
        // Output sample 4i+p is the sum over inputs j = i-d of
        // x[j] * h[c + 4d + p]; p = 0 is the input sample itself.
        for i in 0..x.len() as isize {
            for p in 1..step {
                let mut acc = 0.0;
                for d in -reach..=reach {
                    let idx = c as isize + step * d + p;
                    let j = i - d;
                    if idx >= 0 && (idx as usize) < h.len() && j >= 0 && (j as usize) < x.len() {
                        acc += x[j as usize] * h[idx as usize];
                    }
                }
                peak = peak.max(acc.abs());
            }
        }
    }
    to_db(peak, false)
}

fn clipped_samples(frames: &[[f32; 2]]) -> u64 {
    frames.iter().flatten().filter(|s| s.abs() >= 1.0).count() as u64
}

fn track_levels(track: u32, frames: &[[f32; 2]]) -> TrackLevels {
    let mut peak = 0.0f64;
    let mut sum_sq = 0.0f64;
    for s in frames.iter().flatten() {
        let v = f64::from(*s);
        peak = peak.max(v.abs());
        sum_sq += v * v;
    }
    let count = (frames.len() * 2).max(1) as f64;
    TrackLevels {
        track,
        peak_dbfs: to_db(peak, false),
        rms_dbfs: to_db((sum_sq / count).sqrt(), false),
    }
}

/// Two equal Butterworth sections in series make a Linkwitz-Riley 4th order.
fn cascade(pair: &mut [Biquad; 2], x: f64) -> f64 {
    let y = pair[0].process(x);
    pair[1].process(y)
}

/// Energy share below 250 Hz, from 250 Hz to 4 kHz, above 4 kHz, from
/// Linkwitz-Riley 4th-order crossovers. Sums to 1; silence gives zeros.
pub fn band_balance(frames: &[[f32; 2]], rate: u32) -> [f64; 3] {
    let r = f64::from(rate);
    if rate == 0 || frames.is_empty() {
        return [0.0; 3];
    }
    let mut energy = [0.0f64; 3];
    for ch in 0..2 {
        let mut lp_lo = [Biquad::low_pass(250.0, r); 2];
        let mut hp_lo = [Biquad::high_pass(250.0, r); 2];
        let mut lp_hi = [Biquad::low_pass(4000.0, r); 2];
        let mut hp_hi = [Biquad::high_pass(4000.0, r); 2];
        for f in frames {
            let x = f64::from(f[ch]);
            let low = cascade(&mut lp_lo, x);
            let rest = cascade(&mut hp_lo, x);
            let mid = cascade(&mut lp_hi, rest);
            let high = cascade(&mut hp_hi, rest);
            energy[0] += low * low;
            energy[1] += mid * mid;
            energy[2] += high * high;
        }
    }
    let total: f64 = energy.iter().sum();
    if total <= 0.0 {
        return [0.0; 3];
    }
    [energy[0] / total, energy[1] / total, energy[2] / total]
}
