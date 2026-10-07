// SPDX-License-Identifier: GPL-3.0-or-later
//! The plugin table on the audio thread (SPEC 9.1, 3.3), with a fake plugin
//! behind `PluginApi`, plus one test against the real fixture plugins.

mod common;

use common::*;
use engine::PluginApi;
use engine::plugins::{OutEvents, ProcessArgs};
use engine::rt::{RtGuard, rt_events};
use plugin_host::rt::{RtOutEvent, RtOutKind};
use protocol::consts::EVENT_RING_CAP;
use protocol::engine::{
    ChannelSlot, EngineCommand, EngineEvent, PluginEvent, PluginHandle, PluginSlot, TrackSlot,
};
use protocol::ids::InstanceId;
use protocol::model::{ClapRef, Insert, Instrument};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering::Relaxed};

static SERIAL: Mutex<()> = Mutex::new(());
static STARTS: AtomicU32 = AtomicU32::new(0);
static STOPS: AtomicU32 = AtomicU32::new(0);
static PROCS: AtomicU32 = AtomicU32::new(0);
static NOTE_ONS: AtomicU32 = AtomicU32::new(0);
static NOTE_FRAME_SUM: AtomicU32 = AtomicU32::new(0);
static PARAMS_SEEN: AtomicU32 = AtomicU32::new(0);
static LAST_PARAM_BITS: AtomicU64 = AtomicU64::new(0);
static EMIT: AtomicU32 = AtomicU32::new(0);
static START_OK: AtomicU32 = AtomicU32::new(1);

const INSTRUMENT: usize = 1;
const EFFECT: usize = 2;

fn reset() {
    for a in [
        &STARTS,
        &STOPS,
        &PROCS,
        &NOTE_ONS,
        &NOTE_FRAME_SUM,
        &PARAMS_SEEN,
        &EMIT,
    ] {
        a.store(0, Relaxed);
    }
    START_OK.store(1, Relaxed);
}

unsafe fn f_start(_h: PluginHandle) -> bool {
    STARTS.fetch_add(1, Relaxed);
    START_OK.load(Relaxed) == 1
}

unsafe fn f_stop(_h: PluginHandle) {
    STOPS.fetch_add(1, Relaxed);
}

unsafe fn f_process(h: PluginHandle, a: &mut ProcessArgs, out: &mut OutEvents) -> bool {
    PROCS.fetch_add(1, Relaxed);
    std::thread::sleep(std::time::Duration::from_micros(200));
    for n in a.notes.iter().filter(|n| n.on) {
        NOTE_ONS.fetch_add(1, Relaxed);
        NOTE_FRAME_SUM.fetch_add(n.frame, Relaxed);
    }
    for p in a.params {
        PARAMS_SEEN.fetch_add(1, Relaxed);
        LAST_PARAM_BITS.store(p.value.to_bits(), Relaxed);
    }
    let n = a.frames;
    if h.0 as usize == INSTRUMENT {
        // a steady 0.25 once any note has started
        let on = NOTE_ONS.load(Relaxed) > 0;
        a.out_l[..n].fill(if on { 0.25 } else { 0.0 });
        a.out_r[..n].fill(if on { 0.25 } else { 0.0 });
    } else {
        for i in 0..n {
            a.out_l[i] = a.in_l[i] * 0.5;
            a.out_r[i] = a.in_r[i] * 0.5;
        }
    }
    let emit = EMIT.load(Relaxed) as usize;
    out.len = emit.min(out.buf.len());
    out.dropped = emit.saturating_sub(out.buf.len()) as u32;
    for (i, e) in out.buf[..out.len].iter_mut().enumerate() {
        *e = RtOutEvent {
            kind: RtOutKind::ParamValue,
            param_id: i as u32,
            value: 0.5,
        };
    }
    true
}

fn fake_api() -> PluginApi {
    PluginApi {
        start: f_start,
        stop: f_stop,
        process: f_process,
    }
}

fn handle(kind: usize) -> PluginHandle {
    PluginHandle(kind as *mut core::ffi::c_void)
}

fn clap_ref() -> ClapRef {
    ClapRef {
        instance: InstanceId(50),
        plugin_id: "test.sine".into(),
        plugin_version: "1".into(),
        state_file: None,
        state_bytes: None,
        params: vec![],
    }
}

/// One Clap-instrument channel on track 0 playing a note at tick 0 and one
/// at tick 100.
fn instrument_project() -> protocol::model::Project {
    let mut ch = synth_channel(1, 0, tone_params());
    ch.instrument = Instrument::Clap(clap_ref());
    project(
        120.0,
        vec![],
        vec![ch],
        vec![pattern(
            1,
            16,
            &[(1, vec![(1, 0, 960, 60, 127), (2, 100, 400, 64, 127)])],
        )],
    )
}

fn plugin_rig(p: &protocol::model::Project) -> Rig {
    let mut r = rig(p, 48000.0, false);
    r.rt.set_plugin_api(fake_api());
    r
}

fn send(r: &mut Rig, c: EngineCommand) {
    assert!(r.ui.commands.push(c).is_ok());
}

