// SPDX-License-Identifier: GPL-3.0-or-later
//! Anchor-based transport (SPEC 4.6). Positions are computed from the
//! anchor, never accumulated block by block.

pub const PPQ: u64 = 960;
/// Largest sub-block the engine processes at once (SPEC 4.5).
pub const MAX_BLOCK: usize = 256;

/// `(anchor_sample, anchor_tick, samples_per_tick)` plus a fractional tick
/// part. The absolute sample of tick `T` is
/// `anchor_sample + round((T - anchor_tick - anchor_frac) * samples_per_tick)`.
///
/// `anchor_tick` is signed: a loop wrap moves it back by the loop length,
/// which goes below zero on the first wrap when the anchor sits at tick 0.
#[derive(Clone, Copy, Debug)]
pub struct Transport {
    anchor_sample: u64,
    anchor_tick: i64,
    anchor_frac: f64,
    samples_per_tick: f64,
}

pub fn samples_per_tick(sample_rate: f64, bpm: f64) -> f64 {
    sample_rate * 60.0 / (bpm * PPQ as f64)
}

impl Transport {
    /// `tick` sits at `sample`.
    pub fn at(sample: u64, tick: i64, samples_per_tick: f64) -> Self {
        Transport {
            anchor_sample: sample,
            anchor_tick: tick,
            anchor_frac: 0.0,
            samples_per_tick,
        }
    }

    /// Exact musical position at `sample`, in ticks (fractional).
    pub fn tick_at(&self, sample: u64) -> f64 {
        self.anchor_tick as f64
            + self.anchor_frac
            + (sample as f64 - self.anchor_sample as f64) / self.samples_per_tick
    }

    /// Tick 0 sits at sample 0.
    pub fn new(sample_rate: f64, bpm: f64) -> Self {
        Transport {
            anchor_sample: 0,
            anchor_tick: 0,
            anchor_frac: 0.0,
            samples_per_tick: samples_per_tick(sample_rate, bpm),
        }
    }

    pub fn sample_of_tick(&self, tick: i64) -> u64 {
        self.signed_sample_of_tick(tick).max(0) as u64
    }

    /// As `sample_of_tick` but without clamping at sample 0: a tick before
    /// the transport started (a seek past it) maps to a negative sample.
    pub fn signed_sample_of_tick(&self, tick: i64) -> i64 {
        let ticks = (tick - self.anchor_tick) as f64 - self.anchor_frac;
        let offset = (ticks * self.samples_per_tick).round() as i64;
        self.anchor_sample as i64 + offset
    }

    /// Tempo change at `sample`: the anchor moves to the exact position
    /// there (whole ticks plus a fractional tick) and `samples_per_tick`
    /// is replaced.
    pub fn set_tempo(&mut self, sample: u64, new_samples_per_tick: f64) {
        let since = (sample - self.anchor_sample) as f64 / self.samples_per_tick;
        let total = self.anchor_frac + since;
        let whole = total.floor();
        self.anchor_tick += whole as i64;
        self.anchor_frac = total - whole;
        self.anchor_sample = sample;
        self.samples_per_tick = new_samples_per_tick;
    }

    /// Loop wrap: musical ticks restart `loop_ticks` earlier while the
    /// anchor sample stays put, so the fractional sample position carries
    /// across the wrap and is never rounded per loop.
    pub fn wrap(&mut self, loop_ticks: i64) {
        self.anchor_tick -= loop_ticks;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_tempo_is_closed_form() {
        let t = Transport::new(44100.0, 120.0);
        assert_eq!(t.sample_of_tick(0), 0);
        assert_eq!(t.sample_of_tick(960), 22050);
        assert_eq!(t.sample_of_tick(960 * 3), 66150);
    }

    #[test]
    fn retempo_keeps_the_position() {
        let mut t = Transport::new(48000.0, 120.0);
        // 30000 samples at 120 BPM is 1.25 beats = 1200 ticks
        t.set_tempo(30000, samples_per_tick(48000.0, 60.0));
        assert_eq!(t.sample_of_tick(1200), 30000);
        assert_eq!(t.sample_of_tick(1200 + 960), 30000 + 48000);
        // the fractional tick is kept: a position between ticks
        let mut t = Transport::new(48000.0, 133.33);
        t.set_tempo(12345, samples_per_tick(48000.0, 133.33));
        assert!(t.anchor_frac > 0.0 && t.anchor_frac < 1.0);
    }

    #[test]
    fn wrap_shifts_ticks_not_samples() {
        let mut t = Transport::new(44100.0, 120.0);
        t.wrap(3840);
        // tick 0 of the next loop plays where tick 3840 would have
        assert_eq!(t.sample_of_tick(0), 4 * 22050);
    }
}
