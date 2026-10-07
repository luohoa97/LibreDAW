// SPDX-License-Identifier: GPL-3.0-or-later
//! Offline render (SPEC 8): the same `Runtime` code path as live playback,
//! on the calling (export) thread with its own `Runtime`.

use crate::api::EngineError;
use crate::compiled::{Slots, compile_with};
use crate::runtime::{Runtime, Shared, rings};
use crate::samples::{SampleData, SampleState, SampleStore, resample};
use crate::tables::write_controls;
use crate::transport::{Transport, samples_per_tick};
use protocol::consts::{MAX_TEMPO_BPM, MIN_TEMPO_BPM};
use protocol::engine::{EngineCommand, PluginHandle, PluginSlot};
use protocol::model::{Instrument, Project};
use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering::Relaxed};
use std::time::{Duration, Instant};

/// What `render_range` renders: ticks `start..end` of the timeline plus a
/// tail.
pub struct RangeRequest {
    pub project: Arc<Project>,
    /// `(start, end)` in ticks, `start < end`. `None` renders the loop
    /// region when it is enabled, else `0..arrangement end` (the end of the
    /// last clip). The render never loops.
    pub range: Option<(u32, u32)>,
    /// Seconds rendered after `end` with the transport stopped, so tails
    /// (releases, reverb, delay) ring out.
    pub tail_seconds: f64,
    pub sample_rate: u32,
    /// The live sample store, for sampler channels. `None` renders them as
    /// silence. See `Rendered::warnings` and `prepare_store` for rate and
    /// loading handling.
    pub store: Option<Arc<SampleStore>>,
}

/// Callback size `render_range` uses.
pub const DEFAULT_RENDER_BLOCK: usize = 512;

/// Renders a range of the timeline plus a tail, stereo (20.2). `progress`
/// receives 0 to 100. Export plugin instances are created and attached by
/// the caller's thread and passed as handles; this thread is their only
/// audio thread. Errors when the range is empty (no range given, the loop
/// region is off and no clip has content).
pub fn render_range(
    req: &RangeRequest,
    slots: &Slots,
    plugins: &[(PluginSlot, PluginHandle)],
    progress: &AtomicU32,
    cancel: &AtomicBool,
) -> Result<Rendered, EngineError> {
    render_range_with_block(req, slots, plugins, progress, cancel, DEFAULT_RENDER_BLOCK)
}

/// As `render_range` with an explicit callback size. The output does not
/// depend on it (built-in DSP is per sample and control values are constant).
pub fn render_range_with_block(
    req: &RangeRequest,
    slots: &Slots,
    plugins: &[(PluginSlot, PluginHandle)],
    progress: &AtomicU32,
    cancel: &AtomicBool,
    callback_frames: usize,
) -> Result<Rendered, EngineError> {
    if req.sample_rate == 0 || callback_frames == 0 {
        return Err(EngineError::Invalid(
            "sample rate and block must be > 0".into(),
        ));
    }
    let p = &req.project;
    if !(MIN_TEMPO_BPM..=MAX_TEMPO_BPM).contains(&p.tempo_bpm) {
        return Err(EngineError::Invalid(format!(
            "tempo {} out of range",
            p.tempo_bpm
        )));
    }
    let sr = req.sample_rate as f64;
    let (store, warnings) = prepare_store(p, req.store.as_deref(), req.sample_rate, cancel)?;
    let mut compiled = compile_with(p, slots, sr, store.as_deref());
    let (start, end) = match req.range {
        Some(r) => r,
        None if compiled.loop_enabled => (compiled.loop_start, compiled.loop_end),
        None => (0, compiled.song_len_ticks),
    };
    if end <= start {
        return Err(EngineError::Invalid("nothing to render".into()));
    }
    // A render plays the range once: the loop is off, and the sequencer's
    // end-of-arrangement stop is harmless (the pump keeps pulling frames).
    compiled.loop_enabled = false;

    let shared = Shared::new();
    write_controls(p, slots, &shared.controls, &shared.params);
    let (_ui, ends) = rings();
    let mut rt = Runtime::new(sr, shared, ends);
    rt.set_metronome_allowed(false);
    rt.set_previews_allowed(false);
    let _ = rt.install(compiled);
    for &(slot, handle) in plugins {
        rt.command(EngineCommand::AttachPlugin { slot, handle });
    }
    rt.command(EngineCommand::Seek { tick: start as u64 });
    rt.command(EngineCommand::Play);

    let spt = samples_per_tick(sr, p.tempo_bpm);
    let main = Transport::at(0, 0, spt).sample_of_tick((end - start) as i64);
    let tail = (req.tail_seconds.max(0.0) * sr).round() as u64;
    let audio = pump(&mut rt, main, tail, callback_frames, progress, cancel)?;
    Ok(Rendered { audio, warnings })
}

