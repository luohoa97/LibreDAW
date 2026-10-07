// SPDX-License-Identifier: GPL-3.0-or-later
//! The sampler through the runtime: playback, missing samples, choke groups
//! with sub-block splitting and the 1.5 ms fade (SPEC 15.1, 17.2).

mod common;

use common::*;
use engine::runtime::{Runtime, Shared, rings};
use engine::{SampleData, SampleStore, Slots, compile_with, write_controls};
use protocol::beats::{SampleMode, Sampler, SamplerParams};
use protocol::engine::{ChannelSlot, EngineCommand};
use protocol::ids::{ChannelId, TrackId};
use protocol::model::{Channel, Instrument, Mix, Project};

const SR: f64 = 48000.0;

fn sampler_channel(id: u32, hash: Option<&str>, mode: SampleMode, group: u8) -> Channel {
    Channel {
        id: ChannelId(id),
        name: format!("s{id}"),
        root_key: 60,
        track: TrackId(0),
        mix: Mix::default(),
        instrument: Instrument::Sampler(Sampler {
            sample: hash.map(str::to_string),
            mode,
            reverse: false,
            params: SamplerParams::default(),
        }),
        choke_group: group,
    }
}

fn store_with(items: &[(&str, SampleData)]) -> SampleStore {
    let s = SampleStore::new(SR as u32, 1 << 28);
    for (h, d) in items {
        s.insert(h, d.clone());
    }
    s
}

fn rig_samples(p: &Project, store: &SampleStore) -> Rig {
    let mut slots = Slots::new();
    slots.sync(p).unwrap();
    let shared = Shared::new();
    write_controls(p, &slots, &shared.controls, &shared.params);
    let (ui, ends) = rings();
    let mut rt = Runtime::new(SR, shared.clone(), ends);
    rt.enable_trace(1 << 12);
    rt.install(compile_with(p, &slots, SR, Some(store)));
    rt.command(EngineCommand::SetPlayingPattern {
        pattern: p.patterns[0].id,
    });
    rt.command(EngineCommand::Play);
    Rig {
        rt,
        ui,
        shared,
        slots,
        sr: SR,
    }
}

fn ones(frames: usize) -> SampleData {
    SampleData::from_vec(1, SR as u32, vec![1.0; frames])
}

#[test]
fn a_one_shot_plays_through_the_mixer() {
    let p = project(
        120.0,
        vec![],
        vec![sampler_channel(1, Some("h"), SampleMode::OneShot, 0)],
        vec![pattern(1, 16, &[(1, vec![(1, 0, 240, 60, 127)])])],
    );
    let store = store_with(&[("h", ones(2400))]);
    let mut r = rig_samples(&p, &store);
    let (l, rr) = r.run(4800, 256);
    // Mono source, -3 dB pan law at the centre.
    let want = std::f32::consts::FRAC_1_SQRT_2;
    assert!((l[100] - want).abs() < 1e-3, "{}", l[100]);
    assert!((rr[100] - want).abs() < 1e-3);
    assert!(l[2400..].iter().all(|x| *x == 0.0), "the sample ended");
    assert_eq!(r.rt.active_voices(ChannelSlot(0)), 0);
}

#[test]
fn missing_or_undecoded_samples_play_silence() {
    let notes = vec![(1, 0, 240, 60, 127)];
    for hash in [None, Some("not-in-store")] {
        let p = project(
            120.0,
            vec![],
            vec![sampler_channel(1, hash, SampleMode::OneShot, 0)],
            vec![pattern(1, 16, &[(1, notes.clone())])],
        );
        let store = store_with(&[]);
        let mut r = rig_samples(&p, &store);
        let (l, rr) = r.run(4800, 256);
        assert_eq!(peak(&l), 0.0);
        assert_eq!(peak(&rr), 0.0);
    }
}

/// Open hat (channel 1, group 1) rings; a closed hat (channel 2, group 1)
/// is triggered at frame 100 and chokes it. The closed hat's own sample is
/// silent so the master output is the open hat alone.
fn hat_project(open_group: u8, closed_group: u8) -> Project {
    project(
        120.0,
        vec![],
        vec![
            sampler_channel(1, Some("open"), SampleMode::OneShot, open_group),
            sampler_channel(2, Some("closed"), SampleMode::OneShot, closed_group),
        ],
        // 120 BPM at 48 kHz is 25 samples per tick: tick 4 is frame 100.
        vec![pattern(
            1,
            16,
            &[
                (1, vec![(1, 0, 240, 60, 127)]),
                (2, vec![(2, 4, 240, 60, 127)]),
            ],
        )],
    )
}

