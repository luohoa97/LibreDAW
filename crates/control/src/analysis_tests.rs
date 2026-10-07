// SPDX-License-Identifier: GPL-3.0-or-later
//! Reference-signal tests for `analysis`: BS.1770 / EBU Tech 3341 style
//! sine vectors generated here.

use std::f64::consts::PI;

use protocol::control::Analysis;

use crate::analysis::*;

const RATE: u32 = 48_000;

/// Stereo sine; `gains` are linear amplitudes for left and right.
fn sine(freq: f64, gains: [f64; 2], secs: f64, phase: f64, rate: u32) -> Vec<[f32; 2]> {
    let n = (secs * f64::from(rate)) as usize;
    (0..n)
        .map(|i| {
            let s = (2.0 * PI * freq * i as f64 / f64::from(rate) + phase).sin();
            [(s * gains[0]) as f32, (s * gains[1]) as f32]
        })
        .collect()
}

fn db(a: f64) -> f64 {
    10f64.powf(a / 20.0)
}

#[test]
fn mono_sine_at_minus_20_dbfs_is_minus_23_lufs() {
    // One channel only: half the power of a stereo pair, -3.01 LU.
    let x = sine(1000.0, [db(-20.0), 0.0], 5.0, 0.0, RATE);
    let l = integrated_loudness(&x, RATE);
    assert!((l - -23.0).abs() < 0.1, "{l}");
}

#[test]
fn stereo_sine_at_minus_23_dbfs_is_minus_23_lufs() {
    let x = sine(1000.0, [db(-23.0); 2], 5.0, 0.0, RATE);
    let l = integrated_loudness(&x, RATE);
    assert!((l - -23.0).abs() < 0.1, "{l}");
}

#[test]
fn loudness_works_at_44100_and_96000() {
    for rate in [44_100, 96_000] {
        let x = sine(1000.0, [db(-23.0); 2], 4.0, 0.0, rate);
        let l = integrated_loudness(&x, rate);
        assert!((l - -23.0).abs() < 0.1, "{rate}: {l}");
    }
}

#[test]
fn k_weighting_shapes_the_spectrum() {
    // BS.1770 K-curve: about +3.3 dB near 10 kHz (rel. 1 kHz), strong cut at 50 Hz.
    let ref_l = integrated_loudness(&sine(1000.0, [0.1; 2], 4.0, 0.0, RATE), RATE);
    let hi = integrated_loudness(&sine(10_000.0, [0.1; 2], 4.0, 0.0, RATE), RATE);
    let lo = integrated_loudness(&sine(50.0, [0.1; 2], 4.0, 0.0, RATE), RATE);
    assert!((hi - ref_l - 3.3).abs() < 0.3, "{}", hi - ref_l);
    assert!(lo - ref_l < -1.0, "{}", lo - ref_l);
}

#[test]
fn quiet_passages_are_gated_out() {
    // EBU Tech 3341 style: a loud body with quiet parts that fall below
    // the relative gate; the quiet parts must not move the result.
    let mut x = sine(1000.0, [db(-46.0); 2], 10.0, 0.0, RATE);
    x.extend(sine(1000.0, [db(-23.0); 2], 20.0, 0.0, RATE));
    x.extend(sine(1000.0, [db(-46.0); 2], 10.0, 0.0, RATE));
    let l = integrated_loudness(&x, RATE);
    assert!((l - -23.0).abs() < 0.1, "{l}");
}

#[test]
fn absolute_gate_drops_near_silence() {
    let mut x = sine(1000.0, [db(-80.0); 2], 5.0, 0.0, RATE);
    x.extend(sine(1000.0, [db(-20.0); 2], 5.0, 0.0, RATE));
    let l = integrated_loudness(&x, RATE);
    assert!((l - -20.0).abs() < 0.3, "{l}");
}

#[test]
fn silence_and_empty_report_the_floor() {
    assert_eq!(integrated_loudness(&vec![[0.0; 2]; 48_000], RATE), FLOOR_DB);
    assert_eq!(integrated_loudness(&[], RATE), FLOOR_DB);
    assert_eq!(true_peak_dbtp(&[]), FLOOR_DB);
    assert_eq!(band_balance(&vec![[0.0; 2]; 100], RATE), [0.0; 3]);
}

#[test]
fn shorter_than_one_block_still_measures() {
    let x = sine(1000.0, [db(-23.0); 2], 0.3, 0.0, RATE);
    let l = integrated_loudness(&x, RATE);
    assert!((l - -23.0).abs() < 0.5, "{l}");
}

#[test]
fn full_scale_sine_true_peak_is_zero_dbtp() {
    let x = sine(1000.0, [1.0; 2], 1.0, 0.3, RATE);
    let tp = true_peak_dbtp(&x);
    assert!(tp.abs() < 0.1, "{tp}");
}

