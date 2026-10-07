// SPDX-License-Identifier: GPL-3.0-or-later
//! `loadbench --host pipewire|alsa|jack --buffer N --seconds S --rate R`
//! plays the heavy load project (16 channels, effects, song mode) through
//! `Engine::start` on a real device and reports the DSP load per callback.
//! `loadbench --offline` renders 60 s of the same project with `render_song`.

use engine::loadproject::{LOAD_BPM, load_project, load_project_clips, load_store};
use engine::report::{percentile, sched_name};
use engine::{
    CallbackProbe, Engine, EngineConfig, Host, Slots, SongRequest, compile_with, render_song,
    write_controls,
};
use protocol::engine::{EngineCommand, TransportMode};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering::Relaxed};
use std::time::{Duration, Instant};

const USAGE: &str = "usage: loadbench [--host alsa|jack|pipewire] [--buffer FRAMES] \
[--seconds S] [--rate HZ] | --offline [--rate HZ]";

struct Args {
    host: Host,
    buffer: u32,
    seconds: f64,
    rate: u32,
    offline: bool,
}

fn parse() -> Result<Args, String> {
    let mut a = Args {
        host: Host::PipeWire,
        buffer: 256,
        seconds: 10.0,
        rate: 48000,
        offline: false,
    };
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let mut value = || args.next().ok_or(format!("{flag} needs a value"));
        match flag.as_str() {
            "--host" => {
                a.host = match value()?.as_str() {
                    "alsa" => Host::Alsa,
                    "jack" => Host::Jack,
                    "pipewire" => Host::PipeWire,
                    other => return Err(format!("unknown host '{other}'")),
                }
            }
            "--buffer" => a.buffer = value()?.parse().map_err(|e| format!("--buffer: {e}"))?,
            "--seconds" => a.seconds = value()?.parse().map_err(|e| format!("--seconds: {e}"))?,
            "--rate" => a.rate = value()?.parse().map_err(|e| format!("--rate: {e}"))?,
            "--offline" => a.offline = true,
            "-h" | "--help" => return Err(String::new()),
            other => return Err(format!("unknown argument '{other}'")),
        }
    }
    if a.seconds <= 0.0 || a.rate == 0 {
        return Err("seconds > 0 and rate > 0 required".into());
    }
    Ok(a)
}

/// Peak resident set size in kB from /proc/self/status (VmHWM).
fn peak_rss_kb() -> Option<u64> {
    std::fs::read_to_string("/proc/self/status")
        .ok()?
        .lines()
        .find_map(|l| l.strip_prefix("VmHWM:"))
        .and_then(|v| v.trim().trim_end_matches("kB").trim().parse().ok())
}

fn host_name(h: Host) -> &'static str {
    match h {
        Host::Alsa => "alsa",
        Host::Jack => "jack",
        Host::PipeWire => "pipewire",
    }
}

fn offline(a: &Args) -> Result<(), String> {
    // 35 bars at 140 BPM are 60.0 s of audio
    let bars = (60.0 * LOAD_BPM / 60.0 / 4.0).ceil() as u32;
    let project = std::sync::Arc::new(load_project_clips(bars));
    let store = load_store(a.rate);
    let mut slots = Slots::new();
    slots.sync(&project).map_err(|e| format!("slots: {e:?}"))?;
    let req = SongRequest {
        project,
        tail_seconds: 0.0,
        sample_rate: a.rate,
        store: Some(store),
    };
    let progress = AtomicU32::new(0);
    let cancel = AtomicBool::new(false);
    let t = Instant::now();
    let r = render_song(&req, &slots, &[], &progress, &cancel).map_err(|e| e.to_string())?;
    let wall = t.elapsed().as_secs_f64();
    let audio = r.audio.len() as f64 / a.rate as f64;
    let peak = r
        .audio
        .iter()
        .flat_map(|f| f.iter())
        .fold(0.0f32, |m, v| m.max(v.abs()));
    let rss = peak_rss_kb();
    println!(
        "RESULT mode=offline rate={} audio_s={audio:.3} wall_s={wall:.3} realtime_factor={:.2} \
         peak_rss_kb={} peak_sample={peak:.3} warnings={}",
        a.rate,
        audio / wall,
        rss.map_or("na".into(), |v| v.to_string()),
        r.warnings.len()
    );
    for w in &r.warnings {
        eprintln!("warning: {w}");
    }
    Ok(())
}

