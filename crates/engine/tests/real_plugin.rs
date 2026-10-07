// SPDX-License-Identifier: GPL-3.0-or-later
//! The engine driving the real `plugin_host` fixture plugins (test.sine,
//! test.gain from `crates/plugin-host/examples/test_plugins.rs`). Skipped
//! with a message when the fixture has not been built (`cargo test
//! --workspace` builds it).

mod common;

use common::*;
use plugin_host::host::{Instance, PluginDesc, scan_paths};
use protocol::engine::{ChannelSlot, EngineCommand, PluginEvent, PluginSlot, TrackSlot};
use protocol::ids::InstanceId;
use protocol::model::{ClapRef, Insert, Instrument};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32};

fn fixture() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let so = exe.parent()?.parent()?.join("examples/libtest_plugins.so");
    if !so.exists() {
        eprintln!("SKIPPED: {so:?} not built (cargo build -p libredaw-plugin-host --examples)");
        return None;
    }
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("engine-clap-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("nested")).ok()?;
    std::fs::copy(&so, dir.join("nested/test_plugins.clap")).ok()?;
    Some(dir)
}

fn desc(dir: &PathBuf, id: &str) -> PluginDesc {
    scan_paths(std::slice::from_ref(dir))
        .into_iter()
        .find(|d| d.id == id)
        .unwrap_or_else(|| panic!("no fixture plugin {id}"))
}

fn clap_ref(id: &str) -> ClapRef {
    ClapRef {
        instance: InstanceId(90),
        plugin_id: id.into(),
        plugin_version: "1".into(),
        state_file: None,
        state_bytes: None,
        params: vec![],
    }
}

#[test]
fn sine_instrument_renders_offline_through_the_real_host() {
    let Some(dir) = fixture() else { return };
    let mut inst = Instance::create(&desc(&dir, "test.sine")).unwrap();
    inst.activate(48000.0, 512).unwrap();

    let mut ch = synth_channel(1, 0, tone_params());
    ch.instrument = Instrument::Clap(clap_ref("test.sine"));
    let p = project(
        120.0,
        vec![],
        vec![ch],
        vec![pattern(1, 16, &[(1, vec![(1, 960, 960, 69, 127)])])],
    );
    let mut slots = engine::Slots::new();
    slots.sync(&p).unwrap();
    let req = engine::RenderRequest {
        project: Arc::new(p),
        pattern: protocol::ids::PatternId(1),
        loops: 1,
        tail_seconds: 0.1,
        sample_rate: 48000,
    };
    let slot = PluginSlot::Instrument(ChannelSlot(0));
    let out = engine::render_offline(
        &req,
        &slots,
        &[(slot, inst.handle())],
        &AtomicU32::new(0),
        &AtomicBool::new(false),
    )
    .unwrap();
    let on = ideal_sample(960, 48000, 120, 1) as usize;
    let before = out[..on]
        .iter()
        .flatten()
        .fold(0.0f32, |m, v| m.max(v.abs()));
    let during = out[on..on + 4000]
        .iter()
        .flatten()
        .fold(0.0f32, |m, v| m.max(v.abs()));
    assert_eq!(before, 0.0);
    assert!(during > 0.05, "the plugin sounded: {during}");
    inst.deactivate();
}

#[test]
fn gain_insert_scales_the_track_and_follows_host_parameter_events() {
    let Some(dir) = fixture() else { return };
    let mut gain = Instance::create(&desc(&dir, "test.gain")).unwrap();
    gain.activate(48000.0, 256).unwrap();

    let mut t = track(1);
    t.inserts.push(Insert::Clap(clap_ref("test.gain")));
    let p = project(
        120.0,
        vec![t],
        vec![synth_channel(1, 1, tone_params())],
        vec![pattern(1, 16, &[(1, vec![(1, 0, 3840, 69, 127)])])],
    );
    let mut dry = rig(&p, 48000.0, true);
    dry.run(2400, 256);
    let (dl, _) = dry.run(2400, 256);

    let mut r = rig(&p, 48000.0, false);
    let slot = PluginSlot::Insert {
        track: TrackSlot(1),
        index: 0,
    };
    assert!(
        r.ui.commands
            .push(EngineCommand::AttachPlugin {
                slot,
                handle: gain.handle()
            })
            .is_ok()
    );
    assert!(r.ui.commands.push(EngineCommand::Play).is_ok());
    // The fixture checks thread-check: the audio side must be another thread.
    std::thread::scope(|s| {
        s.spawn(|| {
            r.run(2400, 256);
            let (a, _) = r.run(2400, 256);
            assert!(
                (peak(&a) / peak(&dl) - 1.0).abs() < 0.01,
                "unity by default {} {}",
                peak(&a),
                peak(&dl)
            );
            assert!(
                r.ui.plugin_events
                    .push(PluginEvent {
                        slot,
                        param_id: 0,
                        value: 0.5
                    })
                    .is_ok()
            );
            r.run(256, 256);
            let (b, _) = r.run(2400, 256);
            assert!(
                (peak(&b) / peak(&dl) - 0.5).abs() < 0.01,
                "{} {}",
                peak(&b),
                peak(&dl)
            );
            // detach, then the instance may be torn down
            assert!(
                r.ui.commands
                    .push(EngineCommand::DetachPlugin { slot })
                    .is_ok()
            );
            r.run(256, 256);
            let mut acked = false;
            while let Ok(e) = r.ui.events.pop() {
                acked |= e == protocol::engine::EngineEvent::DetachAck { slot };
            }
            assert!(acked);
        })
        .join()
        .unwrap();
    });
    gain.deactivate();
}
