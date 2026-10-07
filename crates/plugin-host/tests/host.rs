// SPDX-License-Identifier: GPL-3.0-or-later
//! Integration tests against the fixture plugins in `examples/test_plugins.rs`.

use glib::MainContext;
use plugin_host::host::{HostError, Instance, PluginDesc, scan_paths};
use plugin_host::rt::*;
use protocol::consts::MAX_BLOCK;
use protocol::engine::{PluginEvent, PluginHandle, PluginSlot};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

// Counts heap traffic on threads that opted in (the idea of
// crates/engine/src/rt.rs, without the abort).
thread_local! {
    static COUNTING: Cell<bool> = const { Cell::new(false) };
    static EVENTS: Cell<u64> = const { Cell::new(0) };
}

struct CountingAlloc;

fn note() {
    if COUNTING.try_with(Cell::get).unwrap_or(false) {
        let _ = EVENTS.try_with(|e| e.set(e.get() + 1));
    }
}

unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        note();
        unsafe { System.alloc(l) }
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        note();
        unsafe { System.alloc_zeroed(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        note();
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        note();
        unsafe { System.realloc(p, l, n) }
    }
}

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

/// Run `f` and return how many allocations and frees it made on this thread.
fn count_heap<R>(f: impl FnOnce() -> R) -> (R, u64) {
    EVENTS.with(|e| e.set(0));
    COUNTING.with(|c| c.set(true));
    let r = f();
    COUNTING.with(|c| c.set(false));
    (r, EVENTS.with(Cell::get))
}

fn fixture_dir() -> &'static PathBuf {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let exe = std::env::current_exe().unwrap();
        let so = exe
            .parent()
            .and_then(|p| p.parent())
            .unwrap()
            .join("examples/libtest_plugins.so");
        assert!(
            so.exists(),
            "build the fixture first: cargo build --examples ({so:?})"
        );
        let dir =
            PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("clap-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("nested")).unwrap();
        std::fs::copy(&so, dir.join("nested/test_plugins.clap")).unwrap();
        dir
    })
}

fn desc(id: &str) -> PluginDesc {
    scan_paths(std::slice::from_ref(fixture_dir()))
        .into_iter()
        .find(|d| d.id == id)
        .unwrap_or_else(|| panic!("no fixture plugin {id}"))
}

fn ev(param_id: u32, value: f64) -> PluginEvent {
    PluginEvent {
        slot: PluginSlot::Instrument(protocol::engine::ChannelSlot(0)),
        param_id,
        value,
    }
}

#[test]
fn scan_lists_fixture_plugins_without_instantiating() {
    let all = scan_paths(std::slice::from_ref(fixture_dir()));
    let ids: Vec<&str> = all.iter().map(|d| d.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "test.gain",
            "test.sine",
            "test.timer",
            "test.bad-layout",
            "test.fail-activate"
        ]
    );
    let sine = &all[1];
    assert!(sine.instrument && !sine.effect);
    assert_eq!(sine.name, "Test Sine");
    assert_eq!(sine.vendor, "LibreDAW tests");
    assert!(all[0].effect && !all[0].instrument);
}

#[test]
fn scan_ignores_missing_and_empty_paths() {
    let none = scan_paths(&[
        PathBuf::from("/nonexistent/clap"),
        PathBuf::from(env!("CARGO_TARGET_TMPDIR")),
    ]);
    assert!(none.iter().all(|d| d.id.starts_with("test.")) || none.is_empty());
}

#[test]
fn bad_files_are_skipped_by_scan_and_rejected_by_create() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("junk");
    std::fs::create_dir_all(&dir).unwrap();
    let junk = dir.join("junk.clap");
    std::fs::write(&junk, b"not an elf").unwrap();
    assert!(scan_paths(&[dir]).is_empty());
    let d = PluginDesc {
        id: "x".into(),
        name: "x".into(),
        vendor: String::new(),
        version: String::new(),
        path: junk,
        instrument: false,
        effect: true,
    };
    assert!(matches!(Instance::create(&d), Err(HostError::Load(_))));
}

#[test]
fn unknown_plugin_id_is_not_found() {
    let mut d = desc("test.gain");
    d.id = "nope".into();
    assert!(matches!(Instance::create(&d), Err(HostError::NotFound(_))));
}

#[test]
fn wrong_port_layout_is_rejected_with_clear_error() {
    let e = Instance::create(&desc("test.bad-layout"))
        .err()
        .expect("must fail");
    assert!(matches!(e, HostError::PortLayout(_)), "{e:?}");
    assert!(e.to_string().contains("port has 1 channels"), "{e}");
}

#[test]
fn failing_activate_is_an_error_and_instance_still_drops() {
    let mut i = Instance::create(&desc("test.fail-activate")).unwrap();
    assert!(matches!(i.activate(48000.0, 256), Err(HostError::Activate)));
    assert!(!i.is_active());
}

