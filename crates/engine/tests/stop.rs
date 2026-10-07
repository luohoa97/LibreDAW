// SPDX-License-Identifier: GPL-3.0-or-later
//! Stop semantics (20.2): the playhead returns to where the run started
//! (or the loop start inside a loop region); a second stop goes to 0.

mod common;

use common::*;
use protocol::engine::EngineCommand;
use protocol::model::LoopRegion;

const SR: f64 = 48000.0;

fn proj() -> protocol::model::Project {
    project(
        120.0,
        vec![],
        vec![synth_channel(1, 0, tone_params())],
        vec![pattern(1, 16, &[(1, vec![(1, 0, 240, 60, 100)])])],
    )
}

fn playhead(r: &Rig) -> u64 {
    r.shared
        .status
        .playhead_tick
        .load(std::sync::atomic::Ordering::Relaxed)
}

#[test]
fn stop_returns_to_the_run_start_then_to_zero() {
    let mut p = proj();
    p.loop_region.enabled = false;
    let mut r = rig(&p, SR, false);
    r.rt.command(EngineCommand::Seek { tick: 1000 });
    r.rt.command(EngineCommand::Play);
    r.run(500 * 25, 256);
    assert!(playhead(&r) >= 1500);
    r.rt.command(EngineCommand::Stop);
    r.run(256, 256);
    assert_eq!(playhead(&r), 1000);
    r.rt.command(EngineCommand::Stop);
    r.run(256, 256);
    assert_eq!(playhead(&r), 0);
}

#[test]
fn inside_a_loop_region_stop_rests_at_the_loop_start() {
    let mut p = proj();
    p.loop_region = LoopRegion {
        start: 960,
        end: 3840,
        enabled: true,
    };
    let mut r = rig(&p, SR, false);
    r.rt.command(EngineCommand::Seek { tick: 1200 });
    r.rt.command(EngineCommand::Play);
    r.run(1000 * 25, 256);
    r.rt.command(EngineCommand::Stop);
    r.run(256, 256);
    assert_eq!(playhead(&r), 960);
}
