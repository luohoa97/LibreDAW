// SPDX-License-Identifier: GPL-3.0-or-later
//! Note preview (audition): sound while stopped, mixing with playback,
//! auto-release timing, mixer routing, plugin note events.

mod common;

use common::*;
use engine::PluginApi;
use engine::plugins::{OutEvents, ProcessArgs};
use engine::preview::PREVIEW_ID_BASE;
use engine::rt::{RtGuard, rt_events};
use protocol::engine::{
    ChannelSlot, EngineCommand, MixControl, PREVIEW_MAX_SECONDS, PluginHandle, PluginSlot,
    channel_control,
};
use protocol::ids::InstanceId;
use protocol::model::{ClapRef, Instrument};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering::Relaxed};

const SR: f64 = 48000.0;
const C0: ChannelSlot = ChannelSlot(0);

fn on(key: u8) -> EngineCommand {
    EngineCommand::Preview {
        channel: C0,
        key,
        vel: 100,
        on: true,
    }
}

fn off(key: u8) -> EngineCommand {
    EngineCommand::Preview {
        channel: C0,
        key,
        vel: 0,
        on: false,
    }
}

fn send(r: &mut Rig, c: EngineCommand) {
    assert!(r.ui.commands.push(c).is_ok());
}

fn one_channel(notes: Vec<N>) -> protocol::model::Project {
    let pats = if notes.is_empty() {
        vec![pattern(1, 16, &[])]
    } else {
        vec![pattern(1, 16, &[(1, notes)])]
    };
    project(
        120.0,
        vec![],
        vec![synth_channel(1, 0, tone_params())],
        pats,
    )
}

#[test]
fn preview_while_stopped_sounds_then_goes_silent_after_release() {
    let mut r = rig(&one_channel(vec![]), SR, false);
    let (l, _) = r.run(1024, 256);
    assert_eq!(peak(&l), 0.0);
    send(&mut r, on(69));
    let (l, rr) = r.run(2400, 256);
    assert!(!r.rt.is_playing());
    assert!(peak(&l[256..]) > 0.05, "{}", peak(&l));
    assert!(peak(&rr[256..]) > 0.05);
    send(&mut r, off(69));
    r.run(1024, 256); // 5 ms release has finished
    let (l, rr) = r.run(1024, 256);
    assert_eq!(peak(&l), 0.0);
    assert_eq!(peak(&rr), 0.0);
    assert_eq!(r.rt.active_voices(C0), 0);
}

#[test]
fn preview_mixes_with_sequencer_notes_without_cutting_them() {
    // one long sequencer note, key 64, from tick 0
    let mut r = rig(&one_channel(vec![(1, 0, 3840, 64, 100)]), SR, true);
    let (alone, _) = r.run(4800, 256);
    let alone_peak = peak(&alone[2400..]);
    send(&mut r, on(72));
    let (both, _) = r.run(4800, 256);
    assert!(
        peak(&both[1000..]) > alone_peak * 1.2,
        "two tones sum: {} vs {alone_peak}",
        peak(&both[1000..])
    );
    send(&mut r, off(72));
    r.run(1024, 256);
    let (after, _) = r.run(2400, 256);
    assert!(peak(&after) > 0.05, "sequencer note still sounds");
    assert_eq!(r.rt.active_voices(C0), 1);
    assert!(r.rt.is_playing());
    // the sequencer's own note is untouched in the owner table
    assert_eq!(r.rt.live_notes(C0), 1u128 << 64);
    let tr = r.rt.trace();
    let seq_off = tr
        .iter()
        .filter(|e| !e.on && e.id < PREVIEW_ID_BASE)
        .count();
    assert_eq!(seq_off, 0, "no sequencer note was released");
    let prev: Vec<_> = tr.iter().filter(|e| e.id >= PREVIEW_ID_BASE).collect();
    assert_eq!(prev.len(), 2);
    assert!(prev[0].on && !prev[1].on && prev[0].id == prev[1].id);
}

