// SPDX-License-Identifier: GPL-3.0-or-later
//! The phonk effect set (SPEC 24.2): Drive sounds keep the loudness,
//! Duck to Kick dips the bass at every kick, Loudness reaches the target
//! level under the ceiling.

mod common;

use control::fxpresets;
use engine::fx::saturator::Saturator;
use protocol::beats::{BuiltinFxKind, SaturatorCurve};

const SR: f64 = 48000.0;

/// A bass-heavy test signal with some upper harmonics, about -9 dBFS RMS.
fn test_signal(n: usize) -> Vec<f32> {
    let tone = |f: f64, a: f64, i: usize| a * (std::f64::consts::TAU * f * i as f64 / SR).sin();
    (0..n)
        .map(|i| {
            (tone(55.0, 0.4, i)
                + tone(165.0, 0.2, i)
                + tone(1000.0, 0.15, i)
                + tone(3000.0, 0.1, i)) as f32
        })
        .collect()
}

fn db(x: f32) -> f32 {
    20.0 * x.log10()
}

fn saturated_rms_db(curve: SaturatorCurve, values: &[f64]) -> f32 {
    let x = test_signal(SR as usize);
    let (mut l, mut r) = (x.clone(), x);
    let p: Vec<f32> = values.iter().map(|v| *v as f32).collect();
    Saturator::new(SR).process(curve, &p, &mut l, &mut r);
    // Skip the fade-in of the first frames.
    db(common::rms(&l[4800..]))
}

#[test]
fn drive_sounds_keep_the_loudness() {
    let input = db(common::rms(&test_signal(SR as usize)[4800..]));
    let mut bad = Vec::new();
    for s in fxpresets::presets(BuiltinFxKind::Saturator) {
        let curve = s.curve.unwrap();
        let got = saturated_rms_db(curve, s.values);
        // The output gain that would match exactly, for refreshing the table.
        let mut v = s.values.to_vec();
        v[3] = 0.0;
        let exact = input - saturated_rms_db(curve, &v);
        eprintln!(
            "{:14} output {:6.2} dB: {:+.2} dB off input; exact output {:.1}",
            s.name,
            s.values[3],
            got - input,
            exact
        );
        bad.push((s.name, got - input));
    }
    bad.retain(|b| b.1.abs() > 1.0);
    assert!(bad.is_empty(), "{bad:?}");
}

// ---- helpers for projects built with the shared chain builders -----------------

use common::*;
use control::mcp::fxchain::{self, DuckArgs, LoudnessArgs};
use control::mcp::ids::IdGen;
use doc::document::{Document, apply_batch};
use engine::rt::{RtGuard, rt_events};
use protocol::edit::Edit;
use protocol::ids::{ChannelId, FIRST_ID};
use protocol::model::{Project, SynthParams, Wave};

fn apply(p: &Project, edits: &[Edit]) -> Project {
    let d = Document::from_project(p.clone(), FIRST_ID);
    let (d, _) = apply_batch(&d, edits).expect("edits apply");
    (*d.project).clone()
}

fn named(mut c: protocol::model::Channel, name: &str) -> protocol::model::Channel {
    c.name = name.into();
    c
}

fn voice(wave: Wave, gain_db: f64) -> SynthParams {
    let mut s = tone_params();
    s.osc1.wave = wave;
    s.osc2.wave = wave;
    s.gain_db = gain_db;
    s
}

/// Kick on beats 1 to 4 (track 1, muted so only the key is heard) and a
/// sustained bass (track 2).
fn duck_project() -> Project {
    let mut kick_track = track(201);
    kick_track.mix.mute = true;
    let kicks: Vec<N> = (0..4).map(|i| (5000 + i, i * 960, 240, 48, 127)).collect();
    project(
        120.0,
        vec![kick_track, track(202)],
        vec![
            named(synth_channel(1, 201, voice(Wave::Sine, 0.0)), "Kick"),
            named(synth_channel(2, 202, voice(Wave::Sine, 0.0)), "808"),
        ],
        vec![pattern(
            1,
            16,
            &[(1, kicks), (2, vec![(5100, 0, 3840, 33, 127)])],
        )],
    )
}

