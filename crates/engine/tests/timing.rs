// SPDX-License-Identifier: GPL-3.0-or-later
//! Deterministic, offline timing tests (SPEC 14). Every case runs the same
//! callback path the live stream uses, with odd callback sizes, and compares
//! each click onset with an exact closed-form grid computed in integers.

use engine::{CallbackState, Controls, Metronome, Recorder, rt};
use std::sync::Arc;

const SIZES: [usize; 5] = [1, 37, 256, 300, 1024];

/// A tempo as an exact fraction, so the ideal grid needs no floating point.
#[derive(Clone, Copy)]
struct Bpm {
    num: i128,
    den: i128,
}

impl Bpm {
    const fn new(num: i128, den: i128) -> Self {
        Bpm { num, den }
    }
    fn f64(self) -> f64 {
        self.num as f64 / self.den as f64
    }
}

/// Exact rational with a positive denominator.
#[derive(Clone, Copy)]
struct Frac {
    n: i128,
    d: i128,
}

fn gcd(a: i128, b: i128) -> i128 {
    if b == 0 { a.abs() } else { gcd(b, a % b) }
}

impl Frac {
    fn new(n: i128, d: i128) -> Frac {
        let g = gcd(n, d).max(1);
        let s = if d < 0 { -1 } else { 1 };
        Frac {
            n: s * n / g,
            d: s * d / g,
        }
    }
    fn add(self, o: Frac) -> Frac {
        Frac::new(self.n * o.d + o.n * self.d, self.d * o.d)
    }
    fn mul(self, o: Frac) -> Frac {
        Frac::new(self.n * o.n, self.d * o.d)
    }
    /// Round half up. The grids below have no exact ties (checked by test).
    fn round(self) -> i128 {
        (2 * self.n + self.d).div_euclid(2 * self.d)
    }
}

/// Ideal onset sample of beat `n` at a constant tempo from sample 0.
fn ideal_onset(n: u64, rate: u32, bpm: Bpm) -> u64 {
    // n * 960 ticks * (rate * 60 / (bpm * 960)) samples per tick
    Frac::new(n as i128 * rate as i128 * 60 * bpm.den, bpm.num).round() as u64
}

struct Run {
    onsets: Vec<u64>,
    accents: Vec<bool>,
}

/// Renders `frames` frames through `CallbackState::process` with callback
/// sizes cycling through `SIZES`. `steps` are `(frame, new_bpm)` tempo
/// changes, applied between callbacks once `frame` has been reached.
fn render(rate: u32, bpm: Bpm, frames: u64, loop_ticks: Option<u64>, steps: &[(u64, Bpm)]) -> Run {
    let controls = Arc::new(Controls::new(bpm.f64(), 1.0));
    let recorder = Recorder::new(64, frames as usize / (rate as usize / 20) + 16, 8);
    let metronome = Metronome::new(rate, 2, controls.clone(), recorder.clone(), loop_ticks);
    let mut cb = CallbackState::new(metronome, recorder.clone());
    let mut buf = vec![0.0f32; SIZES.iter().max().unwrap() * 2];
    let (mut done, mut i, mut next_step) = (0u64, 0usize, 0usize);
    while done < frames {
        if next_step < steps.len() && done >= steps[next_step].0 {
            controls.set_tempo(steps[next_step].1.f64());
            next_step += 1;
        }
        let n = SIZES[i % SIZES.len()].min((frames - done) as usize);
        i += 1;
        cb.process(&mut buf[..n * 2], 0);
        done += n as u64;
    }
    Run {
        onsets: recorder.onsets(),
        accents: recorder.onset_accents(),
    }
}

/// Number of beats of the ideal constant-tempo grid that start before `frames`.
fn ideal_count(frames: u64, rate: u32, bpm: Bpm) -> u64 {
    let mut n = 0;
    while ideal_onset(n, rate, bpm) < frames {
        n += 1;
    }
    n
}

fn one_hour(rate: u32, bpm: Bpm) {
    let frames = rate as u64 * 3600;
    let run = render(rate, bpm, frames, None, &[]);
    let expect = ideal_count(frames, rate, bpm);
    assert_eq!(run.onsets.len() as u64, expect, "number of clicks");
    for (n, &got) in run.onsets.iter().enumerate() {
        assert_eq!(got, ideal_onset(n as u64, rate, bpm), "beat {n}");
    }
    for (n, &a) in run.accents.iter().enumerate() {
        assert_eq!(a, n % 4 == 0, "accent on beat {n}");
    }
    eprintln!(
        "checked {} onsets at {rate} Hz, {:.2} BPM",
        run.onsets.len(),
        bpm.f64()
    );
}

