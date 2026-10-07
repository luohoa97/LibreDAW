// SPDX-License-Identifier: GPL-3.0-or-later
//! The live engine on a real device. Ignored in CI (no audio device there):
//! `cargo test -p libredaw-engine --test live -- --ignored --nocapture`.

mod common;

use common::*;
use engine::{Engine, EngineConfig, Host, Slots, compile, write_controls};
use protocol::engine::EngineCommand;
use std::sync::atomic::Ordering::Relaxed;
use std::time::Duration;

#[test]
#[ignore = "needs a PipeWire device"]
fn pipewire_engine_plays_a_pattern_at_real_time_priority() {
    let p = project(
        120.0,
        vec![],
        vec![synth_channel(1, 0, tone_params())],
        vec![pattern(
            1,
            16,
            &[(1, vec![(1, 0, 960, 69, 100), (2, 960, 960, 72, 100)])],
        )],
    );
    let mut slots = Slots::new();
    slots.sync(&p).unwrap();
    let cfg = EngineConfig {
        host: Host::PipeWire,
        device: None,
        buffer_frames: 256,
        sample_rate: Some(48000),
    };
    assert!(!Engine::devices(Host::PipeWire).is_empty());
    let mut e = Engine::start(&cfg, compile(&p, &slots, 48000.0)).unwrap();
    write_controls(&p, &slots, &e.controls, &e.params);
    e.command(EngineCommand::SetPlayingPattern {
        pattern: p.patterns[0].id,
    })
    .unwrap();
    e.command(EngineCommand::Play).unwrap();
    // submit a recompile while playing
    let mut c = Some(compile(&p, &slots, 48000.0));
    std::thread::sleep(Duration::from_millis(300));
    while let Some(b) = c.take() {
        if let Err(b) = e.submit(b) {
            c = Some(b);
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    std::thread::sleep(Duration::from_millis(1700));
    let tick = e.status.playhead_tick.load(Relaxed);
    let sched = e.callback_sched();
    println!(
        "playhead_tick={tick} xruns={} sched={sched:?} overflows={}",
        e.status.xruns.load(Relaxed),
        e.status.event_overflows.load(Relaxed)
    );
    assert!(e.status.playing.load(Relaxed));
    assert!(tick > 1000, "{tick}");
    assert!(!e.needs_restart());
    e.command(EngineCommand::Stop).unwrap();
    std::thread::sleep(Duration::from_millis(100));
    let mut stopped = false;
    e.drain_events(|ev| stopped |= matches!(ev, protocol::engine::EngineEvent::Stopped { .. }));
    assert!(stopped);
    e.stop();
}
