// SPDX-License-Identifier: GPL-3.0-or-later
//! `metronome --host alsa|jack --buffer N --seconds S --rate R --bpm B --gain G`
//!
//! Plays a 4/4 click on a real audio device and prints timing measurements.

use engine::live::{HostKind, LiveParams, run};
use std::process::ExitCode;

const USAGE: &str = "usage: metronome [--host alsa|jack] [--buffer FRAMES] [--seconds S] \
[--rate HZ] [--bpm B] [--gain G]";

fn parse() -> Result<LiveParams, String> {
    let mut p = LiveParams {
        host: HostKind::Alsa,
        buffer: 256,
        seconds: 10.0,
        rate: 48000,
        bpm: 120.0,
        gain: 0.5,
    };
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let mut value = || args.next().ok_or(format!("{flag} needs a value"));
        match flag.as_str() {
            "--host" => {
                p.host = match value()?.as_str() {
                    "alsa" => HostKind::Alsa,
                    "jack" => HostKind::Jack,
                    other => return Err(format!("unknown host '{other}'")),
                }
            }
            "--buffer" => p.buffer = value()?.parse().map_err(|e| format!("--buffer: {e}"))?,
            "--seconds" => p.seconds = value()?.parse().map_err(|e| format!("--seconds: {e}"))?,
            "--rate" => p.rate = value()?.parse().map_err(|e| format!("--rate: {e}"))?,
            "--bpm" => p.bpm = value()?.parse().map_err(|e| format!("--bpm: {e}"))?,
            "--gain" => p.gain = value()?.parse().map_err(|e| format!("--gain: {e}"))?,
            "-h" | "--help" => return Err(String::new()),
            other => return Err(format!("unknown argument '{other}'")),
        }
    }
    if !(20.0..=999.0).contains(&p.bpm) || p.seconds <= 0.0 || p.rate == 0 {
        return Err("bpm must be 20..999, seconds > 0, rate > 0".into());
    }
    Ok(p)
}

fn main() -> ExitCode {
    let p = match parse() {
        Ok(p) => p,
        Err(e) => {
            if !e.is_empty() {
                eprintln!("metronome: {e}");
            }
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    match run(&p) {
        Ok(report) => {
            println!("{}", report.result_line());
            print!("{}", report.summary());
            ExitCode::SUCCESS
        }
        Err(e) => {
            let host = if p.host == HostKind::Alsa {
                "alsa"
            } else {
                "jack"
            };
            println!(
                "RESULT host={host} buffer_req={} rate={} bpm={} status=failed",
                p.buffer, p.rate, p.bpm
            );
            eprintln!("metronome: FAILED: {e}");
            ExitCode::from(1)
        }
    }
}