fn events(r: &mut Rig) -> Vec<EngineEvent> {
    let mut v = Vec::new();
    while let Ok(e) = r.ui.events.pop() {
        v.push(e);
    }
    v
}

#[test]
fn instrument_plugin_gets_note_frames_and_its_audio_is_mixed() {
    let _g = SERIAL.lock().unwrap();
    reset();
    let mut r = plugin_rig(&instrument_project());
    let slot = PluginSlot::Instrument(ChannelSlot(0));
    send(
        &mut r,
        EngineCommand::AttachPlugin {
            slot,
            handle: handle(INSTRUMENT),
        },
    );
    send(&mut r, EngineCommand::Play);
    let (l, rr) = r.run(4800, 300);
    assert_eq!(
        STARTS.load(Relaxed),
        1,
        "start_processing on first use, once"
    );
    assert_eq!(NOTE_ONS.load(Relaxed), 2);
    // frame offsets are relative to the sub-block: note 2 at tick 100 =
    // 5000 samples... at 120 BPM, 48 kHz one tick is 25 samples: 2500
    let expected = r.rt.trace().iter().filter(|e| e.on).count();
    assert_eq!(expected, 2);
    assert!(NOTE_FRAME_SUM.load(Relaxed) < 2 * 256);
    // center: mono-free stereo balance is unity, so 0.25 reaches the master
    assert!((peak(&l) - 0.25).abs() < 0.01, "{}", peak(&l));
    assert!((peak(&rr) - 0.25).abs() < 0.01);
    // timing of the call is recorded for the slot
    let st = &r.shared.status;
    assert!(st.plugin_max_us[slot.index()].load(Relaxed) >= 100);
    assert!(st.plugin_last_us[slot.index()].load(Relaxed) >= 100);
}

#[test]
fn unattached_instrument_channel_is_silent() {
    let _g = SERIAL.lock().unwrap();
    reset();
    let mut r = plugin_rig(&instrument_project());
    send(&mut r, EngineCommand::Play);
    let (l, _) = r.run(4800, 256);
    assert_eq!(peak(&l), 0.0);
    assert_eq!(PROCS.load(Relaxed), 0);
}

#[test]
fn detach_stops_processing_clears_the_slot_and_acks() {
    let _g = SERIAL.lock().unwrap();
    reset();
    let mut r = plugin_rig(&instrument_project());
    let slot = PluginSlot::Instrument(ChannelSlot(0));
    send(
        &mut r,
        EngineCommand::AttachPlugin {
            slot,
            handle: handle(INSTRUMENT),
        },
    );
    send(&mut r, EngineCommand::Play);
    r.run(1024, 256);
    assert_eq!(STOPS.load(Relaxed), 0);
    send(&mut r, EngineCommand::DetachPlugin { slot });
    r.run(256, 256);
    assert_eq!(STOPS.load(Relaxed), 1);
    assert!(events(&mut r).contains(&EngineEvent::DetachAck { slot }));
    let procs = PROCS.load(Relaxed);
    r.run(1024, 256);
    assert_eq!(
        PROCS.load(Relaxed),
        procs,
        "a detached plugin is never called"
    );
}

#[test]
fn failed_start_processing_keeps_the_plugin_out_of_the_mix() {
    let _g = SERIAL.lock().unwrap();
    reset();
    START_OK.store(0, Relaxed);
    let mut r = plugin_rig(&instrument_project());
    let slot = PluginSlot::Instrument(ChannelSlot(0));
    send(
        &mut r,
        EngineCommand::AttachPlugin {
            slot,
            handle: handle(INSTRUMENT),
        },
    );
    send(&mut r, EngineCommand::Play);
    let (l, _) = r.run(2048, 256);
    assert_eq!(STARTS.load(Relaxed), 1, "not retried every block");
    assert_eq!(PROCS.load(Relaxed), 0);
    assert_eq!(peak(&l), 0.0);
    send(&mut r, EngineCommand::DetachPlugin { slot });
    r.run(64, 64);
    assert_eq!(STOPS.load(Relaxed), 0, "never started, so never stopped");
    assert!(events(&mut r).contains(&EngineEvent::DetachAck { slot }));
}

#[test]
fn insert_plugin_processes_the_track_bus() {
    let _g = SERIAL.lock().unwrap();
    reset();
    let mut ch = synth_channel(1, 1, tone_params());
    ch.mix.pan = 0.0;
    let mut t = track(1);
    t.inserts.push(Insert::Clap(clap_ref()));
    let p = project(
        120.0,
        vec![t],
        vec![ch],
        vec![pattern(1, 16, &[(1, vec![(1, 0, 3840, 69, 127)])])],
    );
    let mut dry = rig(&p, 48000.0, true);
    dry.run(2400, 256);
    let (dl, _) = dry.run(2400, 256);

    let mut r = plugin_rig(&p);
    let slot = PluginSlot::Insert {
        track: TrackSlot(1),
        index: 0,
    };
    send(
        &mut r,
        EngineCommand::AttachPlugin {
            slot,
            handle: handle(EFFECT),
        },
    );
    send(&mut r, EngineCommand::Play);
    r.run(2400, 256);
    let (wl, _) = r.run(2400, 256);
    assert!(
        (peak(&wl) / peak(&dl) - 0.5).abs() < 0.01,
        "{} {}",
        peak(&wl),
        peak(&dl)
    );
    assert!(PROCS.load(Relaxed) > 0);
}

