// SPDX-License-Identifier: GPL-3.0-or-later
//! Built-in effects and routing through the runtime: sends, solo, the
//! sidechain tap, pool handling, parameters from the table, the limiter,
//! and the real-time rules (SPEC 15.5, 17.2).

mod common;

use common::*;
use engine::rt::{RtGuard, rt_events};
use protocol::beats::{BuiltinFx, BuiltinFxKind, EqParam, LimiterParam};
use protocol::engine::{TrackSlot, fx_param_index};
use protocol::ids::{InstanceId, TrackId};
use protocol::model::{Insert, Project, Send, Track};

const SR: f64 = 48000.0;

fn fx_insert(id: u32, fx: BuiltinFx) -> Insert {
    Insert::Builtin {
        instance: InstanceId(id),
        fx,
        bypass: false,
    }
}

fn with(t: Track, f: impl FnOnce(&mut Track)) -> Track {
    let mut t = t;
    f(&mut t);
    t
}

fn long_tone(ch: u32) -> (u32, Vec<N>) {
    // A steady A440 for the whole 2 s pattern.
    (ch, vec![(ch * 100, 0, 3840, 69, 127)])
}

/// Channel 1 on track 1, a return on track 2, a send 1 -> 2.
fn send_project(pre: bool, level_db: f64, vol1_db: f64) -> Project {
    let t1 = with(track(1), |t| {
        t.mix.volume_db = vol1_db;
        t.sends.push(Send {
            to: TrackId(2),
            level_db,
            pre_fader: pre,
        });
    });
    project(
        120.0,
        vec![t1, track(2)],
        vec![synth_channel(1, 1, tone_params())],
        vec![pattern(1, 16, &[long_tone(1)])],
    )
}

fn master_rms(p: &Project) -> f32 {
    let mut r = rig(p, SR, true);
    let (l, _) = r.run(24000, 256);
    rms(&l[12000..])
}

#[test]
fn a_post_fader_send_follows_the_track_fader_and_a_pre_fader_send_does_not() {
    let base = {
        let mut p = send_project(false, -6.0, -12.0);
        // No send for the reference.
        Arc_make_mut_track(&mut p, 1, |t| t.sends.clear());
        master_rms(&p)
    };
    let g = |db: f32| 10f32.powf(db / 20.0);
    let post = master_rms(&send_project(false, -6.0, -12.0));
    let pre = master_rms(&send_project(true, -6.0, -12.0));
    // Fader -12 dB. Post: f * x + g(-6) * f * x. Pre: f * x + g(-6) * x.
    assert!(
        (post / base - (1.0 + g(-6.0))).abs() < 0.01,
        "{}",
        post / base
    );
    assert!(
        (pre / base - (1.0 + g(-6.0) / g(-12.0))).abs() < 0.01,
        "{}",
        pre / base
    );
}

#[allow(non_snake_case)]
fn Arc_make_mut_track(p: &mut Project, id: u32, f: impl FnOnce(&mut Track)) {
    let t = p.tracks.iter_mut().find(|t| t.id == TrackId(id)).unwrap();
    f(std::sync::Arc::make_mut(t));
}

#[test]
fn a_send_chain_through_two_returns_is_summed_in_topological_order() {
    // 1 -> 3 -> 2, with slot order against the signal flow.
    let t1 = with(track(1), |t| {
        t.sends.push(Send {
            to: TrackId(3),
            level_db: 0.0,
            pre_fader: false,
        });
        t.mix.mute = false;
    });
    let t3 = with(track(3), |t| {
        t.sends.push(Send {
            to: TrackId(2),
            level_db: 0.0,
            pre_fader: false,
        })
    });
    let mut p = project(
        120.0,
        vec![t1, track(2), t3],
        vec![synth_channel(1, 1, tone_params())],
        vec![pattern(1, 16, &[long_tone(1)])],
    );
    let chain = master_rms(&p);
    Arc_make_mut_track(&mut p, 1, |t| t.sends.clear());
    Arc_make_mut_track(&mut p, 3, |t| t.sends.clear());
    let alone = master_rms(&p);
    // Three equal copies reach the master: the source, return 3 (which
    // sums the send) and return 2 (which sums the send from 3).
    assert!((chain / alone - 3.0).abs() < 0.02, "{}", chain / alone);
}

