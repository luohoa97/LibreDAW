// SPDX-License-Identifier: GPL-3.0-or-later
//! Helpers for unit tests only.
#![allow(dead_code)]

/// Frequency of a (possibly fractional) MIDI key.
pub fn key_hz(key: f64) -> f64 {
    440.0 * ((key - 69.0) / 12.0).exp2()
}

/// In-place radix-2 FFT of `(re, im)`; the length must be a power of two.
fn fft(re: &mut [f64], im: &mut [f64]) {
    let n = re.len();
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let ang = -std::f64::consts::TAU / len as f64;
        for s in (0..n).step_by(len) {
            for k in 0..len / 2 {
                let (wr, wi) = ((ang * k as f64).cos(), (ang * k as f64).sin());
                let (a, b) = (s + k, s + k + len / 2);
                let (tr, ti) = (re[b] * wr - im[b] * wi, re[b] * wi + im[b] * wr);
                re[b] = re[a] - tr;
                im[b] = im[a] - ti;
                re[a] += tr;
                im[a] += ti;
            }
        }
        len <<= 1;
    }
}

/// Spectral energy of `x` (Hann window, zero padded to a power of two)
/// between `lo` and `hi` Hz.
pub fn band_energy(x: &[f32], sr: f64, lo: f64, hi: f64) -> f64 {
    let n = x.len().next_power_of_two();
    let mut re = vec![0.0f64; n];
    let mut im = vec![0.0f64; n];
    let m = x.len();
    for (i, v) in x.iter().enumerate() {
        let w = 0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / m as f64).cos();
        re[i] = *v as f64 * w;
    }
    fft(&mut re, &mut im);
    (0..=n / 2)
        .filter(|&k| {
            let f = k as f64 * sr / n as f64;
            f >= lo && f <= hi
        })
        .map(|k| re[k] * re[k] + im[k] * im[k])
        .sum()
}

/// Magnitude (linear, relative to a unit sine) of the response of `x` at
/// `freq`, by a single-bin DFT over whole input.
pub fn tone_level(x: &[f32], sr: f64, freq: f64) -> f64 {
    let (mut re, mut im) = (0.0, 0.0);
    for (i, v) in x.iter().enumerate() {
        let a = std::f64::consts::TAU * freq * i as f64 / sr;
        re += *v as f64 * a.cos();
        im += *v as f64 * a.sin();
    }
    2.0 * (re * re + im * im).sqrt() / x.len() as f64
}
