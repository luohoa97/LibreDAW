// SPDX-License-Identifier: GPL-3.0-or-later
//! The engine as the GTK thread sees it (docs/phase2-interfaces.md): start
//! and stop the stream, hand over compiled states, commands and plugin
//! events, drain engine events. The disposal thread is internal.

use crate::audition::AuditionSample;
use crate::compiled::Compiled;
use crate::live::HostKind;
use crate::runtime::{RtEnds, Runtime, Shared, UiEnds, rings};
use crate::samples::{SampleData, SampleStore, hash_hex};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{BufferSize, ErrorKind, HostId, SampleFormat, StreamConfig};
use protocol::engine::{AuditionSource, PluginEvent};
use protocol::engine::{ControlTable, EngineCommand, EngineEvent, EngineStatus, ParamTable};
use rtrb::Consumer;
use rtrb::PushError;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering::Relaxed};
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
    /// Outcome of the real-time request (the portal inside a Flatpak).
    rt_report: crate::portal_rt::Report,
}

/// Callback index at which the scheduling policy is read (cpal promotes the
/// thread from a helper, so the first callbacks may still be SCHED_OTHER).
const SCHED_PROBE_AFTER: u32 = 100;

/// Per-callback timing the audio thread records for load measurements
/// (`loadbench`). All storage is allocated up front; the callback only does
/// relaxed atomic stores, so recording never allocates or blocks.
pub struct CallbackProbe {
    /// Time spent in the engine's process call, nanoseconds.
    proc_ns: Box<[AtomicU64]>,
    /// Frames of the callback.
    frames: Box<[AtomicU32]>,
    count: AtomicUsize,
    /// Callbacks that did not fit.
    pub overflow: AtomicU64,
    /// Starts more than 1.5 periods after the previous one (after warm-up).
    pub gap_xruns: AtomicU64,
    /// `ErrorKind::Xrun` reported by the backend.
    pub backend_xruns: AtomicU64,
}

impl CallbackProbe {
    pub fn new(capacity: usize) -> Arc<CallbackProbe> {
        Arc::new(CallbackProbe {
            proc_ns: (0..capacity).map(|_| AtomicU64::new(0)).collect(),
            frames: (0..capacity).map(|_| AtomicU32::new(0)).collect(),
            count: AtomicUsize::new(0),
            overflow: AtomicU64::new(0),
            gap_xruns: AtomicU64::new(0),
            backend_xruns: AtomicU64::new(0),
        })
    }

    fn record(&self, frames: usize, proc_ns: u64) {
        let i = self.count.load(Relaxed);
        if i < self.proc_ns.len() {
            self.proc_ns[i].store(proc_ns, Relaxed);
            self.frames[i].store(frames as u32, Relaxed);
            self.count.store(i + 1, Relaxed);
        } else {
            self.overflow.fetch_add(1, Relaxed);
        }
    }