fn ducked(p: &Project, percent: f64) -> Project {
    let a = DuckArgs {
        row: Some(ChannelId(2)),
        track: None,
        amount: Some(percent),
        kick: None,
    };
    let b = fxchain::duck_to_kick(p, &mut IdGen::new(p, Some(FIRST_ID)), &a).unwrap();
    apply(p, &b.edits)
}

fn render(p: &Project, frames: usize) -> (Vec<f32>, Vec<f32>) {
    rig(p, SR, true).run(frames, 256)
}

/// RMS of the 808 in the 10 to 80 ms after the kick at beat `beat`.
fn after_kick(x: &[f32], beat: usize) -> f32 {
    let at = beat * 24000;
    common::rms(&x[at + 480..at + 3840])
}

#[test]
fn duck_to_kick_dips_the_bass_at_every_kick() {
    let base = duck_project();
    let (plain, _) = render(&base, 96000);
    for percent in [100.0, 50.0] {
        let (d, _) = render(&ducked(&base, percent), 96000);
        // Beats 1 to 3 (beat 0 starts with the effect fading in).
        let dips: Vec<f32> = (1..4)
            .map(|b| db(after_kick(&plain, b) / after_kick(&d, b)))
            .collect();
        eprintln!("duck {percent}%: dips {dips:?} dB");
        if percent == 100.0 {
            assert!(dips.iter().all(|d| *d >= 6.0), "{dips:?}");
        } else {
            assert!(dips.iter().all(|d| *d >= 3.0 && *d < 18.0), "{dips:?}");
        }
        // The bass comes back before the next kick.
        let late = |x: &[f32], b: usize| common::rms(&x[b * 24000 + 19200..b * 24000 + 23800]);
        let back = db(late(&plain, 1) / late(&d, 1));
        assert!(back < 3.0, "recovers: still {back} dB down");
    }
}

#[test]
fn duck_to_kick_is_idempotent_and_can_be_turned_off() {
    let base = duck_project();
    let d = ducked(&base, 100.0);
    let t = d.track(protocol::ids::TrackId(202)).unwrap();
    assert_eq!(t.inserts.len(), 1);
    // Again: the same compressor is reused, nothing changes.
    let a = DuckArgs {
        row: Some(ChannelId(2)),
        track: None,
        amount: Some(100.0),
        kick: None,
    };
    let b = fxchain::duck_to_kick(&d, &mut IdGen::new(&d, Some(FIRST_ID)), &a).unwrap();
    assert!(b.edits.is_empty(), "{:?}", b.edits);
    // A different amount changes values only.
    let e = ducked(&d, 40.0);
    assert_eq!(
        e.track(protocol::ids::TrackId(202)).unwrap().inserts.len(),
        1
    );
    let (_, amount) =
        fxchain::duck_state(&e, e.track(protocol::ids::TrackId(202)).unwrap()).unwrap();
    assert!((amount - 0.4).abs() < 0.05, "{amount}");
    // Off removes it.
    let off = ducked(&d, 0.0);
    assert!(
        off.track(protocol::ids::TrackId(202))
            .unwrap()
            .inserts
            .is_empty()
    );
    // Ducking the kick to itself is refused in plain words.
    let own = DuckArgs {
        row: Some(ChannelId(1)),
        track: None,
        amount: None,
        kick: Some(ChannelId(1)),
    };
    assert!(fxchain::duck_to_kick(&base, &mut IdGen::new(&base, None), &own).is_err());
}

// ---- Loudness ----------------------------------------------------------------------

/// A loud-ish beat: kick, sustained saw 808, and a square lead in eighths.
fn loud_beat() -> Project {
    let kicks: Vec<N> = (0..4).map(|i| (5000 + i, i * 960, 360, 36, 127)).collect();
    let lead: Vec<N> = (0..8)
        .map(|i| {
            (
                5200 + i,
                i * 480,
                360,
                [69, 72, 76, 72][(i % 4) as usize],
                110,
            )
        })
        .collect();
    project(
        120.0,
        vec![track(201), track(202), track(203)],
        vec![
            named(synth_channel(1, 201, voice(Wave::Sine, -3.0)), "Kick"),
            named(synth_channel(2, 202, voice(Wave::Saw, -9.0)), "808"),
            named(synth_channel(3, 203, voice(Wave::Square, -16.0)), "Lead"),
        ],
        vec![pattern(
            1,
            16,
            &[(1, kicks), (2, vec![(5100, 0, 3840, 31, 127)]), (3, lead)],
        )],
    )
}