/// Renders `main` frames of playback and then `tail` frames after a stop.
fn pump(
    rt: &mut Runtime,
    main: u64,
    tail: u64,
    callback_frames: usize,
    progress: &AtomicU32,
    cancel: &AtomicBool,
) -> Result<Vec<[f32; 2]>, EngineError> {
    let total = main + tail;

    let mut out: Vec<[f32; 2]> = Vec::with_capacity(total as usize);
    let mut l = vec![0.0f32; callback_frames];
    let mut r = vec![0.0f32; callback_frames];
    let mut done = 0u64;
    let mut stopped = false;
    progress.store(0, Relaxed);
    while done < total {
        if cancel.load(Relaxed) {
            return Err(EngineError::Cancelled);
        }
        if !stopped && done >= main {
            rt.command(EngineCommand::Stop);
            stopped = true;
        }
        let limit = if stopped { total } else { main };
        let n = (limit - done).min(callback_frames as u64) as usize;
        rt.process_planar(&mut l[..n], &mut r[..n]);
        out.extend(l[..n].iter().zip(&r[..n]).map(|(&a, &b)| [a, b]));
        done += n as u64;
        progress.store((done * 100 / total.max(1)) as u32, Relaxed);
    }
    progress.store(100, Relaxed);
    Ok(out)
}

/// The result of an offline render.
#[derive(Debug, Clone, PartialEq)]
pub struct Rendered {
    /// Stereo frames at the request's sample rate.
    pub audio: Vec<[f32; 2]>,
    /// Problems that did not stop the render: samplers whose sample was
    /// missing, failed to load or was still loading after
    /// `SAMPLE_WAIT_SECONDS` rendered as silence. Empty when all is well.
    pub warnings: Vec<String>,
}

/// How long a render waits for samples that are still loading.
pub const SAMPLE_WAIT_SECONDS: u64 = 10;

/// Sample hashes the project's sampler channels use.
fn used_samples(p: &Project) -> BTreeSet<&str> {
    p.channels
        .iter()
        .filter_map(|c| match &c.instrument {
            Instrument::Sampler(s) => s.sample.as_deref(),
            _ => None,
        })
        .collect()
}

/// The store a render compiles against, plus warnings.
///
/// Sampler data in the live store is resampled to the live stream rate. When
/// that differs from the render rate, playing it as is would shift pitch and
/// length, so the render uses a private temporary store at the render rate,
/// filled by resampling each used sample from the live store's rate (this is
/// offline, so the cost does not matter). That is simpler than re-decoding
/// the sources, because the live store keeps no original, and it leaves the
/// live store untouched. Samples still loading are waited for up to
/// `SAMPLE_WAIT_SECONDS`; anything not ready after that compiles as silence
/// with a warning.
fn prepare_store(
    p: &Project,
    store: Option<&SampleStore>,
    rate: u32,
    cancel: &AtomicBool,
) -> Result<(Option<Arc<SampleStore>>, Vec<String>), EngineError> {
    let used = used_samples(p);
    let mut warnings = Vec::new();
    let Some(store) = store else {
        for h in &used {
            warnings.push(format!("sample {h} is unavailable (no sample store)"));
        }
        return Ok((None, warnings));
    };
    let deadline = Instant::now() + Duration::from_secs(SAMPLE_WAIT_SECONDS);
    let mut ready: Vec<(&str, SampleData)> = Vec::new();
    for &h in &used {
        loop {
            match store.state(h) {
                Some(SampleState::Ready(d)) => {
                    ready.push((h, d));
                    break;
                }
                Some(SampleState::Loading) if Instant::now() < deadline => {
                    if cancel.load(Relaxed) {
                        return Err(EngineError::Cancelled);
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                Some(SampleState::Loading) => {
                    warnings.push(format!(
                        "sample {h} was still loading after {SAMPLE_WAIT_SECONDS} s"
                    ));
                    break;
                }
                Some(SampleState::Failed(e)) => {
                    warnings.push(format!("sample {h} failed to load: {e}"));
                    break;
                }
                None => {
                    warnings.push(format!("sample {h} is not in the sample store"));
                    break;
                }
            }
        }
    }
    let tmp = SampleStore::new(rate, store.budget_bytes());
    for (h, d) in ready {
        if d.rate == rate {
            tmp.insert(h, d);
        } else {
            let data = resample(&d.data, d.channels.max(1) as usize, d.rate, rate);
            tmp.insert(h, SampleData::from_vec(d.channels, rate, data));
        }
    }
    Ok((Some(Arc::new(tmp)), warnings))
}