fn hat_store() -> SampleStore {
    store_with(&[
        ("open", ones(48000)),
        (
            "closed",
            SampleData::from_vec(1, SR as u32, vec![0.0; 48000]),
        ),
    ])
}

#[test]
fn a_closed_hat_silences_the_open_hat_within_the_fade() {
    let fade = (0.0015 * SR).round() as usize; // 72 frames
    assert_eq!(fade, 72);
    let store = hat_store();
    for cb in [1usize, 37, 100, 101, 256, 1000] {
        let mut r = rig_samples(&hat_project(1, 1), &store);
        let (l, rr) = r.run(2400, cb);
        let g = std::f32::consts::FRAC_1_SQRT_2;
        assert!(
            (l[99] - g).abs() < 1e-3,
            "full level before the choke, cb {cb}"
        );
        // The fade is a linear ramp from the choke frame.
        let half = l[100 + fade / 2];
        assert!(half > 0.2 * g && half < 0.8 * g, "mid-fade {half} cb {cb}");
        assert!(l[100] > 0.9 * g, "fade starts at frame 100, cb {cb}");
        assert!(
            l[100 + fade..].iter().all(|x| *x == 0.0),
            "silent by frame 100 + fade, cb {cb}"
        );
        assert!(rr[100 + fade..].iter().all(|x| *x == 0.0));
        assert_eq!(
            r.rt.active_voices(ChannelSlot(0)),
            0,
            "voice freed, cb {cb}"
        );
    }
}

#[test]
fn choke_output_is_identical_for_every_callback_size() {
    let store = hat_store();
    let mut a = rig_samples(&hat_project(1, 1), &store);
    let (la, _) = a.run(2400, 37);
    let mut b = rig_samples(&hat_project(1, 1), &store);
    let (lb, _) = b.run(2400, 1000);
    assert_eq!(la, lb);
}

#[test]
fn channels_in_other_groups_are_not_choked_and_group_zero_never_chokes() {
    let store = hat_store();
    for (og, cg) in [(1u8, 2u8), (0, 0), (0, 1), (1, 0)] {
        let mut r = rig_samples(&hat_project(og, cg), &store);
        let (l, _) = r.run(2400, 256);
        assert!(l[2000] > 0.5, "open hat keeps ringing for groups {og}/{cg}");
    }
}

#[test]
fn a_channel_does_not_choke_itself() {
    // Two notes on one channel in a group: the first keeps playing.
    let p = project(
        120.0,
        vec![],
        vec![sampler_channel(1, Some("open"), SampleMode::OneShot, 1)],
        vec![pattern(
            1,
            16,
            &[(1, vec![(1, 0, 240, 60, 127), (2, 4, 240, 60, 127)])],
        )],
    );
    let store = hat_store();
    let mut r = rig_samples(&p, &store);
    let (l, _) = r.run(2400, 256);
    assert!(l[2000] > 0.9, "both voices sound: {}", l[2000]);
}

#[test]
fn the_sampler_path_makes_no_allocations() {
    use engine::rt::{RtGuard, rt_events};
    let store = hat_store();
    let mut r = rig_samples(&hat_project(1, 1), &store);
    let mut l = vec![0.0f32; 300];
    let mut rr = vec![0.0f32; 300];
    let before = rt_events();
    for _ in 0..200 {
        let g = RtGuard::enter_counting();
        r.rt.process_planar(&mut l, &mut rr);
        drop(g);
    }
    assert_eq!(rt_events(), before);
}

#[test]
fn a_sample_arriving_in_a_later_compile_starts_to_sound() {
    let p = project(
        120.0,
        vec![],
        vec![sampler_channel(1, Some("late"), SampleMode::OneShot, 0)],
        vec![pattern(1, 16, &[(1, vec![(1, 0, 240, 60, 127)])])],
    );
    let store = store_with(&[]);
    let mut r = rig_samples(&p, &store);
    let (l, _) = r.run(2400, 256);
    assert_eq!(peak(&l), 0.0);
    store.insert("late", ones(48000));
    // The caller recompiles when the loader reports; the next loop sounds.
    r.rt.install(compile_with(&p, &r.slots, SR, Some(&store)));
    let (l, _) = r.run(96000, 256);
    assert!(peak(&l) > 0.5);
}
