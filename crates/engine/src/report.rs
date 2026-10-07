// SPDX-License-Identifier: GPL-3.0-or-later
//! Turns a `Recorder` into numbers (SPEC 14 measurements). Main thread only,
//! after the stream has stopped.

use crate::recorder::Recorder;
use crate::transport::PPQ;

/// Error-event codes stored by `Recorder::push_error`.
pub const ERR_XRUN: u8 = 1;
pub const ERR_NAMES: [&str; 16] = [
    "other",
    "xrun",
    "device_busy",
    "device_changed",
    "device_not_available",
    "host_unavailable",
    "invalid_input",
    "permission_denied",
    "realtime_denied",
    "resource_exhausted",
    "stream_invalidated",
    "unsupported_config",
    "unsupported_operation",
    "backend_error",
    "reserved14",
    "reserved15",
];

/// What the run was asked to do and what the backend said about it.
#[derive(Clone, Debug)]
pub struct RunInfo {
    pub host: String,
    pub device: String,
    pub requested_buffer: u32,
    /// `Stream::buffer_size()` as reported by cpal.
    pub reported_buffer: Option<u32>,
    pub rate: u32,
    pub bpm: f64,
    pub gain: f32,
    pub seconds: f64,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Dist {
    pub min: f64,
    pub mean: f64,
    pub p50: f64,
    pub p99: f64,
    pub p999: f64,
    pub max: f64,
}

#[derive(Clone, Debug)]
pub struct Report {
    pub info: RunInfo,
    pub callbacks: usize,
    pub callbacks_dropped: u64,
    pub frames_total: u64,
    pub frames_min: u32,
    pub frames_median: u32,
    pub frames_max: u32,
    pub nominal_period_us: f64,
    pub xruns_backend: usize,
    /// Non-xrun events from the error callback, by name.
    pub other_errors: Vec<(&'static str, usize)>,
    pub error_events_dropped: u64,
    pub xruns_gap: usize,
    /// Gaps that start more than 5 s into the run (steady state).
    pub xruns_gap_steady: usize,
    /// The first few gaps: (seconds into the run, interval in microseconds).
    pub gap_events: Vec<(f64, f64)>,
    /// Time between consecutive callbacks, microseconds.
    pub interval_us: Dist,
    /// Absolute deviation of that interval from the nominal period.
    pub jitter_us: Dist,
    pub onsets: usize,
    pub onsets_expected: usize,
    pub onsets_dropped: u64,
    pub drift_max: u64,
    pub drift_nonzero: usize,
    /// Frames delivered per CLOCK_MONOTONIC time, after a 2 s warm-up.
    pub clock_ppm_frames: Option<f64>,
    /// The backend's own callback timestamps per CLOCK_MONOTONIC time.
    pub clock_ppm_stream: Option<f64>,
}

/// Nearest-rank percentile of a sorted slice, `p` in 0..=1.
pub fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = (p * sorted.len() as f64).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

pub fn dist(mut v: Vec<f64>) -> Dist {
    if v.is_empty() {
        return Dist::default();
    }
    v.sort_by(f64::total_cmp);
    Dist {
        min: v[0],
        mean: v.iter().sum::<f64>() / v.len() as f64,
        p50: percentile(&v, 0.5),
        p99: percentile(&v, 0.99),
        p999: percentile(&v, 0.999),
        max: v[v.len() - 1],
    }
}

/// Closed-form onset of beat `n` at a constant tempo from sample 0.
pub fn ideal_onset(n: u64, rate: u32, bpm: f64) -> u64 {
    let samples_per_tick = rate as f64 * 60.0 / (bpm * PPQ as f64);
    ((n * PPQ) as f64 * samples_per_tick).round() as u64
}

pub fn analyze(info: RunInfo, rec: &Recorder) -> Report {
    let cbs = rec.callbacks();
    let frames_total: u64 = cbs.iter().map(|c| c.frames as u64).sum();
    let mut sizes: Vec<u32> = cbs.iter().map(|c| c.frames).collect();
    sizes.sort_unstable();
    let frames_median = sizes.get(sizes.len() / 2).copied().unwrap_or(0);
    let nominal_period_us = frames_median as f64 / info.rate as f64 * 1e6;

    let intervals: Vec<f64> = cbs
        .windows(2)
        .map(|w| (w[1].mono_ns - w[0].mono_ns) as f64 / 1e3)
        .collect();
    let gaps: Vec<(f64, f64)> = cbs
        .windows(2)
        .map(|w| {
            (
                w[1].mono_ns as f64 / 1e9,
                (w[1].mono_ns - w[0].mono_ns) as f64 / 1e3,
            )
        })
        .filter(|g| g.1 > 1.5 * nominal_period_us)
        .collect();
    let xruns_gap = gaps.len();
    let xruns_gap_steady = gaps.iter().filter(|g| g.0 > 5.0).count();
    let gap_events: Vec<(f64, f64)> = gaps.into_iter().take(8).collect();
    let jitter: Vec<f64> = intervals
        .iter()
        .map(|i| (i - nominal_period_us).abs())
        .collect();

    let errors = rec.errors();
    let xruns_backend = errors.iter().filter(|e| e.1 == ERR_XRUN).count();
    let mut other_errors: Vec<(&'static str, usize)> = vec![];
    for &(_, k) in errors.iter().filter(|e| e.1 != ERR_XRUN) {
        let name = ERR_NAMES[(k as usize).min(ERR_NAMES.len() - 1)];
        match other_errors.iter_mut().find(|e| e.0 == name) {
            Some(e) => e.1 += 1,
            None => other_errors.push((name, 1)),
        }
    }

    let onsets = rec.onsets();
    let mut onsets_expected = 0;
    while ideal_onset(onsets_expected as u64, info.rate, info.bpm) < frames_total {
        onsets_expected += 1;
    }
    let diffs: Vec<u64> = onsets
        .iter()
        .enumerate()
        .map(|(n, &o)| o.abs_diff(ideal_onset(n as u64, info.rate, info.bpm)))
        .collect();

    // Clock drift between the first callback after warm-up and the last one.
    let warm = cbs.iter().position(|c| c.mono_ns >= 2_000_000_000);
    let (clock_ppm_frames, clock_ppm_stream) = match (warm, cbs.len()) {
        (Some(k), n) if n > k + 1 => {
            let (a, b) = (cbs[k], cbs[n - 1]);
            let wall = (b.mono_ns - a.mono_ns) as f64;
            let frames: u64 = cbs[k..n - 1].iter().map(|c| c.frames as u64).sum();
            let by_frames = (frames as f64 / info.rate as f64 * 1e9 / wall - 1.0) * 1e6;
            let by_stream = (b.stream_ns > a.stream_ns)
                .then(|| ((b.stream_ns - a.stream_ns) as f64 / wall - 1.0) * 1e6);
            (Some(by_frames), by_stream)
        }
        _ => (None, None),
    };

    Report {
        callbacks: cbs.len(),
        callbacks_dropped: rec.callback_overflow(),
        frames_total,
        frames_min: sizes.first().copied().unwrap_or(0),
        frames_median,
        frames_max: sizes.last().copied().unwrap_or(0),
        nominal_period_us,
        xruns_backend,
        other_errors,
        error_events_dropped: rec.error_overflow(),
        xruns_gap,
        xruns_gap_steady,
        gap_events,
        interval_us: dist(intervals),
        jitter_us: dist(jitter),
        onsets: onsets.len(),
        onsets_expected,
        onsets_dropped: rec.onset_overflow(),
        drift_max: diffs.iter().copied().max().unwrap_or(0),
        drift_nonzero: diffs.iter().filter(|&&d| d != 0).count(),
        clock_ppm_frames,
        clock_ppm_stream,
        info,
    }
}

fn opt(v: Option<f64>) -> String {
    v.map_or("na".into(), |v| format!("{v:.1}"))
}

impl Report {
    /// One machine-readable line of `key=value` pairs.
    pub fn result_line(&self) -> String {
        let i = &self.info;
        let other: usize = self.other_errors.iter().map(|e| e.1).sum();
        format!(
            "RESULT host={} buffer_req={} buffer_cpal={} buffer_cb_median={} rate={} bpm={} \
             gain={} seconds={} callbacks={} frames={} xruns_backend={} xruns_gap={} xruns_gap_steady={} \
             err_other={} int_mean_us={:.1} int_p99_us={:.1} int_p999_us={:.1} int_max_us={:.1} \
             jit_mean_us={:.1} jit_p99_us={:.1} jit_p999_us={:.1} jit_max_us={:.1} \
             onsets={} onsets_expected={} grid_drift_max={} grid_drift_nonzero={} \
             clock_ppm_frames={} clock_ppm_stream={} dropped_cb={} dropped_onsets={}",
            i.host.to_lowercase(),
            i.requested_buffer,
            i.reported_buffer.map_or("na".into(), |b| b.to_string()),
            self.frames_median,
            i.rate,
            i.bpm,
            i.gain,
            i.seconds,
            self.callbacks,
            self.frames_total,
            self.xruns_backend,
            self.xruns_gap,
            self.xruns_gap_steady,
            other,
            self.interval_us.mean,
            self.interval_us.p99,
            self.interval_us.p999,
            self.interval_us.max,
            self.jitter_us.mean,
            self.jitter_us.p99,
            self.jitter_us.p999,
            self.jitter_us.max,
            self.onsets,
            self.onsets_expected,
            self.drift_max,
            self.drift_nonzero,
            opt(self.clock_ppm_frames),
            opt(self.clock_ppm_stream),
            self.callbacks_dropped,
            self.onsets_dropped,
        )
    }

    pub fn summary(&self) -> String {
        let i = &self.info;
        let mut s = String::new();
        let mut line = |l: String| {
            s.push_str(&l);
            s.push('\n');
        };
        line(format!("host:            {} ({})", i.host, i.device));
        line(format!(
            "buffer:          requested {}, cpal reports {}, callbacks delivered {} (min {}, max {}) frames",
            i.requested_buffer,
            i.reported_buffer.map_or("n/a".into(), |b| b.to_string()),
            self.frames_median,
            self.frames_min,
            self.frames_max
        ));
        line(format!(
            "sample rate:     {} Hz, {} BPM, gain {}",
            i.rate, i.bpm, i.gain
        ));
        line(format!(
            "callbacks:       {} ({} frames, nominal period {:.1} us{})",
            self.callbacks,
            self.frames_total,
            self.nominal_period_us,
            if self.callbacks_dropped > 0 {
                format!(", {} NOT RECORDED", self.callbacks_dropped)
            } else {
                String::new()
            }
        ));
        line(format!("xruns (backend): {}", self.xruns_backend));
        line(format!(
            "xruns (gap>1.5x period): {} ({} after 5 s)",
            self.xruns_gap, self.xruns_gap_steady
        ));
        for (t, us) in &self.gap_events {
            line(format!("  gap at {t:.3} s: {us:.0} us"));
        }
        for (name, n) in &self.other_errors {
            line(format!("error events:    {n} x {name}"));
        }
        let d = self.interval_us;
        line(format!(
            "callback interval us: min {:.1} mean {:.1} p50 {:.1} p99 {:.1} p99.9 {:.1} max {:.1}",
            d.min, d.mean, d.p50, d.p99, d.p999, d.max
        ));
        let j = self.jitter_us;
        line(format!(
            "jitter |interval-nominal| us: mean {:.1} p99 {:.1} p99.9 {:.1} max {:.1}",
            j.mean, j.p99, j.p999, j.max
        ));
        line(format!(
            "grid drift:      max {} samples over {} onsets ({} off-grid, {} expected)",
            self.drift_max, self.onsets, self.drift_nonzero, self.onsets_expected
        ));
        line(format!(
            "clock vs CLOCK_MONOTONIC (informational): frames {} ppm, backend timestamps {} ppm",
            opt(self.clock_ppm_frames),
            opt(self.clock_ppm_stream)
        ));
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_rank_percentiles() {
        let v: Vec<f64> = (1..=1000).map(|x| x as f64).collect();
        assert_eq!(percentile(&v, 0.5), 500.0);
        assert_eq!(percentile(&v, 0.99), 990.0);
        assert_eq!(percentile(&v, 0.999), 999.0);
        assert_eq!(percentile(&v, 1.0), 1000.0);
        assert_eq!(percentile(&[7.0], 0.999), 7.0);
        assert_eq!(percentile(&[], 0.5), 0.0);
    }

    #[test]
    fn gaps_and_drift_are_counted() {
        let rec = Recorder::new(8, 8, 8);
        // 100-frame callbacks at 1000 Hz: nominal period 100 ms
        for (i, t) in [0u64, 100, 200, 450, 550].iter().enumerate() {
            rec.push_callback(t * 1_000_000, i as u64, 100);
        }
        rec.push_onset(0, true);
        rec.push_onset(25, false);
        rec.push_error(ERR_XRUN);
        rec.push_error(3);
        let info = RunInfo {
            host: "x".into(),
            device: "y".into(),
            requested_buffer: 100,
            reported_buffer: Some(100),
            rate: 1000,
            bpm: 120.0,
            gain: 0.0,
            seconds: 1.0,
        };
        let r = analyze(info, &rec);
        assert_eq!(r.callbacks, 5);
        assert_eq!(r.xruns_gap, 1); // the 250 ms interval
        assert_eq!(r.xruns_backend, 1);
        assert_eq!(r.other_errors, vec![("device_changed", 1)]);
        assert_eq!(r.interval_us.max, 250_000.0);
        // ideal beat 1 at 120 BPM and 1000 Hz is sample 500, not 25
        assert_eq!(r.drift_nonzero, 1);
        assert_eq!(r.drift_max, 475);
        assert_eq!(r.onsets_expected, 1);
    }
}
