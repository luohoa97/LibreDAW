// SPDX-License-Identifier: GPL-3.0-or-later
//! Smoke tests against the real libre CLAP instruments (Surge XT, Odin2, Dexed)
//! from the Flatpak LinuxAudio extensions. They need those installed, so they
//! are ignored by default: `cargo test -p libredaw-plugin-host --test
//! real_plugins -- --ignored --nocapture --test-threads=1`.

use plugin_host::host::{Instance, PluginDesc, clap_paths, scan_paths};
use plugin_host::rt::*;
use protocol::engine::PluginHandle;
use std::time::Instant;

const RATE: f64 = 48_000.0;
const BLOCK: usize = 256;
const BLOCKS: usize = 188; // 1.003 s
const C3: u8 = 48;

fn find(name: &str) -> PluginDesc {
    scan_paths(&clap_paths())
        .into_iter()
        .find(|d| d.instrument && d.name == name)
        .unwrap_or_else(|| panic!("{name} is not installed as a Flatpak extension"))
}

struct Render {
    left: Vec<f32>,
    right: Vec<f32>,
    /// Wall time of each process() call, in microseconds.
    micros: Vec<f64>,
}

/// Render one second after a C3 note-on, on a fresh thread like the engine's
/// audio thread.
fn render(h: PluginHandle) -> Render {
    struct S(PluginHandle);
    // SAFETY: one thread uses the handle, as the engine does.
    unsafe impl Send for S {}
    let s = S(h);
    std::thread::scope(|sc| {
        sc.spawn(move || {
            let h = s;
            let h = h.0;
            let z = vec![0.0f32; BLOCK];
            let mut out = Render {
                left: Vec::new(),
                right: Vec::new(),
                micros: Vec::new(),
            };
            let mut ev = [RtOutEvent::EMPTY; 64];
            let mut sink = RtEventSink::new(&mut ev);
            assert!(unsafe { start_processing(h) });
            for b in 0..BLOCKS {
                let on = [RtNote {
                    frame: 0,
                    key: C3,
                    vel: 100,
                    on: true,
                    note_id: 1,
                }];
                let (mut l, mut r) = (vec![0.0f32; BLOCK], vec![0.0f32; BLOCK]);
                let mut blk = RtBlock {
                    frames: BLOCK as u32,
                    steady_time: (b * BLOCK) as u64,
                    inputs: [&z, &z],
                    outputs: [&mut l, &mut r],
                    notes: if b == 0 { &on } else { &[] },
                    params: &[],
                };
                sink.clear();
                let t = Instant::now();
                let st = unsafe { process(h, &mut blk, &mut sink) };
                out.micros.push(t.elapsed().as_secs_f64() * 1e6);
                assert_eq!(st, RtStatus::Ok);
                out.left.extend_from_slice(&l);
                out.right.extend_from_slice(&r);
            }
            unsafe { stop_processing(h) };
            out
        })
        .join()
        .unwrap()
    })
}

fn peak(v: &[f32]) -> f32 {
    v.iter().fold(0.0, |m, x| m.max(x.abs()))
}

fn smoke(name: &str) {
    let d = find(name);
    let mut a = Instance::create(&d).unwrap();
    a.set_offline(true);
    a.activate(RATE, BLOCK as u32).unwrap();
    let state = a.save_state().unwrap();
    let ra = render(a.handle());
    let p = peak(&ra.left).max(peak(&ra.right));
    let nan = ra.left.iter().chain(&ra.right).any(|v| !v.is_finite());
    let mut m = ra.micros.clone();
    m.sort_by(|x, y| x.total_cmp(y));
    let mean = m.iter().sum::<f64>() / m.len() as f64;
    let budget = BLOCK as f64 / RATE * 1e6;
    println!(
        "{name}: peak {p:.4}, nan {nan}, state {} bytes, block budget {budget:.0} us, \
         cpu/block mean {mean:.1} us ({:.2}% of budget), p99 {:.1} us, max {:.1} us",
        state.len(),
        mean / budget * 100.0,
        m[m.len() * 99 / 100],
        m[m.len() - 1],
    );
    assert!(p > 0.01, "{name} is silent (peak {p})");
    assert!(!nan, "{name} produced NaN or inf");

    // Restore the saved state into a second instance and render again.
    let mut b = Instance::create(&d).unwrap();
    b.set_offline(true);
    b.activate(RATE, BLOCK as u32).unwrap();
    b.load_state(&state).unwrap();
    let rb = render(b.handle());
    let diff = ra
        .left
        .iter()
        .zip(&rb.left)
        .fold(0.0f32, |m, (x, y)| m.max((x - y).abs()));
    println!(
        "{name}: restored render peak {:.4}, max abs diff vs original {diff:e}",
        peak(&rb.left)
    );
    // A fresh instance with no load at all: tells plugin randomness (random
    // oscillator phase, noise) from a lossy state round trip.
    let mut c = Instance::create(&d).unwrap();
    c.set_offline(true);
    c.activate(RATE, BLOCK as u32).unwrap();
    let rc = render(c.handle());
    let diff_fresh = ra
        .left
        .iter()
        .zip(&rc.left)
        .fold(0.0f32, |m, (x, y)| m.max((x - y).abs()));
    println!("{name}: two default instances differ by {diff_fresh:e} (plugin randomness)");
    assert!(!b.save_state().unwrap().is_empty());
}

#[test]
#[ignore = "needs the Surge XT Flatpak extension"]
fn surge_xt() {
    smoke("Surge XT");
}

#[test]
#[ignore = "needs the Odin2 Flatpak extension"]
fn odin2() {
    smoke("Odin2");
}

#[test]
#[ignore = "needs the Dexed Flatpak extension"]
fn dexed() {
    smoke("Dexed");
}