#[test]
fn params_state_and_flush_on_main_thread() {
    let mut i = Instance::create(&desc("test.gain")).unwrap();
    let ps = i.params();
    assert_eq!(ps.len(), 1);
    assert_eq!((ps[0].id, ps[0].name.as_str()), (0, "Gain"));
    assert_eq!((ps[0].min, ps[0].max, ps[0].default), (0.0, 2.0, 1.0));
    assert!(ps[0].automatable() && !ps[0].stepped());
    assert_eq!(i.param_value(0), Some(1.0));
    assert_eq!(i.param_value(7), None);
    i.flush_params(&[ev(0, 0.25)]);
    assert_eq!(i.param_value(0), Some(0.25));
    // flush echoed the value as a plugin-originated change
    let ch = i.take_param_changes();
    assert_eq!(ch.len(), 1);
    assert_eq!((ch[0].param_id, ch[0].value), (0, 0.25));
    assert_eq!(i.latency(), 32);

    let bytes = i.save_state().unwrap();
    assert_eq!(bytes.len(), 8);
    let mut j = Instance::create(&desc("test.gain")).unwrap();
    assert_eq!(j.param_value(0), Some(1.0));
    j.load_state(&bytes).unwrap();
    assert_eq!(j.param_value(0), Some(0.25));
    assert!(matches!(j.load_state(&[1, 2]), Err(HostError::State(_))));
}

#[test]
fn sine_state_roundtrip() {
    let mut a = Instance::create(&desc("test.sine")).unwrap();
    a.flush_params(&[ev(0, 0.8)]);
    let s = a.save_state().unwrap();
    let mut b = Instance::create(&desc("test.sine")).unwrap();
    assert_eq!(b.param_value(0), Some(0.5));
    b.load_state(&s).unwrap();
    assert_eq!(b.param_value(0), Some(0.8));
}

/// Runs `f` on a fresh thread, the way the engine's audio thread uses a handle.
fn on_audio_thread<R: Send>(h: PluginHandle, f: impl FnOnce(PluginHandle) -> R + Send) -> R {
    struct S(PluginHandle);
    // SAFETY: the handle is used by exactly one thread, as the engine does.
    unsafe impl Send for S {}
    let s = S(h);
    std::thread::scope(|sc| {
        sc.spawn(move || {
            let s = s;
            f(s.0)
        })
        .join()
        .unwrap()
    })
}

#[test]
fn gain_processes_a_block_with_param_events_and_no_allocation() {
    let mut inst = Instance::create(&desc("test.gain")).unwrap();
    inst.activate(48000.0, 256).unwrap();
    let h = inst.handle();
    let n = 128usize;
    let input: Vec<f32> = (0..n).map(|k| k as f32 / n as f32).collect();
    let (mut l, mut r) = (vec![9.0f32; n], vec![9.0f32; n]);
    let mut sink_buf = [RtOutEvent::EMPTY; 16];
    let params = [ev(0, 0.5)];

    let ((started, status, out_events), allocs) = on_audio_thread(h, |h| {
        count_heap(|| {
            // SAFETY: h is a live, activated instance used by this thread only.
            let started = unsafe { start_processing(h) };
            let mut sink = RtEventSink::new(&mut sink_buf);
            let mut block = RtBlock {
                frames: n as u32,
                steady_time: 0,
                inputs: [&input, &input],
                outputs: [&mut l, &mut r],
                notes: &[],
                params: &params,
            };
            let status = unsafe { process(h, &mut block, &mut sink) };
            let ev = (
                sink.as_slice().len(),
                sink.as_slice().first().copied(),
                sink.as_slice().get(1).copied(),
            );
            unsafe { stop_processing(h) };
            (started, status, ev)
        })
    });
    assert!(started);
    assert_eq!(status, RtStatus::Ok);
    assert_eq!(allocs, 0, "heap traffic on the audio thread");
    for k in 0..n {
        assert!((l[k] - input[k] * 0.5).abs() < 1e-6, "frame {k}");
        assert_eq!(l[k], r[k]);
    }
    // gesture begin, value, gesture end
    assert_eq!(out_events.0, 3);
    assert_eq!(out_events.1.unwrap().kind, RtOutKind::GestureBegin);
    let v = out_events.2.unwrap();
    assert_eq!(
        (v.kind, v.param_id, v.value),
        (RtOutKind::ParamValue, 0, 0.5)
    );
    assert_eq!(inst.param_value(0), Some(0.5));
}