    /// `(process nanoseconds, frames)` per callback, in order. Call after
    /// the engine has stopped.
    pub fn samples(&self) -> Vec<(u64, u32)> {
        let n = self.count.load(Relaxed);
        (0..n)
            .map(|i| (self.proc_ns[i].load(Relaxed), self.frames[i].load(Relaxed)))
            .collect()
    }
}

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
    audition: rtrb::Producer<AuditionSample>,
    audition_retired: Consumer<SampleData>,
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
    /// The microphone stream and its worker; `Some` only while recording.
    capture: Option<(cpal::Stream, crate::capture::Capture)>,
    /// Keeps the portal helper thread alive for the life of the stream.
    _promoter: Option<Arc<crate::portal_rt::Promoter>>,
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
        Engine::start_with_probe(cfg, first, None)
    }

    /// As `start`, and records every callback's processing time in `probe`.
    pub fn start_with_probe(
        cfg: &EngineConfig,
        first: Box<Compiled>,
        probe: Option<Arc<CallbackProbe>>,
    ) -> Result<Engine, EngineError> {
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
            rt_report: crate::portal_rt::new_report(),
        });
        // Inside a Flatpak the direct rtkit request fails; the portal
        // promotes the callback thread instead (off the audio thread).
        let promoter =
            crate::portal_rt::Promoter::spawn(rate, cfg.buffer_frames, flags.rt_report.clone())
                .map(Arc::new);
        let cb_promoter = promoter.clone();
        let cb_flags = flags.clone();
        let cb_status = shared.status.clone();
        let err_flags = flags.clone();
        let err_status = shared.status.clone();
        let mut calls: u32 = 0;
        let mut last: Option<Instant> = None;
        let cb_probe = probe.clone();
        let err_probe = probe;

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
                        if let Some(p) = &cb_probe {
                            p.gap_xruns.fetch_add(1, Relaxed);
                        }
                    }
                    last = Some(now);
                    if calls == 1
                        && let Some(p) = &cb_promoter
                    {
                        p.request_from_callback();
                    }
                    runtime.process_interleaved(data);
                    if let Some(p) = &cb_probe {
                        p.record(frames, now.elapsed().as_nanos() as u64);
                    }
                    if calls == SCHED_PROBE_AFTER {
                        let (policy, prio, _) = crate::rt::thread_sched();
                        let v = (1u64 << 63) | ((policy as u32 as u64) << 32) | prio as u32 as u64;
                        cb_flags.sched.store(v, Relaxed);
                    }
                },
                move |e| match e.kind() {
                    ErrorKind::Xrun => {
                        err_status.xruns.fetch_add(1, Relaxed);
                        if let Some(p) = &err_probe {
                            p.backend_xruns.fetch_add(1, Relaxed);
                        }
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
                audition: ui.audition,
                audition_retired: ui.audition_retired,
            },
            rate,
            stream: Some(stream),
            disposal: Some(disposal),
            flags,
            capture: None,
            _promoter: promoter,
        })
    }

    /// Opens the default input device and starts recording (SPEC 21.2).
    /// Call only from a user's click or key press: GNOME shows its
    /// microphone indicator while this runs. Does nothing if already on.
    pub fn start_capture(&mut self) -> Result<(), EngineError> {
        if self.capture.is_some() {
            return Ok(());
        }
        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .ok_or_else(|| EngineError::Device("no microphone found".into()))?;
        let default = device
            .default_input_config()
            .map_err(|e| EngineError::Device(format!("microphone: no default config: {e}")))?;
        let rate = default.sample_rate();
        let channels = default.channels();
        let config = StreamConfig {
            channels,
            sample_rate: rate,
            buffer_size: BufferSize::Default,
        };
        let (cap, mut feeder) = crate::capture::Capture::new(rate, channels);
        let stream = device
            .build_input_stream::<f32, _, _>(
                config,
                move |data, _info| feeder.feed(data),
                |_e| {},
                None,
            )
            .map_err(|e| EngineError::Device(format!("microphone ({device}): {e}")))?;
        stream
            .play()
            .map_err(|e| EngineError::Device(format!("microphone: play failed: {e}")))?;
        self.capture = Some((stream, cap));
        Ok(())
    }

    pub fn is_capturing(&self) -> bool {
        self.capture.is_some()
    }

    /// Peak of the latest input block, for the live level meter.
    pub fn capture_level(&self) -> f32 {
        self.capture.as_ref().map_or(0.0, |(_, c)| c.level())
    }

    /// Closes the microphone and returns what was recorded (empty if capture
    /// was not on). The recording is only in memory.
    pub fn stop_capture(&mut self) -> crate::capture::Captured {
        match self.capture.take() {
            Some((stream, cap)) => {
                drop(stream);
                cap.finish()
            }
            None => crate::capture::Captured::default(),
        }
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

    /// How the real-time priority request went, for logs and the load
    /// probe next to `callback_sched`: inside a Flatpak the portal's answer
    /// (granted, or the reason it failed), otherwise a note that cpal's
    /// direct rtkit request applies. "pending" until the first callback.
    pub fn realtime_report(&self) -> String {
        self.flags
            .rt_report
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Queues a compiled state. `Err` returns it when the state ring is
    /// full; retry later.
    pub fn submit(&mut self, c: Box<Compiled>) -> Result<(), Box<Compiled>> {
        self.ui.state.push(c).map_err(|PushError::Full(c)| c)
    }

    /// Starts or releases a sound-browser audition (20.3). For
    /// `AuditionSource::Sample` the decoded sample is looked up in `store`
    /// by hash and handed to the audio thread first; a sample that is not
    /// ready in the store plays silence. `Err` returns the command when the
    /// command ring is full.
    #[allow(clippy::result_large_err)]
    pub fn audition(
        &mut self,
        store: Option<&SampleStore>,
        source: AuditionSource,
        key: u8,
        vel: u8,
        on: bool,
    ) -> Result<(), EngineCommand> {
        while self.ui.audition_retired.pop().is_ok() {}
        if let (AuditionSource::Sample { hash }, true, Some(store)) = (&source, on, store)
            && let Some(data) = store.get(&hash_hex(hash))
        {
            // A full inbox means earlier samples are still unread; the
            // newest wins once the audio thread catches up.
            let _ = self.ui.audition.push(AuditionSample { hash: *hash, data });
        }
        self.command(EngineCommand::Audition {
            source,
            key,
            vel,
            on,
        })
    }

    /// `Err` returns the command when the ring is full. The command is big
    /// since `Audition` carries synth parameters by value (no allocation
    /// on the audio thread).
    #[allow(clippy::result_large_err)]
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
