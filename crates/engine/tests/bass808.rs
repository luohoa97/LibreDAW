// SPDX-License-Identifier: GPL-3.0-or-later
//! The 808 through the runtime: overlapping notes, callback independence,
//! no allocation (SPEC 15.2, 17.2).

mod common;

use common::*;
use engine::rt::{RtGuard, rt_events};
use protocol::beats::Bass808;
use protocol::engine::ChannelSlot;
use protocol::ids::ChannelId;
use protocol::model::{Channel, Instrument, Mix};

fn channel(mono: bool) -> Channel {
    Channel {
        id: ChannelId(1),
        name: "808".into(),
        root_key: 36,
        track: protocol::ids::TrackId(0),
        mix: Mix::default(),
        instrument: Instrument::Bass808(Bass808 {
            mono,
            ..Bass808::default()
        }),
        choke_group: 0,
    }
}

fn proj(mono: bool) -> protocol::model::Project {
    project(
        120.0,
        vec![],
        vec![channel(mono)],
        // Overlapping: note 2 starts while note 1 is held (legato in mono).
        vec![pattern(
            1,
            16,
            &[(1, vec![(1, 0, 960, 36, 100), (2, 480, 960, 43, 110)])],
        )],
    )
}

#[test]
fn overlapping_808_notes_are_one_voice_in_mono_and_two_in_poly() {
    let mut r = rig(&proj(true), 48000.0, true);
    let (l, _) = r.run(36000, 256); // 0.75 s: both notes are held at 0.25 s
    assert!(peak(&l) > 0.05);
    assert!(l.iter().all(|x| x.is_finite()));
    assert_eq!(r.rt.active_voices(ChannelSlot(0)), 1);

    let mut p = rig(&proj(false), 48000.0, true);
    p.run(18000, 256);
    assert_eq!(p.rt.active_voices(ChannelSlot(0)), 2);
}

#[test]
fn output_does_not_depend_on_the_callback_size() {
    let p = proj(true);
    let mut a = rig(&p, 48000.0, true);
    let mut b = rig(&p, 48000.0, true);
    let (la, ra) = a.run(60000, 37);
    let (lb, rb) = b.run(60000, 1000);
    assert_eq!(la, lb);
    assert_eq!(ra, rb);
}

#[test]
fn the_808_path_makes_no_allocations() {
    let mut r = rig(&proj(true), 48000.0, true);
    let mut l = vec![0.0f32; 300];
    let mut rr = vec![0.0f32; 300];
    let before = rt_events();
    for _ in 0..400 {
        let g = RtGuard::enter_counting();
        r.rt.process_planar(&mut l, &mut rr);
        drop(g);
    }
    assert_eq!(rt_events(), before);
}