fn with_loudness(p: &Project, a: LoudnessArgs) -> Project {
    let b = fxchain::loudness(p, &mut IdGen::new(p, Some(FIRST_ID)), &a).unwrap();
    apply(p, &b.edits)
}

fn measure(p: &Project) -> (f64, f64) {
    let (l, r) = render(p, 48000 * 8);
    let frames: Vec<[f32; 2]> = l.iter().zip(&r).map(|(a, b)| [*a, *b]).collect();
    // Skip the first second: the effects fade in.
    let frames = &frames[48000..];
    (
        control::analysis::integrated_loudness(frames, 48000),
        control::analysis::true_peak_dbtp(frames),
    )
}

#[test]
fn loudness_hard_reaches_the_target_under_the_ceiling() {
    let base = loud_beat();
    let (l0, tp0) = measure(&base);
    eprintln!("no loudness: {l0:.1} LUFS, {tp0:.2} dBTP");
    let mut seen = Vec::new();
    for name in ["clean", "punchy", "hard"] {
        let p = with_loudness(
            &base,
            LoudnessArgs {
                amount: None,
                preset: Some(name.into()),
            },
        );
        let (l, tp) = measure(&p);
        eprintln!("{name}: {l:.1} LUFS, {tp:.2} dBTP");
        assert!(tp <= -0.3, "{name}: true peak {tp} dBTP");
        seen.push(l);
    }
    assert!(seen[0] < seen[1] && seen[1] < seen[2], "{seen:?}");
    assert!((-8.0..=-6.0).contains(&seen[2]), "hard: {} LUFS", seen[2]);
}

#[test]
fn loudness_is_one_chain_that_later_calls_only_retune() {
    let base = loud_beat();
    let a = |v: f64| LoudnessArgs {
        amount: Some(v),
        preset: None,
    };
    let p = with_loudness(&base, a(9.0));
    let m = p.track(protocol::ids::TrackId::MASTER).unwrap();
    assert_eq!(m.inserts.len(), 3);
    assert!((fxchain::loudness_state(m).unwrap() - 9.0).abs() < 1e-6);
    let q = with_loudness(&p, a(4.0));
    assert_eq!(
        q.track(protocol::ids::TrackId::MASTER)
            .unwrap()
            .inserts
            .len(),
        3
    );
    let b = fxchain::loudness(&q, &mut IdGen::new(&q, None), &a(4.0)).unwrap();
    assert!(b.edits.is_empty());
}

#[test]
fn the_chains_make_no_allocations_and_stay_finite() {
    let p = with_loudness(
        &ducked(&loud_beat_with_names(), 100.0),
        LoudnessArgs {
            amount: Some(10.0),
            preset: None,
        },
    );
    let mut r = rig(&p, SR, true);
    let mut l = vec![0.0f32; 300];
    let mut rr = vec![0.0f32; 300];
    let before = rt_events();
    for _ in 0..600 {
        let g = RtGuard::enter_counting();
        r.rt.process_planar(&mut l, &mut rr);
        drop(g);
        assert!(l.iter().chain(&rr).all(|x| x.is_finite()));
    }
    assert_eq!(rt_events(), before);
}

fn loud_beat_with_names() -> Project {
    // Kick on track 1, the 808 on track 2, which `ducked` targets.
    loud_beat()
}

#[test]
fn the_live_loudness_reading_agrees_with_the_offline_analysis() {
    let p = with_loudness(
        &loud_beat(),
        LoudnessArgs {
            amount: None,
            preset: Some("hard".into()),
        },
    );
    let mut r = rig(&p, SR, true);
    let (l, rr) = r.run(48000 * 10, 256);
    let live = r
        .shared
        .loudness
        .lufs()
        .expect("a reading after 10 s of play");
    let frames: Vec<[f32; 2]> = l.iter().zip(&rr).map(|(a, b)| [*a, *b]).collect();
    // The ring holds the last 10 s, which is all of it.
    let offline = control::analysis::integrated_loudness(&frames, 48000);
    eprintln!("live {live:.2} LUFS, offline {offline:.2} LUFS");
    assert!((live - offline).abs() < 0.3, "{live} vs {offline}");
}