#[test]
fn preview_auto_releases_after_the_limit_to_the_sample() {
    let mut r = rig(&one_channel(vec![]), SR, false);
    send(&mut r, on(60));
    let limit = (PREVIEW_MAX_SECONDS * SR) as usize;
    r.run(limit + 4000, 300); // 300 does not divide the limit
    let ev: Vec<_> = r.rt.trace().iter().collect();
    assert_eq!(ev.len(), 2);
    assert!(ev[0].on && ev[0].sample == 0);
    assert!(!ev[1].on);
    assert_eq!(ev[1].sample, limit as u64);
    assert_eq!(ev[0].id, ev[1].id);
    // and it is silent afterwards
    let (l, _) = r.run(1024, 256);
    assert_eq!(peak(&l), 0.0);
}

#[test]
fn stop_releases_previews() {
    let mut r = rig(&one_channel(vec![]), SR, false);
    send(&mut r, on(60));
    r.run(1024, 256);
    send(&mut r, EngineCommand::Stop);
    r.run(1024, 256);
    let offs = r.rt.trace().iter().filter(|e| !e.on).count();
    assert_eq!(offs, 1);
    let (l, _) = r.run(1024, 256);
    assert_eq!(peak(&l), 0.0);
    // the auto-release does not fire a second note-off
    r.run((PREVIEW_MAX_SECONDS * SR) as usize, 256);
    assert_eq!(r.rt.trace().iter().filter(|e| !e.on).count(), 1);
}

#[test]
fn preview_ignores_solo_elsewhere_but_not_mute() {
    let p = project(
        120.0,
        vec![],
        vec![
            synth_channel(1, 0, tone_params()),
            synth_channel(2, 0, tone_params()),
        ],
        vec![pattern(1, 16, &[])],
    );
    let mut r = rig(&p, SR, false);
    let solo = channel_control(ChannelSlot(1), MixControl::Solo);
    r.shared.controls.set(solo, 1.0);
    send(&mut r, on(69));
    let (l, _) = r.run(2400, 256);
    assert!(peak(&l[256..]) > 0.05, "audible although channel 1 solos");
    send(&mut r, off(69));
    r.run(2400, 256);
    // mute wins
    r.shared
        .controls
        .set(channel_control(C0, MixControl::Mute), 1.0);
    send(&mut r, on(69));
    r.run(1024, 256);
    let (l, _) = r.run(2400, 256);
    assert_eq!(peak(&l), 0.0);
}

#[test]
fn preview_follows_channel_volume_and_pan() {
    let mut r = rig(&one_channel(vec![]), SR, false);
    send(&mut r, on(69));
    let (l0, r0) = r.run(2400, 256);
    let (p_l, p_r) = (peak(&l0[512..]), peak(&r0[512..]));
    assert!((p_l - p_r).abs() < 1e-3);
    send(&mut r, off(69));
    r.run(2400, 256);
    r.shared
        .controls
        .set(channel_control(C0, MixControl::Pan), -1.0);
    r.shared
        .controls
        .set(channel_control(C0, MixControl::VolumeDb), -6.0);
    send(&mut r, on(69));
    r.run(1024, 256); // ramps settle
    let (l, rr) = r.run(2400, 256);
    assert!(peak(&rr) < 1e-3, "hard left: right is silent");
    assert!(peak(&l) > 0.05 && peak(&l) < p_l * 1.2);
}

#[test]
fn off_without_on_and_unknown_slot_are_ignored() {
    let mut r = rig(&one_channel(vec![]), SR, false);
    send(&mut r, off(60));
    send(
        &mut r,
        EngineCommand::Preview {
            channel: ChannelSlot(63),
            key: 60,
            vel: 90,
            on: true,
        },
    );
    let (l, _) = r.run(1024, 256);
    assert_eq!(peak(&l), 0.0);
}

// ---- CLAP instrument path (fake plugin recording note events) ----

static SERIAL: Mutex<()> = Mutex::new(());
static ONS: AtomicU32 = AtomicU32::new(0);
static OFFS: AtomicU32 = AtomicU32::new(0);
static LAST_ID: AtomicU32 = AtomicU32::new(0);
static LAST_KEY: AtomicU32 = AtomicU32::new(0);