#[test]
fn one_hour_44100_60bpm() {
    one_hour(44100, Bpm::new(60, 1));
}
#[test]
fn one_hour_44100_120bpm() {
    one_hour(44100, Bpm::new(120, 1));
}
#[test]
fn one_hour_44100_133_33bpm() {
    one_hour(44100, Bpm::new(13333, 100));
}
#[test]
fn one_hour_44100_999bpm() {
    one_hour(44100, Bpm::new(999, 1));
}
#[test]
fn one_hour_48000_60bpm() {
    one_hour(48000, Bpm::new(60, 1));
}
#[test]
fn one_hour_48000_120bpm() {
    one_hour(48000, Bpm::new(120, 1));
}
#[test]
fn one_hour_48000_133_33bpm() {
    one_hour(48000, Bpm::new(13333, 100));
}
#[test]
fn one_hour_48000_999bpm() {
    one_hour(48000, Bpm::new(999, 1));
}

#[test]
fn loop_wrap_ten_minutes_keeps_the_fraction() {
    let (rate, bpm) = (44100, Bpm::new(13333, 100));
    let frames = rate as u64 * 600;
    // one 4/4 bar: 4 * 960 ticks
    let run = render(rate, bpm, frames, Some(4 * 960), &[]);
    assert_eq!(run.onsets.len() as u64, ideal_count(frames, rate, bpm));
    for (n, &got) in run.onsets.iter().enumerate() {
        assert_eq!(got, ideal_onset(n as u64, rate, bpm), "beat {n}");
    }
    for (n, &a) in run.accents.iter().enumerate() {
        assert_eq!(a, n % 4 == 0, "accent on beat {n}");
    }
    eprintln!(
        "checked {} onsets across {} loop wraps",
        run.onsets.len(),
        run.onsets.len() / 4
    );
}

#[test]
fn loop_wrap_wraps_are_counted() {
    let controls = Arc::new(Controls::new(120.0, 1.0));
    let recorder = Recorder::new(8, 64, 8);
    let mut m = Metronome::new(48000, 2, controls, recorder, Some(4 * 960));
    let mut buf = vec![0.0f32; 2 * 48000 * 10];
    m.render(&mut buf);
    // 10 s at 120 BPM: beats 0..=19 emitted, 5 bars of 4 beats; the wrap
    // happens when the last beat of a bar is scheduled past the loop end.
    assert_eq!(m.loops_completed(), 5);
}

/// Piecewise closed form: after a change at frame `s` the exact beat
/// position `pb` is carried as a rational, and every beat whose onset is
/// still ahead of `s` is placed with the new tempo (never before `s`).
fn ideal_with_changes(rate: u32, start: Bpm, steps: &[(u64, Bpm)], frames: u64) -> Vec<u64> {
    let spm = Frac::new(rate as i128 * 60, 1); // samples per minute
    let mut segs: Vec<(u64, Frac, Bpm)> = vec![(0, Frac::new(0, 1), start)];
    for &(at, new) in steps {
        let &(s0, pb0, b0) = segs.last().unwrap();
        // beats advanced = (at - s0) * bpm / (rate * 60)
        let adv = Frac::new((at - s0) as i128 * b0.num, b0.den).mul(Frac::new(1, spm.n));
        segs.push((at, pb0.add(adv), new));
    }
    let mut out = vec![];
    let mut seg = 0;
    for n in 0u64.. {
        loop {
            let (s, pb, b) = segs[seg];
            let beats = Frac::new(n as i128, 1).add(Frac::new(-pb.n, pb.d));
            let samples = beats.mul(Frac::new(spm.n * b.den, b.num));
            let onset = (s as i128 + samples.round()).max(s as i128) as u64;
            let end = segs.get(seg + 1).map_or(u64::MAX, |x| x.0);
            if onset < end {
                if onset >= frames {
                    return out;
                }
                out.push(onset);
                break;
            }
            seg += 1;
        }
    }
    unreachable!()
}

#[test]
fn tempo_changes_follow_the_piecewise_grid() {
    let rate = 44100;
    let start = Bpm::new(120, 1);
    // frames are not callback-aligned on purpose: the change lands on the
    // first callback boundary at or after each frame
    let steps = [
        (100_000, Bpm::new(13333, 100)),
        (700_001, Bpm::new(90, 1)),
        (1_500_000, Bpm::new(999, 1)),
        (2_000_003, Bpm::new(60, 1)),
        (3_000_000, Bpm::new(20, 1)),
        (3_100_000, Bpm::new(120, 1)),
    ];
    let frames = rate as u64 * 300;
    let run = render(rate, start, frames, None, &steps);
    // the render applies each step at the callback boundary at or after it;
    // recompute those boundaries the same way the callbacks are cut
    let mut boundaries = vec![];
    let (mut done, mut i, mut k) = (0u64, 0usize, 0usize);
    while done < frames {
        if k < steps.len() && done >= steps[k].0 {
            boundaries.push((done, steps[k].1));
            k += 1;
        }
        done += SIZES[i % SIZES.len()].min((frames - done) as usize) as u64;
        i += 1;
    }
    assert_eq!(boundaries.len(), steps.len());
    let ideal = ideal_with_changes(rate, start, &boundaries, frames);
    assert_eq!(run.onsets.len(), ideal.len(), "number of clicks");
    for (n, (&got, &want)) in run.onsets.iter().zip(&ideal).enumerate() {
        assert_eq!(got, want, "beat {n}");
    }
    eprintln!(
        "checked {} onsets across {} tempo changes",
        ideal.len(),
        steps.len()
    );
}