#[test]
fn host_parameter_events_reach_the_plugin_and_plugin_events_come_back() {
    let _g = SERIAL.lock().unwrap();
    reset();
    let mut r = plugin_rig(&instrument_project());
    let slot = PluginSlot::Instrument(ChannelSlot(0));
    send(
        &mut r,
        EngineCommand::AttachPlugin {
            slot,
            handle: handle(INSTRUMENT),
        },
    );
    send(&mut r, EngineCommand::Play);
    r.run(256, 256);
    // an event for another slot is not delivered here
    let other = PluginSlot::Instrument(ChannelSlot(3));
    for (s, v) in [(slot, 0.75), (other, 0.1)] {
        assert!(
            r.ui.plugin_events
                .push(PluginEvent {
                    slot: s,
                    param_id: 7,
                    value: v
                })
                .is_ok()
        );
    }
    EMIT.store(2, Relaxed);
    r.run(256, 256);
    assert_eq!(PARAMS_SEEN.load(Relaxed), 1);
    assert_eq!(f64::from_bits(LAST_PARAM_BITS.load(Relaxed)), 0.75);
    let ev = events(&mut r);
    assert!(ev.contains(&EngineEvent::PluginParamChanged {
        slot,
        param_id: 1,
        value: 0.5
    }));
    assert_eq!(r.shared.status.event_overflows.load(Relaxed), 0);
}

#[test]
fn event_ring_overflow_is_counted() {
    let _g = SERIAL.lock().unwrap();
    reset();
    let mut r = plugin_rig(&instrument_project());
    let slot = PluginSlot::Instrument(ChannelSlot(0));
    send(
        &mut r,
        EngineCommand::AttachPlugin {
            slot,
            handle: handle(INSTRUMENT),
        },
    );
    send(&mut r, EngineCommand::Play);
    // 512 events per call (the sink cap) + 100 the sink drops, never drained
    EMIT.store(612, Relaxed);
    for _ in 0..4 {
        r.run(256, 256);
    }
    let overflows = r.shared.status.event_overflows.load(Relaxed);
    // 4 calls: 400 dropped by the sink, and 2048 - 1024 refused by the ring
    assert_eq!(overflows, 400 + (4 * 512 - EVENT_RING_CAP) as u64);
}

#[test]
fn detach_ack_survives_a_full_ring() {
    let _g = SERIAL.lock().unwrap();
    reset();
    let mut r = plugin_rig(&instrument_project());
    let slot = PluginSlot::Instrument(ChannelSlot(0));
    send(
        &mut r,
        EngineCommand::AttachPlugin {
            slot,
            handle: handle(INSTRUMENT),
        },
    );
    send(&mut r, EngineCommand::Play);
    EMIT.store(512, Relaxed);
    for _ in 0..3 {
        r.run(256, 256); // 1536 events into a 1024 ring
    }
    EMIT.store(0, Relaxed);
    send(&mut r, EngineCommand::DetachPlugin { slot });
    r.run(256, 256);
    assert_eq!(STOPS.load(Relaxed), 1);
    // the ring is still full of parameter events: no ack yet
    let mut seen = 0;
    let mut acks = 0;
    // drain a little at a time and keep running; the ack must appear
    for _ in 0..20 {
        for _ in 0..200 {
            match r.ui.events.pop() {
                Ok(EngineEvent::DetachAck { .. }) => acks += 1,
                Ok(_) => seen += 1,
                Err(_) => break,
            }
        }
        r.run(64, 64);
    }
    assert!(seen >= 1024 - 1);
    assert_eq!(acks, 1, "exactly one ack, delivered after room appeared");
}

#[test]
fn plugin_paths_make_no_allocations() {
    let _g = SERIAL.lock().unwrap();
    reset();
    let mut r = plugin_rig(&instrument_project());
    let slot = PluginSlot::Instrument(ChannelSlot(0));
    let _ = r.ui.commands.push(EngineCommand::AttachPlugin {
        slot,
        handle: handle(INSTRUMENT),
    });
    let _ = r.ui.commands.push(EngineCommand::Play);
    EMIT.store(3, Relaxed);
    let mut l = vec![0.0f32; 300];
    let mut rr = vec![0.0f32; 300];
    let before = rt_events();
    for i in 0..60 {
        if i == 30 {
            let _ = r.ui.plugin_events.push(PluginEvent {
                slot,
                param_id: 1,
                value: 0.3,
            });
            let _ = r.ui.commands.push(EngineCommand::DetachPlugin { slot });
        }
        let g = RtGuard::enter_counting();
        r.rt.process_planar(&mut l, &mut rr);
        drop(g);
        let _ = events(&mut r);
    }
    assert_eq!(rt_events(), before);
}