#[test]
fn solo_on_a_source_keeps_its_return_and_mute_silences_the_sends() {
    let mut p = send_project(true, 0.0, 0.0);
    let heard = master_rms(&p);
    // Solo the source: the return stays audible.
    Arc_make_mut_track(&mut p, 1, |t| t.mix.solo = true);
    let solo = master_rms(&p);
    assert!((solo / heard - 1.0).abs() < 0.01, "{}", solo / heard);
    // Mute the source: nothing reaches the master through either path.
    Arc_make_mut_track(&mut p, 1, |t| {
        t.mix.solo = false;
        t.mix.mute = true;
    });
    assert!(master_rms(&p) < 1e-6);
    // Solo something else: the source and its return go quiet.
    let mut q = send_project(true, 0.0, 0.0);
    let other = project(
        120.0,
        vec![
            std::sync::Arc::unwrap_or_clone(q.tracks[1].clone()),
            track(2),
            with(track(3), |t| t.mix.solo = true),
        ],
        vec![synth_channel(1, 1, tone_params())],
        vec![pattern(1, 16, &[long_tone(1)])],
    );
    q = other;
    assert!(master_rms(&q) < 1e-6);
}

/// A kick-like channel 1 on track 1 and a steady tone on track 2, which has
/// a compressor keyed by track 1.
fn duck_project(mute1: bool, solo2: bool) -> Project {
    let comp = BuiltinFx::Compressor {
        params: protocol::beats::CompressorParams {
            threshold_db: -30.0,
            ratio: 20.0,
            attack_ms: 0.5,
            release_ms: 50.0,
            knee_db: 0.0,
            ..Default::default()
        },
        sidechain: Some(TrackId(1)),
    };
    let t1 = with(track(1), |t| {
        t.mix.mute = mute1;
    });
    let t2 = with(track(2), |t| {
        t.mix.solo = solo2;
        t.inserts.push(fx_insert(10, comp));
    });
    // Kicks: 1/8 of a second on, every quarter second (120 BPM: 480 ticks).
    let kicks: Vec<N> = (0..8).map(|i| (100 + i, i * 480, 240, 60, 127)).collect();
    project(
        120.0,
        vec![t1, t2],
        vec![
            synth_channel(1, 1, tone_params()),
            synth_channel(2, 2, tone_params()),
        ],
        vec![pattern(
            1,
            16,
            &[(1, kicks), (2, vec![(200, 0, 3840, 81, 100)])],
        )],
    )
}

fn render(p: &Project, frames: usize) -> Vec<f32> {
    let mut r = rig(p, SR, true);
    r.run(frames, 256).0
}

#[test]
fn sidechain_ducking_ignores_mute_and_solo_of_the_key_track() {
    let frames = 48000;
    // A: both audible. B: key track muted. C: bass track soloed (the key
    // track is silenced by solo). D: the key track alone.
    let a = render(&duck_project(false, false), frames);
    let b = render(&duck_project(true, false), frames);
    let c = render(&duck_project(false, true), frames);
    // The bass alone, ducked: same signal in B and C, bit for bit.
    assert_eq!(
        b, c,
        "ducking is the same with the key track muted or soloed out"
    );
    // And it is ducked: during a kick the bass is much quieter than between.
    let during = rms(&b[1000..5000]);
    let between = rms(&b[18000..22000]);
    assert!(during < between * 0.3, "{during} vs {between}");
    // A is the bass plus the audible key track.
    let d = {
        let mut p = duck_project(false, false);
        Arc_make_mut_track(&mut p, 2, |t| t.mix.mute = true);
        render(&p, frames)
    };
    for i in 0..frames {
        assert!((a[i] - (b[i] + d[i])).abs() < 1e-5, "frame {i}");
    }
}

