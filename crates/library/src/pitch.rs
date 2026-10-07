// SPDX-License-Identifier: GPL-3.0-or-later
//! Root note of a one-note sample by autocorrelation on the decoded audio.
//! Used only for multisamples whose file names carry no note (for example
//! `Jazz Guitar (3)`).

use std::path::Path;

/// Seconds skipped after the start so the attack does not dominate.
const SKIP_S: f64 = 0.12;
/// Seconds analysed.
const WINDOW_S: f64 = 0.35;

/// MIDI note of the dominant pitch, or `None` for noise, silence or a file
/// that does not decode.
pub fn estimate_root(path: &Path) -> Option<u8> {
    let a = audiofile::decode_file(path).ok()?;
    let ch = usize::from(a.channels.max(1));
    let mono: Vec<f32> = a
        .data
        .chunks_exact(ch)
        .map(|f| f.iter().sum::<f32>() / ch as f32)
        .collect();
    let rate = f64::from(a.rate);
    let total = mono.len();
    let skip = ((rate * SKIP_S) as usize).min(total / 2);
    let want = ((rate * WINDOW_S) as usize).min(total - skip);
    if want < 2048 {
        return None;
    }
    pitch_midi(&mono[skip..skip + want], rate)
}

fn pitch_midi(x: &[f32], rate: f64) -> Option<u8> {
    let n = x.len();
    let energy: f32 = x.iter().map(|v| v * v).sum();
    if energy < 1e-6 {
        return None;
    }
    let min_lag = (rate / 1500.0) as usize;
    let max_lag = ((rate / 27.0) as usize).min(n / 2);
    if min_lag < 2 || max_lag <= min_lag + 2 {
        return None;
    }
    let mut r = vec![0f32; max_lag + 2];
    for (lag, slot) in r.iter_mut().enumerate().take(max_lag + 2).skip(min_lag - 1) {
        let m = n - lag;
        let mut acc = 0f32;
        for i in 0..m {
            acc += x[i] * x[i + lag];
        }
        // Normalised by the overlapped energy so long lags are not penalised.
        let e: f32 = x[..m].iter().map(|v| v * v).sum::<f32>().max(1e-9);
        *slot = acc / e;
    }
    let best = r[min_lag..=max_lag]
        .iter()
        .copied()
        .fold(f32::MIN, f32::max);
    if best < 0.3 {
        return None;
    }
    // The shortest lag whose peak is near the best one (avoids sub-octave picks).
    let lag = (min_lag..=max_lag)
        .find(|&l| r[l] >= 0.9 * best && r[l] >= r[l - 1] && r[l] >= r[l + 1])?;
    let (a, b, c) = (r[lag - 1], r[lag], r[lag + 1]);
    let denom = a - 2.0 * b + c;
    let off = if denom.abs() > 1e-9 {
        0.5 * (a - c) / denom
    } else {
        0.0
    };
    let period = lag as f64 + f64::from(off).clamp(-1.0, 1.0);
    let hz = rate / period;
    let midi = 69.0 + 12.0 * (hz / 440.0).log2();
    (0.0..=127.0).contains(&midi).then(|| midi.round() as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sine_a4_is_69() {
        let rate = 44100.0;
        let x: Vec<f32> = (0..15000)
            .map(|i| (std::f64::consts::TAU * 440.0 * f64::from(i) / rate).sin() as f32)
            .collect();
        assert_eq!(pitch_midi(&x, rate), Some(69));
    }

    #[test]
    fn low_and_high_notes() {
        let rate = 44100.0;
        for (hz, midi) in [(55.0, 33u8), (261.63, 60), (1046.5, 84)] {
            let x: Vec<f32> = (0..16000)
                .map(|i| (std::f64::consts::TAU * hz * f64::from(i) / rate).sin() as f32)
                .collect();
            assert_eq!(pitch_midi(&x, rate), Some(midi), "{hz} Hz");
        }
    }

    #[test]
    fn silence_has_no_pitch() {
        assert_eq!(pitch_midi(&vec![0.0; 15000], 44100.0), None);
    }
}
