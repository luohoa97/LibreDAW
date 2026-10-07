// SPDX-License-Identifier: GPL-3.0-or-later
//! Microphone capture for the Hum flow (SPEC 21.2). The input callback owns
//! a `Feeder` that copies frames into a preallocated SPSC ring: no
//! allocation, lock or syscall. A worker thread drains the ring into a
//! growing `Vec<f32>` (capped at `MAX_SECONDS`) and hands it back on stop.
//! The stream itself is opened by `Engine::start_capture`, and only then.

use rtrb::{Consumer, Producer, RingBuffer};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering::*};
use std::thread::JoinHandle;
use std::time::Duration;

/// Longest recording kept. Later samples are dropped.
pub const MAX_SECONDS: usize = 120;

/// What a finished capture hands back: interleaved samples as recorded.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Captured {
    pub rate: u32,
    pub channels: u16,
    pub samples: Vec<f32>,
}

impl Captured {
    pub fn seconds(&self) -> f64 {
        if self.rate == 0 || self.channels == 0 {
            return 0.0;
        }
        self.samples.len() as f64 / (self.rate as f64 * self.channels as f64)
    }

    /// Channels averaged to one.
    pub fn mono(&self) -> Vec<f32> {
        let c = self.channels.max(1) as usize;
        if c == 1 {
            return self.samples.clone();
        }
        self.samples
            .chunks_exact(c)
            .map(|f| f.iter().sum::<f32>() / c as f32)
            .collect()
    }
}

struct Meter {
    /// Peak of the latest callback, `f32` bits.
    peak: AtomicU32,
    /// Samples the ring had no room for.
    dropped: AtomicU64,
}

/// The audio-thread end.
pub struct Feeder {
    ring: Producer<f32>,
    meter: Arc<Meter>,
}

impl Feeder {
    /// Audio thread. Copies `data` into the ring; samples that do not fit are
    /// counted and dropped.
    pub fn feed(&mut self, data: &[f32]) {
        let mut peak = 0.0f32;
        let mut lost = 0u64;
        for &s in data {
            peak = peak.max(s.abs());
            if self.ring.push(s).is_err() {
                lost += 1;
            }
        }
        self.meter.peak.store(peak.to_bits(), Relaxed);
        if lost > 0 {
            self.meter.dropped.fetch_add(lost, Relaxed);
        }
    }
}

/// The control-thread end: the worker, the meter and the result.
pub struct Capture {
    rate: u32,
    channels: u16,
    stop: Arc<AtomicBool>,
    meter: Arc<Meter>,
    join: Option<JoinHandle<Vec<f32>>>,
}

impl Capture {
    pub fn new(rate: u32, channels: u16) -> (Capture, Feeder) {
        let cap = (rate as usize * channels as usize).max(1);
        let (prod, cons) = RingBuffer::<f32>::new(cap);
        let meter = Arc::new(Meter {
            peak: AtomicU32::new(0),
            dropped: AtomicU64::new(0),
        });
        let stop = Arc::new(AtomicBool::new(false));
        let limit = MAX_SECONDS * rate as usize * channels as usize;
        let join = {
            let stop = stop.clone();
            std::thread::Builder::new()
                .name("libredaw-capture".into())
                .spawn(move || drain(cons, stop, limit))
                .expect("spawn capture thread")
        };
        (
            Capture {
                rate,
                channels,
                stop,
                meter: meter.clone(),
                join: Some(join),
            },
            Feeder { ring: prod, meter },
        )
    }

    /// Peak of the latest callback, 0..=1 and beyond if clipping.
    pub fn level(&self) -> f32 {
        f32::from_bits(self.meter.peak.load(Relaxed))
    }

    /// Samples lost because the worker fell behind.
    pub fn dropped(&self) -> u64 {
        self.meter.dropped.load(Relaxed)
    }

    /// Stops the worker after it has drained what is left, and returns the
    /// recording. Drop the feeder (the stream) first.
    pub fn finish(mut self) -> Captured {
        self.stop.store(true, Release);
        let samples = self
            .join
            .take()
            .and_then(|j| j.join().ok())
            .unwrap_or_default();
        Captured {
            rate: self.rate,
            channels: self.channels,
            samples,
        }
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.stop.store(true, Release);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

fn drain(mut cons: Consumer<f32>, stop: Arc<AtomicBool>, limit: usize) -> Vec<f32> {
    let mut out: Vec<f32> = Vec::with_capacity(limit.min(48_000 * 2 * 30));
    loop {
        let done = stop.load(Acquire);
        while let Ok(s) = cons.pop() {
            if out.len() < limit {
                out.push(s);
            }
        }
        if done {
            return out;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rt::{RtGuard, rt_events};

    #[test]
    fn feeds_and_returns_in_order() {
        let (cap, mut feeder) = Capture::new(8000, 1);
        let a: Vec<f32> = (0..100).map(|i| i as f32 / 100.0).collect();
        feeder.feed(&a[..40]);
        feeder.feed(&a[40..]);
        drop(feeder);
        let got = cap.finish();
        assert_eq!(got.samples, a);
        assert_eq!((got.rate, got.channels), (8000, 1));
    }

    #[test]
    fn feeder_does_not_allocate() {
        let (cap, mut feeder) = Capture::new(8000, 2);
        let block = vec![0.25f32; 512];
        let before;
        {
            let _g = RtGuard::enter_counting();
            before = rt_events();
            for _ in 0..4 {
                feeder.feed(&block);
            }
        }
        assert_eq!(rt_events(), before);
        drop(feeder);
        assert_eq!(cap.finish().samples.len(), 2048);
    }

    #[test]
    fn level_follows_latest_block() {
        let (cap, mut feeder) = Capture::new(8000, 1);
        feeder.feed(&[0.1, -0.6, 0.2]);
        assert!((cap.level() - 0.6).abs() < 1e-6);
        feeder.feed(&[0.0; 4]);
        assert_eq!(cap.level(), 0.0);
    }

    #[test]
    fn overflow_is_counted_not_blocking() {
        let (cap, mut feeder) = Capture::new(10, 1);
        // The worker may drain meanwhile; push far more than the ring holds
        // in one call so some must be lost or kept, never block.
        feeder.feed(&vec![0.5f32; 100]);
        drop(feeder);
        let lost = cap.dropped();
        let got = cap.finish();
        assert_eq!(got.samples.len() as u64 + lost, 100);
    }

    #[test]
    fn mono_averages_channels() {
        let c = Captured {
            rate: 1,
            channels: 2,
            samples: vec![1.0, 0.0, 0.5, 0.5],
        };
        assert_eq!(c.mono(), vec![0.5, 0.5]);
        assert_eq!(c.seconds(), 2.0);
    }
}
