// SPDX-License-Identifier: GPL-3.0-or-later
//! Metronome: 4/4 click on the anchor-based transport, split into sub-blocks
//! of at most `MAX_BLOCK` frames, plus the per-callback entry point that the
//! live stream and the offline tests share.

use crate::recorder::Recorder;
use crate::rt::{RtGuard, enter_rt_fp_mode, restore_fp_mode};
use crate::transport::{MAX_BLOCK, PPQ, Transport, samples_per_tick};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering::Relaxed};

pub const BEATS_PER_BAR: i64 = 4;
pub const MIN_BPM: f64 = 20.0;
pub const MAX_BPM: f64 = 999.0;

/// Values the control thread changes while the stream runs (SPEC 4.3).
/// Tempo is kept as f64 bits: an f32 would turn 133.33 into 133.330002 and
/// move the grid by about a sample per minute.
pub struct Controls {
    tempo_bits: AtomicU64,
    gain_bits: AtomicU32,
}

impl Controls {
    pub fn new(bpm: f64, gain: f32) -> Self {
        Controls {
            tempo_bits: AtomicU64::new(bpm.clamp(MIN_BPM, MAX_BPM).to_bits()),
            gain_bits: AtomicU32::new(gain.to_bits()),
        }
    }
    pub fn set_tempo(&self, bpm: f64) {
        self.tempo_bits
            .store(bpm.clamp(MIN_BPM, MAX_BPM).to_bits(), Relaxed);
    }
    pub fn set_gain(&self, gain: f32) {
        self.gain_bits.store(gain.to_bits(), Relaxed);
    }
}

/// A short sine burst with a squared linear fade-out.
#[derive(Clone, Copy)]
struct Click {
    active: bool,
    age: u32,
    len: u32,
    w: f32,
    amp: f32,
}

impl Click {
    const IDLE: Click = Click {
        active: false,
        age: 0,
        len: 0,
        w: 0.0,
        amp: 0.0,
    };

    fn start(sample_rate: f64, accent: bool) -> Click {
        let (freq, amp) = if accent { (1500.0, 0.9) } else { (1000.0, 0.6) };
        Click {
            active: true,
            age: 0,
            len: (sample_rate * 0.020) as u32,
            w: (std::f64::consts::TAU * freq / sample_rate) as f32,
            amp,
        }
    }

    /// Writes `out.len() / channels` frames, every channel the same.
    fn render(&mut self, out: &mut [f32], channels: usize, gain: f32) {
        if !self.active {
            out.fill(0.0);
            return;
        }
        for frame in out.chunks_exact_mut(channels) {
            let mut x = 0.0;
            if self.active {
                let fade = 1.0 - self.age as f32 / self.len as f32;
                x = self.amp * fade * fade * (self.w * (self.age + 1) as f32).sin() * gain;
                self.age += 1;
                self.active = self.age < self.len;
            }
            frame.fill(x);
        }
    }
}

pub struct Metronome {
    sample_rate: f64,
    channels: usize,
    transport: Transport,
    controls: Arc<Controls>,
    recorder: Arc<Recorder>,
    /// Loop length in ticks, starting at tick 0. `None`: play straight on.
    loop_ticks: Option<i64>,
    loops: u64,
    tempo_bits: u64,
    /// Musical tick of the next beat to emit, in the current anchor frame.
    next_tick: i64,
    next_onset: u64,
    /// Absolute stream sample at the start of the next sub-block.
    pos: u64,
    click: Click,
}

