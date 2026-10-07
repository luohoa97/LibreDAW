// SPDX-License-Identifier: GPL-3.0-or-later
//! Note extraction from model activations (port of `output_to_notes_polyphonic`
//! in Basic Pitch's `note_creation.py`, with the melodia trick, without
//! pitch bends).

use crate::model::{Activations, FFT_HOP, N_PITCHES, SAMPLE_RATE, frame_time};
use crate::{NoteEvent, Options};

const MIDI_OFFSET: u8 = 21;
const ENERGY_TOL: usize = 11;

pub fn extract(act: &Activations, opts: &Options) -> Vec<NoteEvent> {
    let n = act.n_frames;
    if n < 3 {
        return Vec::new();
    }
    let frames = &act.frames;
    let min_len =
        (opts.min_note_ms / 1000.0 * (SAMPLE_RATE as f32 / FFT_HOP as f32)).round() as usize;
    let onsets = inferred_onsets(&act.onsets, frames, n);
    let at = |t: usize, f: usize| t * N_PITCHES + f;

    // Onset peaks over time, strict local maxima, at or above the threshold.
    let mut starts: Vec<(usize, usize)> = Vec::new(); // (time, pitch)
    for t in 1..n - 1 {
        for f in 0..N_PITCHES {
            let v = onsets[at(t, f)];
            if v > onsets[at(t - 1, f)] && v > onsets[at(t + 1, f)] && v >= opts.onset_threshold {
                starts.push((t, f));
            }
        }
    }
    // Upstream walks backwards in time.
    starts.reverse();

    let mut remaining = frames.clone();
    let mut events: Vec<(usize, usize, usize, f32)> = Vec::new(); // start, end, pitch idx, amplitude
    let ft = opts.frame_threshold;

    let mean = |s: usize, e: usize, f: usize| {
        let sum: f32 = (s..e).map(|t| frames[at(t, f)]).sum();
        sum / (e - s) as f32
    };
    let clear_around = |rem: &mut Vec<f32>, t: usize, f: usize| {
        rem[at(t, f)] = 0.0;
        if f + 1 < N_PITCHES {
            rem[at(t, f + 1)] = 0.0;
        }
        if f > 0 {
            rem[at(t, f - 1)] = 0.0;
        }
    };

    for (start, f) in starts {
        if start >= n - 1 {
            continue;
        }
        let mut i = start + 1;
        let mut k = 0;
        while i < n - 1 && k < ENERGY_TOL {
            if remaining[at(i, f)] < ft {
                k += 1;
            } else {
                k = 0;
            }
            i += 1;
        }
        i -= k;
        if i - start <= min_len {
            continue;
        }
        for t in start..i {
            clear_around(&mut remaining, t, f);
        }
        events.push((start, i, f, mean(start, i, f)));
    }

    // Melodia trick: grow notes from the strongest leftover frame activity.
    loop {
        let (mut best, mut at_best) = (f32::MIN, 0usize);
        for (idx, &v) in remaining.iter().enumerate() {
            if v > best {
                best = v;
                at_best = idx;
            }
        }
        if best <= ft {
            break;
        }
        let (mid, f) = (at_best / N_PITCHES, at_best % N_PITCHES);
        remaining[at_best] = 0.0;

        let mut i = mid + 1;
        let mut k = 0;
        while i < n - 1 && k < ENERGY_TOL {
            if remaining[at(i, f)] < ft {
                k += 1;
            } else {
                k = 0;
            }
            clear_around(&mut remaining, i, f);
            i += 1;
        }
        let end = i - 1 - k;

        let mut i = mid as isize - 1;
        let mut k = 0;
        while i > 0 && k < ENERGY_TOL {
            if remaining[at(i as usize, f)] < ft {
                k += 1;
            } else {
                k = 0;
            }
            clear_around(&mut remaining, i as usize, f);
            i -= 1;
        }
        let begin = (i + 1) as usize + k;

        if end <= begin || end - begin <= min_len {
            continue;
        }
        events.push((begin, end, f, mean(begin, end, f)));
    }

    let mut out: Vec<NoteEvent> = events
        .into_iter()
        .map(|(s, e, f, amp)| NoteEvent {
            start_s: frame_time(s),
            end_s: frame_time(e),
            midi_key: f as u8 + MIDI_OFFSET,
            velocity: (amp * 127.0).round().clamp(1.0, 127.0) as u8,
            confidence: amp,
        })
        .collect();
    out.sort_by(|a, b| {
        a.start_s
            .total_cmp(&b.start_s)
            .then(a.midi_key.cmp(&b.midi_key))
    });
    out
}

/// Adds onsets inferred from rises in the frame activations (`get_infered_onsets`, n_diff = 2).
fn inferred_onsets(onsets: &[f32], frames: &[f32], n: usize) -> Vec<f32> {
    const N_DIFF: usize = 2;
    let mut diff = vec![0.0f32; n * N_PITCHES];
    let mut max_diff = 0.0f32;
    for t in N_DIFF..n {
        for f in 0..N_PITCHES {
            let cur = frames[t * N_PITCHES + f];
            let d1 = cur - frames[(t - 1) * N_PITCHES + f];
            let d2 = cur - frames[(t - 2) * N_PITCHES + f];
            let d = d1.min(d2).max(0.0);
            diff[t * N_PITCHES + f] = d;
            max_diff = max_diff.max(d);
        }
    }
    let max_onset = onsets.iter().copied().fold(0.0f32, f32::max);
    let scale = if max_diff > 0.0 {
        max_onset / max_diff
    } else {
        0.0
    };
    onsets
        .iter()
        .zip(&diff)
        .map(|(&o, &d)| o.max(d * scale))
        .collect()
}
