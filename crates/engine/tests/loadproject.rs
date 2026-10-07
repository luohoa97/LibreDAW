// SPDX-License-Identifier: GPL-3.0-or-later
//! The heavy load project (`loadbench`) is valid, audible, and its song
//! playback makes no allocation on the audio path.

use engine::loadproject::{load_project, load_store};
use engine::rt::{RtGuard, rt_events};
use engine::runtime::{Runtime, Shared, rings};
use engine::{Slots, compile_with, write_controls};
use protocol::engine::EngineCommand;

const SR: f64 = 48000.0;

#[test]
fn load_project_is_valid() {
    let p = load_project();
    protocol::validate::validate(&p).unwrap();
    assert_eq!(p.channels.len(), 16);
    assert_eq!(p.tracks.len(), 11);
    assert_eq!(p.clips.len(), 16 * 8);
    assert!(p.loop_region.enabled);
}

#[test]
fn load_project_song_makes_no_allocations_for_ten_seconds() {
    let p = load_project();
    let store = load_store(SR as u32);
    let mut slots = Slots::new();
    slots.sync(&p).unwrap();
    let shared = Shared::new();
    write_controls(&p, &slots, &shared.controls, &shared.params);
    let (_ui, ends) = rings();
    let mut rt = Runtime::new(SR, shared, ends);
    let _ = rt.install(compile_with(&p, &slots, SR, Some(&store)));
    rt.command(EngineCommand::Seek { tick: 0 });
    rt.command(EngineCommand::Play);

    let mut l = vec![0.0f32; 256];
    let mut r = vec![0.0f32; 256];
    let before = rt_events();
    let mut peak = 0.0f32;
    let mut finite = true;
    for _ in 0..(10.0 * SR / 256.0) as usize {
        let g = RtGuard::enter_counting();
        rt.process_planar(&mut l, &mut r);
        drop(g);
        for &v in l.iter().chain(r.iter()) {
            finite &= v.is_finite();
            peak = peak.max(v.abs());
        }
    }
    assert_eq!(rt_events(), before, "the audio path allocated or freed");
    assert!(finite);
    assert!(peak > 0.05, "the load project is silent (peak {peak})");
    assert!(peak <= 1.0, "master limiter exceeded: {peak}");
}
