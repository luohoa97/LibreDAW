// SPDX-License-Identifier: GPL-3.0-or-later
//! Sample-rate conversion to the model rate (windowed sinc, Hann window).

pub const MODEL_RATE: u32 = 22050;

/// Half-width of the sinc kernel in zero crossings.
const ZERO_CROSSINGS: f64 = 16.0;

pub fn to_model_rate(input: &[f32], rate: u32) -> Vec<f32> {
    if rate == MODEL_RATE {
        return input.to_vec();
    }
    let ratio = rate as f64 / MODEL_RATE as f64; // input samples per output sample
    let n_out = (input.len() as f64 / ratio).floor() as usize;
    // Low-pass at the lower of the two Nyquists.
    let cutoff = (1.0 / ratio).min(1.0);
    let half = ZERO_CROSSINGS / cutoff;
    let mut out = Vec::with_capacity(n_out);
    for i in 0..n_out {
        let t = i as f64 * ratio;
        let lo = ((t - half).ceil().max(0.0)) as usize;
        let hi = ((t + half).floor() as usize).min(input.len().saturating_sub(1));
        let mut acc = 0.0f64;
        for (j, &x) in input.iter().enumerate().take(hi + 1).skip(lo) {
            let d = j as f64 - t;
            let a = d * cutoff;
            let sinc = if a.abs() < 1e-9 {
                1.0
            } else {
                (std::f64::consts::PI * a).sin() / (std::f64::consts::PI * a)
            };
            let w = 0.5 * (1.0 + (std::f64::consts::PI * d / half).cos());
            acc += x as f64 * sinc * w;
        }
        out.push((acc * cutoff) as f32);
    }
    out
}