fn live(a: &Args) -> Result<(), String> {
    let project = load_project();
    let store = load_store(a.rate);
    let mut slots = Slots::new();
    slots.sync(&project).map_err(|e| format!("slots: {e:?}"))?;
    let compiled = compile_with(&project, &slots, a.rate as f64, Some(&store));

    // room for every callback of the run at the smallest plausible size
    let frames = a.buffer.max(32) as f64;
    let capacity = (a.seconds * a.rate as f64 / frames * 2.0) as usize + 4096;
    let probe = CallbackProbe::new(capacity);
    let cfg = EngineConfig {
        host: a.host,
        device: None,
        buffer_frames: a.buffer,
        sample_rate: Some(a.rate),
    };
    let mut e =
        Engine::start_with_probe(&cfg, compiled, Some(probe.clone())).map_err(|e| e.to_string())?;
    write_controls(&project, &slots, &e.controls, &e.params);
    let send = |e: &mut Engine, c: EngineCommand| {
        let mut c = Some(c);
        while let Some(x) = c.take() {
            if let Err(x) = e.command(x) {
                c = Some(x);
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    };
    send(
        &mut e,
        EngineCommand::SetTransportMode {
            mode: TransportMode::Song,
            loop_song: true,
        },
    );
    send(&mut e, EngineCommand::Seek { tick: 0 });
    send(&mut e, EngineCommand::Play);

    std::thread::sleep(Duration::from_secs_f64(a.seconds));
    let playing = e.status.playing.load(Relaxed);
    let tick = e.status.playhead_tick.load(Relaxed);
    let sched = e.callback_sched();
    let restart = e.needs_restart();
    e.stop(); // joins the audio thread; the probe is quiet from here on

    let samples = probe.samples();
    let rate = a.rate as f64;
    let mut load: Vec<f64> = samples
        .iter()
        .map(|&(ns, f)| ns as f64 / 1e9 / (f as f64 / rate) * 100.0)
        .collect();
    let mut frames_sorted: Vec<u32> = samples.iter().map(|s| s.1).collect();
    frames_sorted.sort_unstable();
    let cb_median = frames_sorted
        .get(frames_sorted.len() / 2)
        .copied()
        .unwrap_or(0);
    let total_ns: u64 = samples.iter().map(|s| s.0).sum();
    let mean = load.iter().sum::<f64>() / load.len().max(1) as f64;
    load.sort_by(f64::total_cmp);
    let (p99, p999, max) = (
        percentile(&load, 0.99),
        percentile(&load, 0.999),
        load.last().copied().unwrap_or(0.0),
    );
    let backend = probe.backend_xruns.load(Relaxed);
    let gap = probe.gap_xruns.load(Relaxed);
    let dropped = probe.overflow.load(Relaxed);
    let sched_text = sched.map_or("na".into(), |(p, prio)| {
        format!("{}/prio{}", sched_name(p), prio)
    });
    println!(
        "callbacks:    {} (median {} frames, {:.1} us period, {} not recorded)",
        samples.len(),
        cb_median,
        cb_median as f64 / rate * 1e6,
        dropped
    );
    println!("xruns:        backend {backend}, gap {gap}");
    println!(
        "DSP load %:   mean {mean:.2} p99 {p99:.2} p99.9 {p999:.2} max {max:.2} (processing {:.1} ms total)",
        total_ns as f64 / 1e6
    );
    println!("thread:       {sched_text}");
    println!("transport:    playing={playing} playhead_tick={tick} needs_restart={restart}");
    println!(
        "RESULT mode=live host={} buffer_req={} buffer_cb_median={} rate={} seconds={} callbacks={} \
         xruns_backend={backend} xruns_gap={gap} load_mean_pct={mean:.2} load_p99_pct={p99:.2} \
         load_p999_pct={p999:.2} load_max_pct={max:.2} dropped_cb={dropped} sched={sched_text} playing={playing}",
        host_name(a.host),
        a.buffer,
        cb_median,
        a.rate,
        a.seconds,
        samples.len(),
    );
    Ok(())
}

fn main() -> ExitCode {
    let a = match parse() {
        Ok(a) => a,
        Err(e) => {
            if !e.is_empty() {
                eprintln!("loadbench: {e}");
            }
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    let r = if a.offline { offline(&a) } else { live(&a) };
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            println!(
                "RESULT mode={} host={} buffer_req={} rate={} status=failed",
                if a.offline { "offline" } else { "live" },
                host_name(a.host),
                a.buffer,
                a.rate
            );
            eprintln!("loadbench: FAILED: {e}");
            ExitCode::from(1)
        }
    }
}
