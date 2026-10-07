// SPDX-License-Identifier: GPL-3.0-or-later
//! Root note of a one-note WAV sample by autocorrelation. Used only for
//! multisamples whose file names carry no note (for example `Jazz Guitar (3)`).

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

use crate::header::wav_layout;

/// Seconds skipped after the start so the attack does not dominate.
const SKIP_S: f64 = 0.12;
/// Seconds analysed.
const WINDOW_S: f64 = 0.35;

/// MIDI note of the dominant pitch, or `None` for noise or silence.
pub fn estimate_root(path: &Path) -> io::Result<Option<u8>> {
    let mut f = File::open(path)?;
    let len = f.metadata()?.len();
    let l = wav_layout(&mut f, len)?;
    let bytes_per = usize::from(l.bits / 8);
    let ch = usize::from(l.channels.max(1));
    if bytes_per == 0 || usize::from(l.block_align) != bytes_per * ch || !matches!(l.tag, 1 | 3) {
        return Ok(None);
    }
    let rate = f64::from(l.sample_rate);
    let total = l.data_len / u64::from(l.block_align);
    let skip = ((rate * SKIP_S) as u64).min(total / 2);
    let want = ((rate * WINDOW_S) as u64).min(total - skip);
    if want < 2048 {
        return Ok(None);
    }
    f.seek(SeekFrom::Start(
        l.data_pos + skip * u64::from(l.block_align),
    ))?;
    let mut raw = vec![0u8; want as usize * usize::from(l.block_align)];
    f.read_exact(&mut raw)?;
    let mono: Vec<f32> = raw
        .chunks_exact(usize::from(l.block_align))
        .map(|fr| {
            let sum: f32 = fr
                .chunks_exact(bytes_per)
                .map(|s| sample(s, l.tag, l.bits))
                .sum();
            sum / ch as f32
        })
        .collect();
    Ok(pitch_midi(&mono, rate))
}

fn sample(s: &[u8], tag: u16, bits: u16) -> f32 {
    match (tag, bits) {
        (1, 8) => (f32::from(s[0]) - 128.0) / 128.0,
        (1, 16) => f32::from(i16::from_le_bytes([s[0], s[1]])) / 32768.0,
        (1, 24) => (i32::from_le_bytes([0, s[0], s[1], s[2]]) >> 8) as f32 / 8_388_608.0,
        (1, 32) => i32::from_le_bytes([s[0], s[1], s[2], s[3]]) as f32 / 2_147_483_648.0,
        (3, 32) => f32::from_le_bytes([s[0], s[1], s[2], s[3]]),
        _ => 0.0,
    }
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
    let mut lag = (min_lag..=max_lag)
        .find(|&l| r[l] >= 0.9 * best && r[l] >= r[l - 1] && r[l] >= r[l + 1])?;
    let (a, b, c) = (r[lag - 1], r[lag], r[lag + 1]);
    let denom = a - 2.0 * b + c;
    let off = if denom.abs() > 1e-9 {
        0.5 * (a - c) / denom
    } else {
        0.0
    };
    let period = lag as f64 + f64::from(off).clamp(-1.0, 1.0);
    lag = lag.max(1);
    let hz = rate / period.max(lag as f64 * 0.5);
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
            .map(|i| (2.0 * std::f64::consts::PI * 440.0 * f64::from(i) / rate).sin() as f32)
            .collect();
        assert_eq!(pitch_midi(&x, rate), Some(69));
    }

    #[test]
    fn silence_has_no_pitch() {
        assert_eq!(pitch_midi(&vec![0.0; 15000], 44100.0), None);
    }
}