#[test]
fn a_compressor_without_sidechain_keys_from_its_own_input() {
    let comp = BuiltinFx::Compressor {
        params: protocol::beats::CompressorParams {
            threshold_db: -30.0,
            ratio: 20.0,
            ..Default::default()
        },
        sidechain: None,
    };
    let t = with(track(1), |t| t.inserts.push(fx_insert(10, comp)));
    let p = project(
        120.0,
        vec![t],
        vec![synth_channel(1, 1, tone_params())],
        vec![pattern(1, 16, &[long_tone(1)])],
    );
    let bare = master_rms(&project(
        120.0,
        vec![track(1)],
        vec![synth_channel(1, 1, tone_params())],
        vec![pattern(1, 16, &[long_tone(1)])],
    ));
    assert!(master_rms(&p) < bare * 0.5);
}

#[test]
fn eq_parameters_come_from_the_param_table_while_playing() {
    let t = with(track(1), |t| {
        t.inserts
            .push(fx_insert(10, BuiltinFx::new(BuiltinFxKind::Eq)))
    });
    let p = project(
        120.0,
        vec![t],
        vec![synth_channel(1, 1, tone_params())],
        vec![pattern(1, 16, &[long_tone(1)])],
    );
    let mut r = rig(&p, SR, true);
    let (l, _) = r.run(12000, 256);
    let flat = rms(&l[6000..]);
    // Mid +12 dB at 440 Hz through the table, no recompile.
    let ts = r.slots.track_slot(TrackId(1)).unwrap();
    let set = |q: EqParam, v: f32| {
        r.shared
            .params
            .set(fx_param_index(TrackSlot(ts.0), 0, q.index()), v)
    };
    set(EqParam::MidHz, 440.0);
    set(EqParam::MidQ, 1.0);
    set(EqParam::MidGainDb, 12.0);
    r.run(4800, 256);
    let (l, _) = r.run(12000, 256);
    let boosted = rms(&l[6000..]);
    let ratio = boosted / flat;
    assert!((ratio - 3.98).abs() < 0.1, "{ratio}");
}

#[test]
fn the_master_limiter_holds_the_ceiling() {
    let mut p = project(
        120.0,
        vec![],
        vec![synth_channel(1, 0, tone_params())],
        vec![pattern(
            1,
            16,
            &[(1, vec![(1, 0, 3840, 69, 127), (2, 0, 3840, 72, 127)])],
        )],
    );
    Arc_make_mut_track(&mut p, 0, |t| {
        t.mix.volume_db = 12.0;
        t.inserts
            .push(fx_insert(10, BuiltinFx::new(BuiltinFxKind::Limiter)));
    });
    let mut r = rig(&p, SR, true);
    let ts = r.slots.track_slot(TrackId(0)).unwrap();
    r.shared.params.set(
        fx_param_index(TrackSlot(ts.0), 0, LimiterParam::CeilingDb.index()),
        -6.0,
    );
    let (l, rr) = r.run(24000, 256);
    // The limiter sits before the master fader (+12 dB), so after it the
    // ceiling is -6 dB + 12 dB = +6 dB at most. Check the pre-fader level by
    // removing the fader boost.
    let want = 10f32.powf(6.0 / 20.0);
    assert!(peak(&l) <= want + 1e-3 && peak(&rr) <= want + 1e-3);
    assert!(
        peak(&l) > want * 0.9,
        "it does reach the ceiling: {}",
        peak(&l)
    );
}

fn all_fx_project() -> Project {
    let mut ins = vec![
        fx_insert(10, BuiltinFx::new(BuiltinFxKind::Eq)),
        fx_insert(11, BuiltinFx::new(BuiltinFxKind::Compressor)),
        fx_insert(12, BuiltinFx::new(BuiltinFxKind::Saturator)),
        fx_insert(13, BuiltinFx::new(BuiltinFxKind::Delay)),
        fx_insert(14, BuiltinFx::new(BuiltinFxKind::Reverb)),
        fx_insert(15, BuiltinFx::new(BuiltinFxKind::Limiter)),
    ];
    if let Insert::Builtin {
        fx: BuiltinFx::Delay { ping_pong, .. },
        ..
    } = &mut ins[3]
    {
        *ping_pong = true;
    }
    let t1 = with(track(1), |t| {
        t.inserts = ins;
        t.sends.push(Send {
            to: TrackId(2),
            level_db: -3.0,
            pre_fader: true,
        });
    });
    let t2 = with(track(2), |t| {
        t.inserts
            .push(fx_insert(20, BuiltinFx::new(BuiltinFxKind::Reverb)))
    });
    project(
        120.0,
        vec![t1, t2],
        vec![
            synth_channel(1, 1, tone_params()),
            synth_channel(2, 2, tone_params()),
        ],
        vec![pattern(
            1,
            16,
            &[
                (1, vec![(1, 0, 960, 60, 100), (2, 1920, 480, 64, 100)]),
                (2, vec![(3, 960, 480, 67, 100)]),
            ],
        )],
    )
}