impl Metronome {
    /// `loop_ticks`, if given, must be a whole number of beats.
    pub fn new(
        sample_rate: u32,
        channels: usize,
        controls: Arc<Controls>,
        recorder: Arc<Recorder>,
        loop_ticks: Option<u64>,
    ) -> Self {
        assert!(channels >= 1);
        if let Some(l) = loop_ticks {
            assert!(l >= PPQ && l % PPQ == 0, "loop must be whole beats");
        }
        let tempo_bits = controls.tempo_bits.load(Relaxed);
        let sample_rate = sample_rate as f64;
        Metronome {
            sample_rate,
            channels,
            transport: Transport::new(sample_rate, f64::from_bits(tempo_bits)),
            controls,
            recorder,
            loop_ticks: loop_ticks.map(|l| l as i64),
            loops: 0,
            tempo_bits,
            next_tick: 0,
            next_onset: 0,
            pos: 0,
            click: Click::IDLE,
        }
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    /// How many times the loop has wrapped (the wrap is applied when the
    /// last beat of a loop is scheduled).
    pub fn loops_completed(&self) -> u64 {
        self.loops
    }

    /// Renders interleaved frames, any number, in sub-blocks of at most
    /// `MAX_BLOCK`.
    pub fn render(&mut self, out: &mut [f32]) {
        for block in out.chunks_mut(MAX_BLOCK * self.channels) {
            self.sub_block(block);
        }
    }

    fn sub_block(&mut self, out: &mut [f32]) {
        let n = out.len() / self.channels;
        // Control values are read once per sub-block.
        let bits = self.controls.tempo_bits.load(Relaxed);
        if bits != self.tempo_bits {
            self.tempo_bits = bits;
            let spt = samples_per_tick(self.sample_rate, f64::from_bits(bits));
            self.transport.set_tempo(self.pos, spt);
            // The pending beat has not sounded yet: place it with the new
            // tempo, never before this sub-block.
            self.next_onset = self.transport.sample_of_tick(self.next_tick).max(self.pos);
        }
        let gain = f32::from_bits(self.controls.gain_bits.load(Relaxed));

        let mut i = 0;
        while i < n {
            let at = self.pos + i as u64;
            let until = self.next_onset.saturating_sub(at);
            if until == 0 {
                self.trigger(at);
                continue;
            }
            let seg = (n - i).min(until.min(n as u64) as usize);
            self.click.render(
                &mut out[i * self.channels..(i + seg) * self.channels],
                self.channels,
                gain,
            );
            i += seg;
        }
        self.pos += n as u64;
    }

    /// Starts the click of the pending beat at `at` and schedules the next.
    fn trigger(&mut self, at: u64) {
        let accent = (self.next_tick / PPQ as i64) % BEATS_PER_BAR == 0;
        self.click = Click::start(self.sample_rate, accent);
        self.recorder.push_onset(at, accent);

        self.next_tick += PPQ as i64;
        if let Some(l) = self.loop_ticks
            && self.next_tick >= l
        {
            self.next_tick -= l;
            self.transport.wrap(l);
            self.loops += 1;
        }
        self.next_onset = self.transport.sample_of_tick(self.next_tick).max(at + 1);
    }
}

/// Everything one audio callback does. The live stream and the offline tests
/// both call `process`, so the tests cover the real callback path.
pub struct CallbackState {
    metronome: Metronome,
    recorder: Arc<Recorder>,
    sched_probed: bool,
}

impl CallbackState {
    pub fn new(metronome: Metronome, recorder: Arc<Recorder>) -> Self {
        CallbackState {
            metronome,
            recorder,
            sched_probed: false,
        }
    }

    /// `data` is interleaved; `stream_ns` is the backend's timestamp for this
    /// callback (0 if none). No allocation, locks or printing in here.
    pub fn process(&mut self, data: &mut [f32], stream_ns: u64) {
        let fp = enter_rt_fp_mode();
        let _rt = RtGuard::enter();
        let frames = (data.len() / self.metronome.channels()) as u32;
        self.recorder
            .push_callback(self.recorder.elapsed_ns(), stream_ns, frames);
        if !self.sched_probed {
            // Once, measurement only: two scheduler queries on the callback thread.
            self.sched_probed = true;
            self.recorder.set_sched(crate::rt::thread_sched());
        }
        self.metronome.render(data);
        restore_fp_mode(fp);
    }
}
