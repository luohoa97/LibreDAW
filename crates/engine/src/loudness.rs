// SPDX-License-Identifier: GPL-3.0-or-later
//! Integrated loudness of Main Output while it plays (SPEC 24.2-4): ITU-R
//! BS.1770 K-weighting on the audio thread, one mean-square value per 100 ms
//! hop in a ring of atomics. The window thread reads the last 10 s and
//! applies the 400 ms block gating. The audio side does no allocation, takes
//! no lock and makes no system call: two biquads per channel and a store
//! per hop.

use std::f64::consts::PI;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

/// Hops kept: 10 seconds.
pub const RING_HOPS: usize = 100;
const HOP_SECONDS: f64 = 0.1;
const ABSOLUTE_GATE_LUFS: f64 = -70.0;
const RELATIVE_GATE_LU: f64 = -10.0;

/// The hops of the last 10 s, shared with the window thread.
pub struct LoudnessRing {
    hops: Box<[AtomicU64]>,
    count: AtomicU64,
}

impl Default for LoudnessRing {
    fn default() -> LoudnessRing {
        LoudnessRing::new()
    }
}

impl LoudnessRing {
    pub fn new() -> LoudnessRing {
        LoudnessRing {
            hops: (0..RING_HOPS).map(|_| AtomicU64::new(0)).collect(),
            count: AtomicU64::new(0),
        }
    }

    fn push(&self, z: f64) {
        let n = self.count.load(Relaxed);
        self.hops[n as usize % RING_HOPS].store(z.to_bits(), Relaxed);
        self.count
            .store(n + 1, std::sync::atomic::Ordering::Release);
    }

    /// Mean square of each hop, both channels summed, oldest first.
    pub fn hops(&self) -> Vec<f64> {
        let n = self.count.load(std::sync::atomic::Ordering::Acquire);
        let have = (n as usize).min(RING_HOPS);
        (n - have as u64..n)
            .map(|i| f64::from_bits(self.hops[i as usize % RING_HOPS].load(Relaxed)))
            .collect()
    }

    /// Forgets everything (a new play from the start).
    pub fn clear(&self) {
        self.count.store(0, Relaxed);
    }

    /// Integrated loudness of the last 10 s in LUFS, or `None` before there
    /// is a 400 ms block above the gate.
    pub fn lufs(&self) -> Option<f64> {
        lufs_of_hops(&self.hops())
    }
}

fn block_loudness(z: f64) -> f64 {
    if z <= 0.0 {
        f64::NEG_INFINITY
    } else {
        -0.691 + 10.0 * z.log10()
    }
}

/// BS.1770-4 gating over blocks of four hops, stepping one hop (75 percent
/// overlap).
pub fn lufs_of_hops(hops: &[f64]) -> Option<f64> {
    let blocks: Vec<f64> = hops
        .windows(4)
        .map(|w| w.iter().sum::<f64>() / 4.0)
        .filter(|z| block_loudness(*z) > ABSOLUTE_GATE_LUFS)
        .collect();
    if blocks.is_empty() {
        return None;
    }
    let mean = blocks.iter().sum::<f64>() / blocks.len() as f64;
    let gate = block_loudness(mean) + RELATIVE_GATE_LU;
    let kept: Vec<f64> = blocks
        .into_iter()
        .filter(|z| block_loudness(*z) > gate)
        .collect();
    if kept.is_empty() {
        return None;
    }
    Some(block_loudness(kept.iter().sum::<f64>() / kept.len() as f64))
}

#[derive(Clone, Copy)]
struct Biquad {
    b: [f64; 3],
    a: [f64; 2],
    x: [f64; 2],
    y: [f64; 2],
}

impl Biquad {
    #[inline]
    fn process(&mut self, x: f64) -> f64 {
        let y = self.b[0] * x + self.b[1] * self.x[0] + self.b[2] * self.x[1]
            - self.a[0] * self.y[0]
            - self.a[1] * self.y[1];
        self.x = [x, self.x[0]];
        self.y = [y, self.y[0]];
        y
    }
}

