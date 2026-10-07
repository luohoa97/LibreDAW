// SPDX-License-Identifier: GPL-3.0-or-later
//! The engine as the GTK thread sees it (docs/phase2-interfaces.md): start
//! and stop the stream, hand over compiled states, commands and plugin
//! events, drain engine events. The disposal thread is internal.

use crate::compiled::Compiled;
use crate::live::HostKind;
use crate::runtime::{RtEnds, Runtime, Shared, UiEnds, rings};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{BufferSize, ErrorKind, HostId, SampleFormat, StreamConfig};
use protocol::engine::PluginEvent;
use protocol::engine::{ControlTable, EngineCommand, EngineEvent, EngineStatus, ParamTable};
use rtrb::Consumer;
use rtrb::PushError;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

pub use crate::live::HostKind as Host;

#[derive(Debug)]
pub enum EngineError {
    /// The host, device, or stream could not be opened.
    Device(String),
    /// A request was invalid (unknown pattern, zero loops, ...).
    Invalid(String),
    /// An offline render was cancelled.
    Cancelled,
    Io(std::io::Error),
}

impl std::fmt::Display for EngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EngineError::Device(s) => write!(f, "audio device: {s}"),
            EngineError::Invalid(s) => write!(f, "invalid request: {s}"),
            EngineError::Cancelled => f.write_str("cancelled"),
            EngineError::Io(e) => write!(f, "io: {e}"),
        }
    }
}

impl std::error::Error for EngineError {}

impl From<std::io::Error> for EngineError {
    fn from(e: std::io::Error) -> EngineError {
        EngineError::Io(e)
    }
}

#[derive(Clone, Debug)]
pub struct EngineConfig {
    pub host: Host,
    /// `None`: the host's default output device.
    pub device: Option<String>,
    /// 0 asks the backend for its default.
    pub buffer_frames: u32,
    /// `None`: the device's default rate.
    pub sample_rate: Option<u32>,
}

/// Flags the audio side sets for the GTK side.
struct Flags {
    needs_restart: AtomicBool,
    /// Bit 63 set once probed; policy in bits 32..63, priority in the low 32.
    sched: AtomicU64,
}

/// Callback index at which the scheduling policy is read (cpal promotes the
/// thread from a helper, so the first callbacks may still be SCHED_OTHER).
const SCHED_PROBE_AFTER: u32 = 100;