unsafe fn f_start(_h: PluginHandle) -> bool {
    true
}
unsafe fn f_stop(_h: PluginHandle) {}
unsafe fn f_process(_h: PluginHandle, a: &mut ProcessArgs, out: &mut OutEvents) -> bool {
    for n in a.notes {
        if n.on {
            ONS.fetch_add(1, Relaxed);
        } else {
            OFFS.fetch_add(1, Relaxed);
        }
        LAST_ID.store(n.note_id, Relaxed);
        LAST_KEY.store(n.key as u32, Relaxed);
    }
    let on = ONS.load(Relaxed) > OFFS.load(Relaxed);
    a.out_l[..a.frames].fill(if on { 0.25 } else { 0.0 });
    a.out_r[..a.frames].fill(if on { 0.25 } else { 0.0 });
    out.len = 0;
    out.dropped = 0;
    true
}

fn clap_rig() -> Rig {
    let mut ch = synth_channel(1, 0, tone_params());
    ch.instrument = Instrument::Clap(ClapRef {
        instance: InstanceId(50),
        plugin_id: "test.sine".into(),
        plugin_version: "1".into(),
        state_file: None,
        state_bytes: None,
        params: vec![],
    });
    let p = project(120.0, vec![], vec![ch], vec![pattern(1, 16, &[])]);
    let mut r = rig(&p, SR, false);
    r.rt.set_plugin_api(PluginApi {
        start: f_start,
        stop: f_stop,
        process: f_process,
    });
    send(
        &mut r,
        EngineCommand::AttachPlugin {
            slot: PluginSlot::Instrument(C0),
            handle: PluginHandle(std::ptr::dangling_mut::<core::ffi::c_void>()),
        },
    );
    r
}

#[test]
fn preview_sends_note_on_and_off_to_a_plugin_instrument() {
    let _g = SERIAL.lock().unwrap();
    for a in [&ONS, &OFFS, &LAST_ID, &LAST_KEY] {
        a.store(0, Relaxed);
    }
    let mut r = clap_rig();
    send(&mut r, on(67));
    let (l, _) = r.run(1024, 256);
    assert_eq!(ONS.load(Relaxed), 1);
    assert_eq!(OFFS.load(Relaxed), 0);
    assert_eq!(LAST_KEY.load(Relaxed), 67);
    assert!(LAST_ID.load(Relaxed) >= PREVIEW_ID_BASE);
    assert!((peak(&l) - 0.25).abs() < 0.01, "through the mixer path");
    send(&mut r, off(67));
    let (l, _) = r.run(1024, 256);
    assert_eq!(OFFS.load(Relaxed), 1);
    assert!(LAST_ID.load(Relaxed) >= PREVIEW_ID_BASE);
    assert_eq!(peak(&l[256..]), 0.0);
}

#[test]
fn preview_auto_release_reaches_a_plugin_instrument() {
    let _g = SERIAL.lock().unwrap();
    for a in [&ONS, &OFFS, &LAST_ID, &LAST_KEY] {
        a.store(0, Relaxed);
    }
    let mut r = clap_rig();
    send(&mut r, on(60));
    r.run((PREVIEW_MAX_SECONDS * SR) as usize + 2000, 256);
    assert_eq!(ONS.load(Relaxed), 1);
    assert_eq!(OFFS.load(Relaxed), 1);
}

#[test]
fn preview_paths_make_no_allocations() {
    let mut r = rig(&one_channel(vec![(1, 0, 960, 64, 100)]), SR, false);
    send(&mut r, EngineCommand::Play);
    let mut l = vec![0.0f32; 300];
    let mut rr = vec![0.0f32; 300];
    let before = rt_events();
    let blocks = (PREVIEW_MAX_SECONDS * SR) as usize / 300 + 40;
    for i in 0..blocks {
        match i {
            2 | 5 => send(&mut r, on(60 + i as u8)),
            4 => send(&mut r, off(62)),
            7 => send(&mut r, on(65)),
            9 => send(&mut r, on(65)), // retrigger
            11 => send(&mut r, EngineCommand::Stop),
            13 => send(&mut r, on(70)), // left to the auto-release
            _ => {}
        }
        let g = RtGuard::enter_counting();
        r.rt.process_planar(&mut l, &mut rr);
        drop(g);
    }
    assert_eq!(rt_events(), before);
    assert_eq!(r.rt.active_voices(C0), 0);
}