#[test]
fn process_requires_start_and_valid_sizes() {
    let mut inst = Instance::create(&desc("test.gain")).unwrap();
    // not activated: start refuses
    let h = inst.handle();
    assert!(!unsafe { start_processing(h) });
    inst.activate(44100.0, 256).unwrap();
    let z = vec![0.0f32; MAX_BLOCK * 2];
    let (mut l, mut r) = (vec![0.0f32; MAX_BLOCK * 2], vec![0.0f32; MAX_BLOCK * 2]);
    let mut buf = [RtOutEvent::EMPTY; 4];
    let mut sink = RtEventSink::new(&mut buf);
    fn mk<'a>(frames: u32, z: &'a [f32], l: &'a mut [f32], r: &'a mut [f32]) -> RtBlock<'a> {
        RtBlock {
            frames,
            steady_time: 0,
            inputs: [z, z],
            outputs: [l, r],
            notes: &[],
            params: &[],
        }
    }
    assert_eq!(
        unsafe { process(h, &mut mk(64, &z, &mut l, &mut r), &mut sink) },
        RtStatus::Error
    );
    on_audio_thread(h, |h| {
        assert!(unsafe { start_processing(h) });
    });
    let big = MAX_BLOCK as u32 + 1;
    assert_eq!(
        unsafe { process(h, &mut mk(big, &z, &mut l, &mut r), &mut sink) },
        RtStatus::Error
    );
    unsafe { stop_processing(h) };
    inst.deactivate();
}

#[test]
fn sine_instrument_sounds_for_a_note_and_stops_on_note_off() {
    let mut inst = Instance::create(&desc("test.sine")).unwrap();
    inst.activate(44100.0, 256).unwrap();
    let h = inst.handle();
    let n = 256usize;
    let z = vec![0.0f32; n];
    let (mut l, mut r) = (vec![0.0f32; n], vec![0.0f32; n]);
    let (mut l2, mut r2) = (vec![1.0f32; n], vec![1.0f32; n]);
    let mut buf = [RtOutEvent::EMPTY; 4];
    let on = [RtNote {
        frame: 10,
        key: 69,
        vel: 127,
        on: true,
        note_id: 7,
    }];
    let off = [RtNote {
        frame: 100,
        key: 69,
        vel: 0,
        on: false,
        note_id: 7,
    }];
    let ((), allocs) = on_audio_thread(h, |h| {
        count_heap(|| {
            let mut sink = RtEventSink::new(&mut buf);
            assert!(unsafe { start_processing(h) });
            let mut b1 = RtBlock {
                frames: n as u32,
                steady_time: 0,
                inputs: [&z, &z],
                outputs: [&mut l, &mut r],
                notes: &on,
                params: &[],
            };
            assert_eq!(unsafe { process(h, &mut b1, &mut sink) }, RtStatus::Ok);
            let mut b2 = RtBlock {
                frames: n as u32,
                steady_time: n as u64,
                inputs: [&z, &z],
                outputs: [&mut l2, &mut r2],
                notes: &off,
                params: &[],
            };
            assert_eq!(unsafe { process(h, &mut b2, &mut sink) }, RtStatus::Ok);
            unsafe { stop_processing(h) };
        })
    });
    assert_eq!(allocs, 0);
    assert!(l[..10].iter().all(|v| *v == 0.0), "silent before the note");
    let peak = l[10..].iter().fold(0f32, |m, v| m.max(v.abs()));
    assert!(
        (peak - 0.5).abs() < 0.01,
        "volume 0.5 at velocity 1.0, got {peak}"
    );
    let crossings = l[10..]
        .windows(2)
        .filter(|w| w[0] <= 0.0 && w[1] > 0.0)
        .count();
    assert!(
        (2..=3).contains(&crossings),
        "A4 over 246 frames, got {crossings}"
    );
    assert_eq!(l, r);
    assert!(l2[..100].iter().any(|v| v.abs() > 0.1));
    assert!(l2[100..].iter().all(|v| *v == 0.0), "silent after note-off");
}

#[test]
fn no_gui_is_reported_and_hide_is_harmless() {
    let mut i = Instance::create(&desc("test.gain")).unwrap();
    assert!(matches!(
        i.show_gui("Track 1 - Test Gain"),
        Err(HostError::NoGui)
    ));
    assert!(!i.gui_open());
    i.hide_gui();
    i.poll_main_thread();
    assert!(!i.gui_open());
}

fn le(b: &[u8], k: usize) -> u32 {
    u32::from_le_bytes(b[k * 4..k * 4 + 4].try_into().unwrap())
}

#[test]
fn timer_fd_and_main_thread_callbacks_run_on_glib_sources() {
    let ctx = MainContext::new();
    ctx.with_thread_default(|| {
        let mut i = Instance::create(&desc("test.timer")).unwrap();
        i.activate(44100.0, 256).unwrap(); // plugin calls request_callback
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            while ctx.iteration(false) {}
            i.poll_main_thread();
            let s = i.save_state().unwrap();
            if le(&s, 0) >= 3 && le(&s, 1) == 1 && le(&s, 2) == 1 {
                break;
            }
            assert!(Instant::now() < deadline, "callbacks did not arrive: {s:?}");
            std::thread::sleep(Duration::from_millis(2));
        }
        // No more request_callback: the flag was consumed exactly once.
        i.poll_main_thread();
        assert_eq!(le(&i.save_state().unwrap(), 2), 1);
        // Destroy the plugin, then keep iterating: removed sources must not
        // call into freed memory.
        drop(i);
        let until = Instant::now() + Duration::from_millis(40);
        while Instant::now() < until {
            ctx.iteration(false);
            std::thread::sleep(Duration::from_millis(2));
        }
    })
    .unwrap();
}