/// The two K-weighting stages for `rate`, derived from the analog
/// prototype as in BS.1770-4.
fn k_weighting(rate: f64) -> [Biquad; 2] {
    let (f0, gain_db, q) = (1681.974450955533, 3.999843853973347, 0.7071752369554196);
    let k = (PI * f0 / rate).tan();
    let vh = 10f64.powf(gain_db / 20.0);
    let vb = vh.powf(0.4996667741545416);
    let a0 = 1.0 + k / q + k * k;
    let shelf = Biquad {
        b: [
            (vh + vb * k / q + k * k) / a0,
            2.0 * (k * k - vh) / a0,
            (vh - vb * k / q + k * k) / a0,
        ],
        a: [2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
        x: [0.0; 2],
        y: [0.0; 2],
    };
    let (f0, q) = (38.13547087602444, 0.5003270373238773);
    let k = (PI * f0 / rate).tan();
    let a0 = 1.0 + k / q + k * k;
    let rlb = Biquad {
        b: [1.0, -2.0, 1.0],
        a: [2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
        x: [0.0; 2],
        y: [0.0; 2],
    };
    [shelf, rlb]
}

/// The audio-thread side: filters and the running hop.
pub struct LoudnessMeter {
    ring: Arc<LoudnessRing>,
    filters: [[Biquad; 2]; 2],
    hop_len: usize,
    pos: usize,
    acc: f64,
}

impl LoudnessMeter {
    pub fn new(sample_rate: f64, ring: Arc<LoudnessRing>) -> LoudnessMeter {
        let f = k_weighting(sample_rate);
        LoudnessMeter {
            ring,
            filters: [f, f],
            hop_len: ((HOP_SECONDS * sample_rate).round() as usize).max(1),
            pos: 0,
            acc: 0.0,
        }
    }

    /// Feeds one block of the output.
    pub fn process(&mut self, l: &[f32], r: &[f32]) {
        for i in 0..l.len().min(r.len()) {
            for (ch, x) in [l[i], r[i]].into_iter().enumerate() {
                let f = &mut self.filters[ch];
                let s = f[0].process(x as f64);
                let y = f[1].process(s);
                self.acc += y * y;
            }
            self.pos += 1;
            if self.pos == self.hop_len {
                self.ring.push(self.acc / self.hop_len as f64);
                self.pos = 0;
                self.acc = 0.0;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_scale_1khz_stereo_sine_reads_zero_lufs() {
        // BS.1770: a 0 dBFS sine on one channel is -3.01 LUFS, so on both it
        // is 0 LUFS (K-weighting is nearly flat at 1 kHz).
        let ring = Arc::new(LoudnessRing::new());
        let mut m = LoudnessMeter::new(48000.0, ring.clone());
        let x: Vec<f32> = (0..48000 * 5)
            .map(|i| (2.0 * PI * 1000.0 * i as f64 / 48000.0).sin() as f32)
            .collect();
        m.process(&x, &x);
        let l = ring.lufs().unwrap();
        assert!(l.abs() < 0.1, "{l}");
    }

    #[test]
    fn silence_reads_nothing_and_the_ring_forgets_after_ten_seconds() {
        let ring = Arc::new(LoudnessRing::new());
        let mut m = LoudnessMeter::new(48000.0, ring.clone());
        let z = vec![0.0f32; 48000];
        m.process(&z, &z);
        assert_eq!(ring.lufs(), None);
        let loud = vec![0.5f32; 48000];
        m.process(&loud, &loud);
        assert!(ring.lufs().is_some());
        for _ in 0..11 {
            m.process(&z, &z);
        }
        assert_eq!(ring.hops().len(), RING_HOPS);
        assert_eq!(ring.lufs(), None);
    }
}
