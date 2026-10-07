// SPDX-License-Identifier: GPL-3.0-or-later
//! Sound-browser audition (SPEC 20.3): a dedicated voice set into the
//! master, independent of instruments, auto-release, replacement, samples
//! by hash, and no allocation on the audio thread.

mod common;

use common::*;
use engine::SampleData;
use engine::audition::AuditionSample;
use engine::rt::{RtGuard, rt_events};
use protocol::beats::Bass808Params;
use protocol::engine::{AuditionSource, EngineCommand, PREVIEW_MAX_SECONDS};

const SR: f64 = 48000.0;
const HASH: [u8; 32] = [7; 32];

/// A project with one muted instrument: audition must not depend on it.
fn quiet_project() -> protocol::model::Project {
    let mut p = project(
        120.0,
        vec![],
        vec![synth_channel(1, 0, tone_params())],
        vec![pattern(1, 16, &[])],
    );
    let mut c = (*p.channels[0]).clone();
    c.mix.mute = true;
    p.channels[0] = std::sync::Arc::new(c);
    p
}

fn synth_on(key: u8, on: bool) -> EngineCommand {
    EngineCommand::Audition {
        source: AuditionSource::Synth(tone_params()),
        key,
        vel: 100,
        on,
    }
}

/// Linear level of `x` at `freq` (single-bin DFT, unit sine = 1).
fn tone_level(x: &[f32], sr: f64, freq: f64) -> f64 {
    let (mut re, mut im) = (0.0, 0.0);
    for (i, v) in x.iter().enumerate() {
        let a = std::f64::consts::TAU * freq * i as f64 / sr;
        re += *v as f64 * a.cos();
        im += *v as f64 * a.sin();
    }
    2.0 * (re * re + im * im).sqrt() / x.len() as f64
}

fn sine(rate: u32, hz: f64) -> SampleData {
    let v = (0..rate)
        .map(|i| (std::f64::consts::TAU * hz * i as f64 / rate as f64).sin() as f32 * 0.8)
        .collect();
    SampleData::from_vec(1, rate, v)
}

#[test]
fn a_synth_audition_sounds_on_the_master_without_instruments() {
    let mut r = rig(&quiet_project(), SR, false);
    r.rt.command(synth_on(69, true));
    let (l, rr) = r.run(4800, 256);
    assert!(peak(&l) > 0.1 && peak(&rr) > 0.1, "{}", peak(&l));
    // Not playing the transport: the audition is independent of it.
    assert!(!r.rt.is_playing());
    r.rt.command(synth_on(69, false));
    r.run(2400, 256);
    let (l, _) = r.run(2400, 256);
    assert!(peak(&l) < 1e-4, "released: {}", peak(&l));
}

#[test]
fn an_audition_releases_itself_after_the_maximum_time() {
    let mut r = rig(&quiet_project(), SR, false);
    r.rt.command(synth_on(69, true));
    let hold = (PREVIEW_MAX_SECONDS * SR) as usize;
    let (l, _) = r.run(hold - 2000, 512);
    assert!(peak(&l[l.len() - 2000..]) > 0.1, "still held");
    r.run(4000, 512);
    let (l, _) = r.run(2000, 512);
    assert!(peak(&l) < 1e-4, "auto-released: {}", peak(&l));
}

#[test]
fn a_new_audition_replaces_the_previous_one() {
    let mut r = rig(&quiet_project(), SR, false);
    r.rt.command(synth_on(69, true));
    r.run(4800, 256);
    r.rt.command(synth_on(81, true));
    r.run(4800, 256);
    let (l, _) = r.run(9600, 256);
    let low = tone_level(&l, SR, 440.0);
    let high = tone_level(&l, SR, 880.0);
    assert!(high > 0.2, "{high}");
    assert!(low < high * 0.05, "old note still sounds: {low} vs {high}");
}

#[test]
fn an_808_audition_sounds() {
    let mut r = rig(&quiet_project(), SR, false);
    r.rt.command(EngineCommand::Audition {
        source: AuditionSource::Bass808(Bass808Params::default()),
        key: 36,
        vel: 110,
        on: true,
    });
    let (l, rr) = r.run(9600, 256);
    assert!(peak(&l) > 0.1 && peak(&rr) > 0.1);
}

#[test]
fn a_sample_audition_plays_a_delivered_sample_and_silences_a_missing_one() {
    let mut r = rig(&quiet_project(), SR, false);
    // Not delivered: silence.
    r.rt.command(EngineCommand::Audition {
        source: AuditionSource::Sample { hash: HASH },
        key: 60,
        vel: 100,
        on: true,
    });
    let (l, _) = r.run(4800, 256);
    assert_eq!(peak(&l), 0.0);

    // Delivered before the command, as `Engine::audition` does.
    let first = sine(48000, 480.0);
    r.ui.audition
        .push(AuditionSample {
            hash: HASH,
            data: first.clone(),
        })
        .unwrap();
    r.rt.command(EngineCommand::Audition {
        source: AuditionSource::Sample { hash: HASH },
        key: 60,
        vel: 100,
        on: true,
    });
    let (l, _) = r.run(9600, 256);
    assert!(peak(&l) > 0.3, "{}", peak(&l));
    assert!(tone_level(&l, SR, 480.0) > 0.2);

    // A second sample replaces the first; the first leaves through the
    // retire ring instead of being freed on the audio thread.
    r.ui.audition
        .push(AuditionSample {
            hash: [9; 32],
            data: sine(48000, 960.0),
        })
        .unwrap();
    r.rt.command(EngineCommand::Audition {
        source: AuditionSource::Sample { hash: [9; 32] },
        key: 60,
        vel: 100,
        on: true,
    });
    assert!(r.ui.audition_retired.pop().is_ok());
    r.run(4800, 256);
    let (l, _) = r.run(9600, 256);
    assert!(tone_level(&l, SR, 960.0) > 0.2);
    assert!(tone_level(&l, SR, 480.0) < 0.02);
    // The old hash is no longer held.
    r.rt.command(EngineCommand::Audition {
        source: AuditionSource::Sample { hash: HASH },
        key: 60,
        vel: 100,
        on: true,
    });
    r.run(4800, 256);
    let (l, _) = r.run(9600, 256);
    assert!(peak(&l) < 1e-3);
}

#[test]
fn audition_through_the_rings_makes_no_allocations() {
    let mut r = rig(&quiet_project(), SR, true);
    let mut l = vec![0.0f32; 300];
    let mut rr = vec![0.0f32; 300];
    let before = rt_events();
    for round in 0..40 {
        let cmd = match round % 4 {
            0 => synth_on(60 + (round % 12) as u8, true),
            1 => EngineCommand::Audition {
                source: AuditionSource::Bass808(Bass808Params::default()),
                key: 40,
                vel: 100,
                on: true,
            },
            2 => EngineCommand::Audition {
                source: AuditionSource::Sample { hash: HASH },
                key: 60,
                vel: 100,
                on: true,
            },
            _ => synth_on(60, false),
        };
        if round % 4 == 2 {
            let _ = r.ui.audition.push(AuditionSample {
                hash: HASH,
                data: sine(48000, 300.0),
            });
        }
        r.ui.commands.push(cmd).unwrap();
        let g = RtGuard::enter_counting();
        for _ in 0..20 {
            r.rt.process_planar(&mut l, &mut rr);
        }
        drop(g);
        // The UI side frees what the audio thread retired.
        while r.ui.audition_retired.pop().is_ok() {}
    }
    assert_eq!(rt_events(), before, "the audio path allocated or freed");
}