#[test]
fn inter_sample_peak_is_found() {
    // fs/4 at 45 degrees: every sample is +-0.7071 (-3.01 dBFS) but the
    // waveform between them reaches 1.0.
    let x = sine(12_000.0, [1.0; 2], 0.5, PI / 4.0, RATE);
    let sample_peak = x.iter().flatten().fold(0.0f32, |m, s| m.max(s.abs()));
    assert!((f64::from(sample_peak) - std::f64::consts::FRAC_1_SQRT_2).abs() < 1e-3);
    let tp = true_peak_dbtp(&x);
    assert!(tp.abs() < 0.2, "{tp}");
}

#[test]
fn true_peak_scales_with_level() {
    let x = sine(1000.0, [db(-6.0); 2], 1.0, 0.0, RATE);
    let tp = true_peak_dbtp(&x);
    assert!((tp - -6.0).abs() < 0.1, "{tp}");
}

#[test]
fn clipped_samples_counts_both_channels() {
    let x = [[1.0, 0.5], [-1.0, 1.2], [0.99, -0.99], [0.0, -1.0]];
    let a = analyze(&x, RATE, &[]);
    assert_eq!(a.clipped_samples, 4);
    assert_eq!(analyze(&[], RATE, &[]).clipped_samples, 0);
}

#[test]
fn track_peak_and_rms() {
    let x = sine(1000.0, [1.0; 2], 1.0, 0.0, RATE);
    let a = analyze(&[], RATE, &[(7, x), (1, vec![[0.0; 2]; 10])]);
    let t = &a.tracks[0];
    assert_eq!(t.track, 7);
    assert!(t.peak_dbfs.abs() < 0.01, "{}", t.peak_dbfs);
    assert!((t.rms_dbfs - -3.01).abs() < 0.01, "{}", t.rms_dbfs);
    let quiet = &a.tracks[1];
    assert_eq!(quiet.peak_dbfs, FLOOR_DB);
    assert_eq!(quiet.rms_dbfs, FLOOR_DB);
}

#[test]
fn band_balance_follows_the_tone() {
    for (freq, band) in [(80.0, 0), (1000.0, 1), (10_000.0, 2)] {
        let b = band_balance(&sine(freq, [0.5; 2], 1.0, 0.0, RATE), RATE);
        assert!(b[band] > 0.95, "{freq} Hz: {b:?}");
        assert!((b.iter().sum::<f64>() - 1.0).abs() < 1e-9);
    }
}

#[test]
fn band_balance_of_a_mix_splits_by_energy() {
    // Equal-amplitude tones in each band: roughly a third each.
    let a = sine(80.0, [0.3; 2], 1.0, 0.0, RATE);
    let b = sine(1000.0, [0.3; 2], 1.0, 0.0, RATE);
    let c = sine(10_000.0, [0.3; 2], 1.0, 0.0, RATE);
    let mix: Vec<[f32; 2]> = (0..a.len())
        .map(|i| [a[i][0] + b[i][0] + c[i][0], a[i][1] + b[i][1] + c[i][1]])
        .collect();
    let bal = band_balance(&mix, RATE);
    for v in bal {
        assert!((v - 1.0 / 3.0).abs() < 0.05, "{bal:?}");
    }
}

#[test]
fn analyze_fills_every_field_and_serialises() {
    let mix = sine(1000.0, [1.0; 2], 2.0, 0.0, RATE);
    let kick = sine(60.0, [0.5; 2], 1.0, 0.0, RATE);
    let a = analyze(&mix, RATE, &[(3, kick)]);
    assert!(a.integrated_lufs.abs() < 1.0, "{}", a.integrated_lufs);
    assert!(a.true_peak_dbtp.abs() < 0.1);
    assert!(a.clipped_samples > 0);
    let json = serde_json::to_string(&a).expect("finite numbers serialise");
    // serde_json's default float parser may differ in the last digit.
    let back: Analysis = serde_json::from_str(&json).expect("round trip");
    assert_eq!(back.clipped_samples, a.clipped_samples);
    assert_eq!(back.tracks.len(), 1);
    assert!((back.integrated_lufs - a.integrated_lufs).abs() < 1e-9);
}

#[test]
fn shelf_matches_the_published_48k_coefficients() {
    // BS.1770-4 table 1 (pre-filter at 48 kHz).
    let (b, a) = shelf_coefficients(48_000.0);
    let want_b = [1.53512485958697, -2.69169618940638, 1.19839281085285];
    let want_a = [-1.69065929318241, 0.73248077421585];
    for (g, w) in b.iter().zip(want_b).chain(a.iter().zip(want_a)) {
        assert!((g - w).abs() < 1e-9, "{g} vs {w}");
    }
}

#[test]
fn silent_analysis_still_serialises() {
    let a = analyze(&vec![[0.0; 2]; 48_000], RATE, &[(1, vec![[0.0; 2]; 10])]);
    let json = serde_json::to_string(&a).unwrap();
    let back: Analysis = serde_json::from_str(&json).unwrap();
    assert_eq!(back, a);
}