#[test]
fn every_effect_path_makes_no_allocations_and_stays_finite() {
    let mut r = rig(&all_fx_project(), SR, true);
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

#[test]
fn effect_output_does_not_depend_on_the_callback_size() {
    let p = all_fx_project();
    let a = {
        let mut r = rig(&p, SR, true);
        r.run(72000, 37)
    };
    let b = {
        let mut r = rig(&p, SR, true);
        r.run(72000, 1024)
    };
    assert_eq!(a.0, b.0);
    assert_eq!(a.1, b.1);
}

#[test]
fn a_newly_assigned_pool_entry_fades_in_and_starts_clean() {
    use engine::{Slots, compile};
    // One reverb insert; render, then replace it by a different instance:
    // the entry is reused (or another taken) and must start silent.
    let t = with(track(1), |t| {
        t.inserts
            .push(fx_insert(10, BuiltinFx::new(BuiltinFxKind::Reverb)))
    });
    let p = project(
        120.0,
        vec![t],
        vec![synth_channel(1, 1, tone_params())],
        vec![pattern(1, 16, &[(1, vec![(1, 0, 480, 69, 127)])])],
    );
    let mut r = rig(&p, SR, true);
    r.run(24000, 256);
    // New project without the note and a fresh reverb instance (id 11).
    let t = with(track(1), |t| {
        t.inserts
            .push(fx_insert(11, BuiltinFx::new(BuiltinFxKind::Reverb)))
    });
    let q = project(
        120.0,
        vec![t],
        vec![synth_channel(1, 1, tone_params())],
        vec![pattern(1, 16, &[])],
    );
    let mut slots: Slots = r.slots.clone();
    slots.sync(&q).unwrap();
    engine::write_controls(&q, &slots, &r.shared.controls, &r.shared.params);
    r.rt.install(compile(&q, &slots, SR));
    let (l, rr) = r.run(24000, 256);
    assert!(l.iter().chain(&rr).all(|x| x.abs() < 1e-9), "no old tail");
}

#[test]
fn pool_exhaustion_bypasses_the_extra_insert() {
    use engine::Slots;
    // Seventeen reverbs on distinct tracks cannot all get a pool entry of
    // 16; the extra insert is a bypass and nothing panics.
    let mut tracks = Vec::new();
    for i in 1..=17u32 {
        tracks.push(with(track(i), |t| {
            t.inserts
                .push(fx_insert(100 + i, BuiltinFx::new(BuiltinFxKind::Reverb)))
        }));
    }
    let p = project(120.0, tracks, vec![], vec![pattern(1, 16, &[])]);
    let mut s = Slots::new();
    s.sync(&p).unwrap();
    let c = engine::compile(&p, &s, SR);
    let bypassed = (1..=17)
        .filter(|&t| c.tracks[t].inserts == vec![engine::compiled::InsertC::Empty])
        .count();
    assert_eq!(bypassed, 1);
    // Removing one frees an entry.
    let mut p2 = p.clone();
    Arc_make_mut_track(&mut p2, 1, |t| t.inserts.clear());
    s.sync(&p2).unwrap();
    let c = engine::compile(&p2, &s, SR);
    assert!(
        (1..=17).all(|t| c.tracks[t].inserts != vec![engine::compiled::InsertC::Empty]),
        "the freed entry went to the 17th"
    );
}