/// Frees compiled states the audio thread retired (SPEC 3.1, 4.2).
struct Disposal {
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl Disposal {
    fn spawn(mut retired: Consumer<Box<Compiled>>) -> (Disposal, Arc<AtomicBool>) {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let join = std::thread::Builder::new()
            .name("libredaw-disposal".into())
            .spawn(move || {
                loop {
                    while let Ok(b) = retired.pop() {
                        drop(b);
                    }
                    if flag.load(Relaxed) {
                        while let Ok(b) = retired.pop() {
                            drop(b);
                        }
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
            })
            .expect("spawn disposal thread");
        (
            Disposal {
                stop: stop.clone(),
                join: Some(join),
            },
            stop,
        )
    }

    fn shutdown(mut self) {
        self.stop.store(true, Relaxed);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

/// Producer and consumer ends the GTK thread keeps (the retired end lives in
/// the disposal thread).
struct UiSide {
    state: rtrb::Producer<Box<Compiled>>,
    commands: rtrb::Producer<EngineCommand>,
    plugin_events: rtrb::Producer<PluginEvent>,
    events: Consumer<EngineEvent>,
}

pub struct Engine {
    pub controls: Arc<ControlTable>,
    pub params: Arc<ParamTable>,
    pub status: Arc<EngineStatus>,
    ui: UiSide,
    rate: u32,
    stream: Option<cpal::Stream>,
    disposal: Option<Disposal>,
    flags: Arc<Flags>,
}

fn host_id(h: Host) -> (HostId, &'static str) {
    match h {
        HostKind::Alsa => (HostId::Alsa, "ALSA"),
        HostKind::Jack => (HostId::Jack, "JACK"),
        HostKind::PipeWire => (HostId::PipeWire, "PipeWire"),
    }
}

impl Engine {
    /// Output device names of a host.
    pub fn devices(host: Host) -> Vec<String> {
        let (id, _) = host_id(host);
        let Ok(h) = cpal::host_from_id(id) else {
            return Vec::new();
        };
        match h.output_devices() {
            Ok(it) => it.map(|d| d.to_string()).collect(),
            Err(_) => Vec::new(),
        }
    }

    /// Opens the stream and starts calling the engine. Per SPEC 4.7 the
    /// caller recompiles and restarts after a device or rate change.
    pub fn start(cfg: &EngineConfig, first: Box<Compiled>) -> Result<Engine, EngineError> {
        let (id, host_name) = host_id(cfg.host);
        let host = cpal::host_from_id(id)
            .map_err(|e| EngineError::Device(format!("{host_name} host unavailable: {e}")))?;
        let device = match &cfg.device {
            Some(name) => host
                .output_devices()
                .map_err(|e| EngineError::Device(format!("{host_name}: cannot list devices: {e}")))?
                .find(|d| d.to_string() == *name)
                .ok_or_else(|| EngineError::Device(format!("{host_name}: no device '{name}'")))?,
            None => host
                .default_output_device()
                .ok_or_else(|| EngineError::Device(format!("{host_name}: no default output")))?,
        };
        let rate = match cfg.sample_rate {
            Some(r) => r,
            None => device
                .default_output_config()
                .map_err(|e| EngineError::Device(format!("{host_name}: no default config: {e}")))?
                .sample_rate(),
        };
        let ok = device
            .supported_output_configs()
            .map_err(|e| EngineError::Device(format!("{host_name}: cannot list configs: {e}")))?
            .any(|c| {
                c.channels() == 2
                    && c.sample_format() == SampleFormat::F32
                    && c.min_sample_rate() <= rate
                    && rate <= c.max_sample_rate()
            });
        if !ok {
            return Err(EngineError::Device(format!(
                "{host_name} ({device}): no stereo f32 output supports {rate} Hz"
            )));
        }
        let config = StreamConfig {
            channels: 2,
            sample_rate: rate,
            buffer_size: if cfg.buffer_frames == 0 {
                BufferSize::Default
            } else {
                BufferSize::Fixed(cfg.buffer_frames)
            },
        };

        let shared = Shared::new();
        let (ui, rt): (UiEnds, RtEnds) = rings();
        let (disposal, _) = Disposal::spawn(ui.retired);
        let mut runtime = Runtime::new(rate as f64, shared.clone(), rt);
        let _ = runtime.install(first);

        let flags = Arc::new(Flags {
            needs_restart: AtomicBool::new(false),
            sched: AtomicU64::new(0),
        });
        let cb_flags = flags.clone();
        let cb_status = shared.status.clone();
        let err_flags = flags.clone();
        let err_status = shared.status.clone();
        let mut calls: u32 = 0;
        let mut last: Option<Instant> = None;

        let stream = device
            .build_output_stream::<f32, _, _>(
                config,
                move |data, _info| {
                    let now = Instant::now();
                    calls = calls.saturating_add(1);
                    let frames = data.len() / 2;
                    if let Some(prev) = last
                        && calls > SCHED_PROBE_AFTER
                        && now.duration_since(prev).as_secs_f64()
                            > 1.5 * frames as f64 / rate as f64
                    {
                        cb_status.xruns.fetch_add(1, Relaxed);
                    }
                    last = Some(now);
                    runtime.process_interleaved(data);
                    if calls == SCHED_PROBE_AFTER {
                        let (policy, prio, _) = crate::rt::thread_sched();
                        let v = (1u64 << 63) | ((policy as u32 as u64) << 32) | prio as u32 as u64;
                        cb_flags.sched.store(v, Relaxed);
                    }
                },
                move |e| match e.kind() {
                    ErrorKind::Xrun => {
                        err_status.xruns.fetch_add(1, Relaxed);
                    }
                    ErrorKind::DeviceNotAvailable
                    | ErrorKind::StreamInvalidated
                    | ErrorKind::UnsupportedConfig => err_flags.needs_restart.store(true, Relaxed),
                    _ => {}
                },
                None,
            )
            .map_err(|e| {
                EngineError::Device(format!(
                    "{host_name} ({device}): build_output_stream at {rate} Hz, buffer {}: {e}",
                    cfg.buffer_frames
                ))
            })?;
        stream
            .play()
            .map_err(|e| EngineError::Device(format!("{host_name}: play failed: {e}")))?;

        Ok(Engine {
            controls: shared.controls,
            params: shared.params,
            status: shared.status,
            ui: UiSide {
                state: ui.state,
                commands: ui.commands,
                plugin_events: ui.plugin_events,
                events: ui.events,
            },
            rate,
            stream: Some(stream),
            disposal: Some(disposal),
            flags,
        })
    }

    /// SPEC 4.7 step 1: stops the stream and waits for it to be dropped.
    /// The `Runtime` and `Compiled` are freed on this thread; started
    /// plugins get `stop_processing` before this returns.
    pub fn stop(mut self) {
        drop(self.stream.take());
        if let Some(d) = self.disposal.take() {
            d.shutdown();
        }
    }

    pub fn sample_rate(&self) -> f64 {
        self.rate as f64
    }

    /// True when the backend invalidated the stream or lost the device:
    /// stop, rebuild, and start again (SPEC 4.7).
    pub fn needs_restart(&self) -> bool {
        self.flags.needs_restart.load(Relaxed)
    }

    /// `(policy, priority)` of the callback thread, read once inside the
    /// callback after 100 calls. Policy numbers are Linux's (1 FIFO, 2 RR),
    /// possibly OR-ed with SCHED_RESET_ON_FORK (0x40000000).
    pub fn callback_sched(&self) -> Option<(i32, i32)> {
        let v = self.flags.sched.load(Relaxed);
        (v >> 63 == 1).then_some((((v >> 32) & 0x7fff_ffff) as i32, v as u32 as i32))
    }

    /// Queues a compiled state. `Err` returns it when the state ring is
    /// full; retry later.
    pub fn submit(&mut self, c: Box<Compiled>) -> Result<(), Box<Compiled>> {
        self.ui.state.push(c).map_err(|PushError::Full(c)| c)
    }

    pub fn command(&mut self, c: EngineCommand) -> Result<(), EngineCommand> {
        self.ui.commands.push(c).map_err(|PushError::Full(c)| c)
    }

    pub fn plugin_event(&mut self, e: PluginEvent) -> Result<(), PluginEvent> {
        self.ui
            .plugin_events
            .push(e)
            .map_err(|PushError::Full(e)| e)
    }

    /// Hands every pending engine event to `f`. Call from the 10 ms source.
    pub fn drain_events(&mut self, mut f: impl FnMut(EngineEvent)) {
        while let Ok(e) = self.ui.events.pop() {
            f(e);
        }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        drop(self.stream.take());
        if let Some(d) = self.disposal.take() {
            d.shutdown();
        }
    }
}
