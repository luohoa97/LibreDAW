// SPDX-License-Identifier: GPL-3.0-or-later
//! Helpers on note lists: monophonic reduction, grid quantizing, key detection.

use crate::NoteEvent;

/// Notes shorter than this are dropped when trimming overlaps.
const MIN_MONO_S: f64 = 0.06;
/// Same-pitch notes closer than this are joined (vibrato splits a hum).
const JOIN_GAP_S: f64 = 0.03;

/// Keeps one note at a time. The strongest note wins an overlap; the weaker
/// one is trimmed to its longest free piece, or dropped when too short.
pub(crate) fn make_monophonic(notes: Vec<NoteEvent>) -> Vec<NoteEvent> {
    let mut by_strength = notes;
    by_strength.sort_by(|a, b| b.confidence.total_cmp(&a.confidence));
    let mut kept: Vec<NoteEvent> = Vec::new();
    for mut n in by_strength {
        // Free pieces of [start, end) after removing the kept notes.
        let mut pieces = vec![(n.start_s, n.end_s)];
        for k in &kept {
            pieces = pieces
                .into_iter()
                .flat_map(|(s, e)| {
                    if k.end_s <= s || k.start_s >= e {
                        vec![(s, e)]
                    } else {
                        let mut v = Vec::new();
                        if k.start_s > s {
                            v.push((s, k.start_s));
                        }
                        if k.end_s < e {
                            v.push((k.end_s, e));
                        }
                        v
                    }
                })
                .collect();
        }
        let best = pieces
            .into_iter()
            .max_by(|a, b| (a.1 - a.0).total_cmp(&(b.1 - b.0)));
        if let Some((s, e)) = best.filter(|(s, e)| e - s >= MIN_MONO_S) {
            n.start_s = s;
            n.end_s = e;
            kept.push(n);
        }
    }
    kept.sort_by(|a, b| a.start_s.total_cmp(&b.start_s));
    let mut out: Vec<NoteEvent> = Vec::new();
    for n in kept {
        match out.last_mut() {
            Some(p) if p.midi_key == n.midi_key && n.start_s - p.end_s <= JOIN_GAP_S => {
                p.end_s = p.end_s.max(n.end_s);
                p.confidence = p.confidence.max(n.confidence);
                p.velocity = p.velocity.max(n.velocity);
            }
            _ => out.push(n),
        }
    }
    out
}

/// Snaps note times to a grid of `grid_beats` (1.0 = quarter note, 0.25 = sixteenth)
/// at `tempo_bpm`, with beat 0 at `origin_s`. A note is at least one grid step
/// long and never overlaps the next note of the same pitch. With `keep_timing`
/// the notes are returned as they are.
pub fn quantize(
    notes: &[NoteEvent],
    tempo_bpm: f64,
    grid_beats: f64,
    origin_s: f64,
    keep_timing: bool,
) -> Vec<NoteEvent> {
    if keep_timing || tempo_bpm <= 0.0 || grid_beats <= 0.0 {
        return notes.to_vec();
    }
    let step = 60.0 / tempo_bpm * grid_beats;
    let snap = |t: f64| origin_s + ((t - origin_s) / step).round() * step;
    let mut out: Vec<NoteEvent> = notes
        .iter()
        .map(|n| {
            let s = snap(n.start_s);
            let e = snap(n.end_s).max(s + step);
            NoteEvent {
                start_s: s,
                end_s: e,
                ..n.clone()
            }
        })
        .collect();
    out.sort_by(|a, b| {
        a.start_s
            .total_cmp(&b.start_s)
            .then(a.midi_key.cmp(&b.midi_key))
    });
    for i in 0..out.len() {
        let (key, end) = (out[i].midi_key, out[i].end_s);
        if let Some(next) = out[i + 1..]
            .iter()
            .find(|m| m.midi_key == key && m.start_s < end)
        {
            let s = next.start_s;
            if s > out[i].start_s {
                out[i].end_s = s;
            }
        }
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scale {
    Major,
    Minor,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Key {
    /// Pitch class of the tonic, 0 = C.
    pub tonic: u8,
    pub scale: Scale,
    /// Correlation with the winning profile, -1 to 1.
    pub correlation: f64,
}

impl Key {
    /// For example "C major", "F# minor".
    pub fn name(&self) -> String {
        const NAMES: [&str; 12] = [
            "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
        ];
        let s = match self.scale {
            Scale::Major => "major",
            Scale::Minor => "minor",
        };
        format!("{} {}", NAMES[self.tonic as usize % 12], s)
    }
}

const KK_MAJOR: [f64; 12] = [
    6.35, 2.23, 3.48, 2.33, 4.38, 4.09, 2.52, 5.19, 2.39, 3.66, 2.29, 2.88,
];
const KK_MINOR: [f64; 12] = [
    6.33, 2.68, 3.52, 5.38, 2.60, 3.53, 2.54, 4.75, 3.98, 2.69, 3.34, 3.17,
];

/// Krumhansl-Kessler key finding over the note durations. `None` when there
/// are no notes.
pub fn detect_key(notes: &[NoteEvent]) -> Option<Key> {
    let mut hist = [0.0f64; 12];
    for n in notes {
        hist[n.midi_key as usize % 12] += (n.end_s - n.start_s).max(0.0);
    }
    if hist.iter().sum::<f64>() <= 0.0 {
        return None;
    }
    let mut best: Option<Key> = None;
    for tonic in 0..12u8 {
        for (scale, profile) in [(Scale::Major, &KK_MAJOR), (Scale::Minor, &KK_MINOR)] {
            let rotated: Vec<f64> = (0..12)
                .map(|pc| profile[(pc + 12 - tonic as usize) % 12])
                .collect();
            let c = correlation(&hist, &rotated);
            if best.as_ref().is_none_or(|b| c > b.correlation) {
                best = Some(Key {
                    tonic,
                    scale,
                    correlation: c,
                });
            }
        }
    }
    best
}

fn correlation(a: &[f64], b: &[f64]) -> f64 {
    let n = a.len() as f64;
    let (ma, mb) = (a.iter().sum::<f64>() / n, b.iter().sum::<f64>() / n);
    let (mut num, mut da, mut db) = (0.0, 0.0, 0.0);
    for (x, y) in a.iter().zip(b) {
        num += (x - ma) * (y - mb);
        da += (x - ma).powi(2);
        db += (y - mb).powi(2);
    }
    if da <= 0.0 || db <= 0.0 {
        0.0
    } else {
        num / (da * db).sqrt()
    }
}