#[test]
fn no_exact_ties_in_the_test_grids() {
    // The ideal grids round half up; the engine rounds half away from zero.
    // They agree unless a value is exactly x.5. Check the constant grids.
    for rate in [44100u32, 48000] {
        for bpm in [
            Bpm::new(60, 1),
            Bpm::new(120, 1),
            Bpm::new(13333, 100),
            Bpm::new(999, 1),
        ] {
            for n in 0..200_000u64 {
                let v = Frac::new(n as i128 * rate as i128 * 60 * bpm.den, bpm.num);
                assert!(v.d != 2, "tie at beat {n}");
            }
        }
    }
}

#[test]
fn click_is_audible_exactly_at_the_recorded_onset() {
    let (rate, bpm) = (48000u32, Bpm::new(13333, 100));
    let controls = Arc::new(Controls::new(bpm.f64(), 1.0));
    let recorder = Recorder::new(16, 64, 8);
    let m = Metronome::new(rate, 2, controls, recorder.clone(), None);
    let mut cb = CallbackState::new(m, recorder.clone());
    let total = rate as usize * 6;
    let mut out = vec![0.0f32; total * 2];
    let (mut done, mut i) = (0usize, 0usize);
    while done < total {
        let n = SIZES[i % SIZES.len()].min(total - done);
        i += 1;
        cb.process(&mut out[done * 2..(done + n) * 2], 0);
        done += n;
    }
    let onsets = recorder.onsets();
    assert!(onsets.len() >= 10);
    for &o in &onsets {
        let o = o as usize;
        assert!(out[o * 2] != 0.0, "first click sample at {o}");
        assert_eq!(out[o * 2], out[o * 2 + 1], "both channels carry the click");
        if o > 0 {
            assert_eq!(out[(o - 1) * 2], 0.0, "silence before {o}");
        }
    }
    // the first nonzero frame after silence is always a recorded onset
    let mut expect = onsets.iter().map(|&o| o as usize);
    let mut next = expect.next();
    let mut prev_zero = true;
    for f in 0..total {
        let nz = out[f * 2] != 0.0;
        if nz && prev_zero {
            assert_eq!(Some(f), next, "unexpected click start at {f}");
            next = expect.next();
        }
        prev_zero = !nz;
    }
}

#[test]
fn output_does_not_depend_on_callback_size() {
    let rate = 44100u32;
    let render_with = |sizes: &[usize]| {
        let controls = Arc::new(Controls::new(133.33, 0.8));
        let recorder = Recorder::new(16, 64, 8);
        let m = Metronome::new(rate, 2, controls, recorder.clone(), None);
        let mut cb = CallbackState::new(m, recorder);
        let total = rate as usize * 4;
        let mut out = vec![0.0f32; total * 2];
        let (mut done, mut i) = (0, 0);
        while done < total {
            let n = sizes[i % sizes.len()].min(total - done);
            i += 1;
            cb.process(&mut out[done * 2..(done + n) * 2], 0);
            done += n;
        }
        out
    };
    let a = render_with(&[256]);
    let b = render_with(&SIZES);
    let c = render_with(&[64]);
    assert!(a == b && a == c, "audio differs between callback sizes");
}

#[cfg(debug_assertions)]
#[test]
fn callback_path_makes_zero_allocations() {
    let rate = 48000u32;
    let controls = Arc::new(Controls::new(120.0, 0.5));
    let recorder = Recorder::new(1 << 16, 2048, 16);
    let m = Metronome::new(rate, 2, controls.clone(), recorder.clone(), Some(4 * 960));
    let mut cb = CallbackState::new(m, recorder.clone());
    let mut buf = vec![0.0f32; 1024 * 2];
    let total = rate as u64 * 600;
    let before = rt::rt_events();
    let guard = rt::RtGuard::enter_counting();
    let (mut done, mut i) = (0u64, 0usize);
    while done < total {
        let n = SIZES[i % SIZES.len()];
        i += 1;
        if i % 5000 == 0 {
            controls.set_tempo(100.0 + (i % 97) as f64); // tempo changes are allocation-free too
        }
        cb.process(&mut buf[..n * 2], 0);
        done += n as u64;
    }
    drop(guard);
    assert_eq!(
        rt::rt_events() - before,
        0,
        "allocations or frees in the callback"
    );
    assert!(recorder.onsets().len() > 1000);
}
