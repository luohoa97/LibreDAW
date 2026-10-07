// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;
use std::f32::consts::TAU;
use std::time::Instant;

const SR: u32 = 22050;

/// Sine notes with a short gap after each; returns the audio and (start, end, key).
fn melody(
    keys: &[u8],
    note_s: f32,
    gap_s: f32,
    sr: u32,
    vibrato: bool,
) -> (Vec<f32>, Vec<(f64, f64, u8)>) {
    let mut audio = vec![0.0f32; (0.3 * sr as f32) as usize];
    let mut truth = Vec::new();
    for &k in keys {
        let start = audio.len() as f64 / sr as f64;
        let f0 = 440.0 * 2f32.powf((k as f32 - 69.0) / 12.0);
        let n = (note_s * sr as f32) as usize;
        let mut phase = 0.0f32;
        for i in 0..n {
            let t = i as f32 / sr as f32;
            let f = if vibrato {
                f0 * (1.0 + 0.01 * (TAU * 5.5 * t).sin())
            } else {
                f0
            };
            phase += TAU * f / sr as f32;
            let env = (t / 0.03).min(1.0) * ((note_s - t) / 0.03).clamp(0.0, 1.0);
            audio.push(0.5 * env * (phase.sin() + 0.3 * (2.0 * phase).sin()));
        }
        truth.push((start, start + note_s as f64, k));
        audio.extend(std::iter::repeat_n(0.0, (gap_s * sr as f32) as usize));
    }
    audio.extend(std::iter::repeat_n(0.0, (0.3 * sr as f32) as usize));
    (audio, truth)
}

/// Fraction of true notes with a detected note of the same key and onset within 50 ms.
fn recall(truth: &[(f64, f64, u8)], got: &[NoteEvent]) -> f64 {
    let hit = truth
        .iter()
        .filter(|(s, _, k)| {
            got.iter()
                .any(|n| n.midi_key == *k && (n.start_s - s).abs() <= 0.05)
        })
        .count();
    hit as f64 / truth.len() as f64
}

#[test]
fn golden_sine_melody() {
    let (audio, truth) = melody(&[60, 64, 67, 72], 0.5, 0.1, SR, false);
    let got = transcribe(&audio, SR, &Options::default()).unwrap();
    println!("{got:#?}");
    assert_eq!(recall(&truth, &got), 1.0, "{got:?}");
    for (s, e, k) in &truth {
        let n = got
            .iter()
            .find(|n| n.midi_key == *k && (n.start_s - s).abs() <= 0.05)
            .unwrap();
        assert!((n.end_s - e).abs() < 0.15, "end of {k}: {} vs {e}", n.end_s);
        assert!((1..=127).contains(&n.velocity));
    }
}

#[test]
fn resampled_input_matches() {
    let (audio, truth) = melody(&[60, 64, 67, 72], 0.5, 0.1, 44100, false);
    let got = transcribe(&audio, 44100, &Options::default()).unwrap();
    assert_eq!(recall(&truth, &got), 1.0, "{got:?}");
}

#[test]
fn hum_with_vibrato_and_noise_monophonic() {
    let keys = [57, 60, 62, 64, 62, 60, 57, 55];
    let (mut audio, truth) = melody(&keys, 0.45, 0.05, SR, true);
    let mut seed = 12345u32;
    for x in audio.iter_mut() {
        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        *x += 0.02 * ((seed >> 8) as f32 / (1u32 << 24) as f32 - 0.5);
    }
    let opts = Options {
        monophonic: true,
        ..Options::default()
    };
    let got = transcribe(&audio, SR, &opts).unwrap();
    println!("hum recall {:.2}: {got:#?}", recall(&truth, &got));
    for w in got.windows(2) {
        assert!(w[0].end_s <= w[1].start_s + 1e-9, "overlap {w:?}");
    }
    assert!(recall(&truth, &got) >= 0.85, "{got:?}");
}

#[test]
fn silence_gives_no_notes() {
    let got = transcribe(&vec![0.0; 44100], SR, &Options::default()).unwrap();
    assert!(got.is_empty());
    assert!(transcribe(&[], SR, &Options::default()).unwrap().is_empty());
    assert!(matches!(
        transcribe(&[0.0], 0, &Options::default()),
        Err(Error::BadSampleRate)
    ));
}

