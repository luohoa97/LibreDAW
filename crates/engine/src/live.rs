// SPDX-License-Identifier: GPL-3.0-or-later
//! The metronome on a real audio device through cpal.

use crate::metronome::{CallbackState, Controls, Metronome};
use crate::recorder::Recorder;
use crate::report::{ERR_XRUN, Report, RunInfo, analyze};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{BufferSize, ErrorKind, HostId, SampleFormat, StreamConfig};
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum HostKind {
    Alsa,
    Jack,
}

#[derive(Clone, Debug)]
pub struct LiveParams {
    pub host: HostKind,
    /// 0 asks the backend for its default.
    pub buffer: u32,
    pub seconds: f64,
    pub rate: u32,
    pub bpm: f64,
    pub gain: f32,
}

fn error_code(kind: ErrorKind) -> u8 {
    match kind {
        ErrorKind::Xrun => ERR_XRUN,
        ErrorKind::DeviceBusy => 2,
        ErrorKind::DeviceChanged => 3,
        ErrorKind::DeviceNotAvailable => 4,
        ErrorKind::HostUnavailable => 5,
        ErrorKind::InvalidInput => 6,
        ErrorKind::PermissionDenied => 7,
        ErrorKind::RealtimeDenied => 8,
        ErrorKind::ResourceExhausted => 9,
        ErrorKind::StreamInvalidated => 10,
        ErrorKind::UnsupportedConfig => 11,
        ErrorKind::UnsupportedOperation => 12,
        ErrorKind::BackendError => 13,
        _ => 0,
    }
}

/// Runs the stream for `seconds`, then analyzes it. `Err` carries a
/// human-readable reason; nothing is silently substituted (for example a
/// buffer size the backend rejects is reported, not replaced).
pub fn run(p: &LiveParams) -> Result<Report, String> {
    let (id, host_name) = match p.host {
        HostKind::Alsa => (HostId::Alsa, "ALSA"),
        HostKind::Jack => (HostId::Jack, "JACK"),
    };
    let host = cpal::host_from_id(id).map_err(|e| format!("{host_name} host unavailable: {e}"))?;
    let device = host
        .default_output_device()
        .ok_or_else(|| format!("{host_name}: no default output device"))?;
    let device_name = device.to_string();

    let config = StreamConfig {
        channels: 2,
        sample_rate: p.rate,
        buffer_size: if p.buffer == 0 {
            BufferSize::Default
        } else {
            BufferSize::Fixed(p.buffer)
        },
    };
    let ok = device
        .supported_output_configs()
        .map_err(|e| format!("{host_name}: cannot list configs: {e}"))?
        .any(|c| {
            c.channels() == 2
                && c.sample_format() == SampleFormat::F32
                && c.min_sample_rate() <= p.rate
                && p.rate <= c.max_sample_rate()
        });
    if !ok {
        return Err(format!(
            "{host_name} ({device_name}): no stereo f32 output config supports {} Hz",
            p.rate
        ));
    }

    let per_callback = p.buffer.max(32) as f64;
    let callbacks = (p.seconds * p.rate as f64 / per_callback * 2.0) as usize + 4096;
    let onsets = (p.seconds * p.bpm / 60.0) as usize + 64;
    let recorder = Recorder::new(callbacks, onsets, 4096);
    let controls = std::sync::Arc::new(Controls::new(p.bpm, p.gain));
    let metronome = Metronome::new(p.rate, 2, controls, recorder.clone(), None);
    let mut state = CallbackState::new(metronome, recorder.clone());
    let err_recorder = recorder.clone();

    let stream = device
        .build_output_stream::<f32, _, _>(
            config,
            move |data, info| {
                let ts = info.timestamp().callback.as_nanos() as u64;
                state.process(data, ts);
            },
            move |e| err_recorder.push_error(error_code(e.kind())),
            None,
        )
        .map_err(|e| {
            format!(
                "{host_name} ({device_name}): build_output_stream rejected rate {} Hz, buffer {}: {e}",
                p.rate,
                if p.buffer == 0 { "default".into() } else { p.buffer.to_string() }
            )
        })?;
    let reported_buffer = stream.buffer_size().ok();
    stream
        .play()
        .map_err(|e| format!("{host_name}: play failed: {e}"))?;
    std::thread::sleep(Duration::from_secs_f64(p.seconds));
    drop(stream); // joins the audio thread; the recorder is quiet from here on

    let info = RunInfo {
        host: host_name.into(),
        device: device_name,
        requested_buffer: p.buffer,
        reported_buffer,
        rate: p.rate,
        bpm: p.bpm,
        gain: p.gain,
        seconds: p.seconds,
    };
    Ok(analyze(info, &recorder))
}