fn note(start: f64, end: f64, key: u8) -> NoteEvent {
    NoteEvent {
        start_s: start,
        end_s: end,
        midi_key: key,
        velocity: 90,
        confidence: 0.8,
    }
}

#[test]
fn key_of_c_major_scale() {
    let notes: Vec<_> = [60u8, 62, 64, 65, 67, 69, 71, 72]
        .iter()
        .enumerate()
        .map(|(i, &k)| note(i as f64 * 0.5, i as f64 * 0.5 + 0.5, k))
        .collect();
    let key = detect_key(&notes).unwrap();
    assert_eq!(key.name(), "C major");
    let a_minor: Vec<_> = [57u8, 59, 60, 62, 64, 65, 68, 69]
        .iter()
        .enumerate()
        .map(|(i, &k)| {
            note(
                i as f64 * 0.5,
                i as f64 * 0.5 + if k == 57 || k == 69 { 1.0 } else { 0.4 },
                k,
            )
        })
        .collect();
    assert_eq!(detect_key(&a_minor).unwrap().name(), "A minor");
    assert!(detect_key(&[]).is_none());
}

#[test]
fn quantize_snaps_and_keeps_timing() {
    let notes = vec![note(0.03, 0.46, 60), note(0.52, 0.98, 62)];
    // 120 bpm, eighth-note grid: 0.25 s.
    let q = quantize(&notes, 120.0, 0.5, 0.0, false);
    assert_eq!((q[0].start_s, q[0].end_s), (0.0, 0.5));
    assert_eq!((q[1].start_s, q[1].end_s), (0.5, 1.0));
    assert_eq!(quantize(&notes, 120.0, 0.5, 0.0, true), notes);
}

#[test]
fn monophonic_keeps_the_strongest() {
    let mut weak = note(0.0, 1.0, 60);
    weak.confidence = 0.4;
    let strong = note(0.3, 0.8, 64);
    let m = post::make_monophonic(vec![weak, strong]);
    assert!(m.iter().any(|n| n.midi_key == 64 && n.start_s == 0.3));
    for w in m.windows(2) {
        assert!(w[0].end_s <= w[1].start_s);
    }
}

#[test]
#[ignore = "timing; run with --release --ignored"]
fn thirty_seconds_under_three_seconds() {
    let keys: Vec<u8> = (0..60).map(|i| 55 + (i * 5 % 12) as u8).collect();
    let (audio, _) = melody(&keys, 0.45, 0.05, SR, true);
    let audio = &audio[..(30 * SR as usize).min(audio.len())];
    transcribe(&audio[..SR as usize], SR, &Options::default()).unwrap(); // load the model
    let t = Instant::now();
    let got = transcribe(audio, SR, &Options::default()).unwrap();
    let dt = t.elapsed();
    println!(
        "30 s of audio: {:.2} s, {} notes",
        dt.as_secs_f64(),
        got.len()
    );
    assert!(dt.as_secs_f64() < 3.0, "took {dt:?}");
}

/// Synthetic accuracy over several vibrato-and-noise melodies. Prints the
/// numbers quoted in the report; this is not a real-humming measurement.
#[test]
#[ignore = "measurement; run with --release --ignored --nocapture"]
fn synthetic_accuracy() {
    let (mut hit, mut total) = (0usize, 0usize);
    for m in 0..6u32 {
        let mut s = 7 + m * 101;
        let keys: Vec<u8> = (0..10)
            .map(|_| {
                s = s.wrapping_mul(1664525).wrapping_add(1013904223);
                50 + ((s >> 16) % 24) as u8
            })
            .collect();
        let (mut audio, truth) = melody(&keys, 0.4, 0.06, SR, true);
        for x in audio.iter_mut() {
            s = s.wrapping_mul(1664525).wrapping_add(1013904223);
            *x += 0.02 * ((s >> 8) as f32 / (1u32 << 24) as f32 - 0.5);
        }
        let opts = Options {
            monophonic: true,
            ..Options::default()
        };
        let got = transcribe(&audio, SR, &opts).unwrap();
        let h = (recall(&truth, &got) * truth.len() as f64).round() as usize;
        println!(
            "melody {m}: {h}/{} notes right, {} detected",
            truth.len(),
            got.len()
        );
        hit += h;
        total += truth.len();
    }
    println!(
        "synthetic total: {hit}/{total} = {:.0}%",
        100.0 * hit as f64 / total as f64
    );
}
