// SPDX-License-Identifier: GPL-3.0-or-later
//! Sampler (SPEC 15.1, 17.2): 16 voices per channel playing a decoded
//! sample with start and end trim, reverse, an ADSR and cubic interpolation.
//!
//! One-shot mode plays to the end and ignores note-off and key. Pitched
//! mode tracks the key from the channel's root key and releases on
//! note-off (by note id). A choke fades every voice out over 1.5 ms. A
//! sample that is missing or not decoded plays silence.

use crate::bass808::CHOKE_FADE_SECONDS;
use crate::samples::SampleData;
use crate::sequencer::{ChannelEvent, ChokeEvent, SeqEvent, channel_events};
use crate::synth::{Env, EnvCoefs, Stage};
use protocol::beats::{SampleMode, SamplerParam};
use protocol::consts::{MIN_GAIN_DB, SYNTH_VOICES};
use protocol::engine::{ChannelSlot, ParamTable, param_index};

/// Continuous sampler values for one sub-block.
#[derive(Clone, Copy, Debug)]
pub struct SamplerCtl {
    start: f64,
    end: f64,
    semitones: f64,
    env: EnvCoefs,
    gain: f32,
    fade_frames: u32,
}

impl SamplerCtl {
    /// Adds a pitch shape's offset (24.2-1).
    pub fn shift_semitones(&mut self, st: f32) {
        self.semitones += st as f64;
    }

    pub fn read(params: &ParamTable, slot: ChannelSlot, sr: f64) -> SamplerCtl {
        let p = |q: SamplerParam| params.get(param_index(slot, q.index()));
        use SamplerParam::*;
        let gain_db = p(GainDb);
        SamplerCtl {
            start: (p(Start) as f64).clamp(0.0, 1.0),
            end: (p(End) as f64).clamp(0.0, 1.0),
            semitones: p(Semitones) as f64 + p(Cents) as f64 / 100.0,
            env: EnvCoefs::new(sr, p(AttackMs), p(DecayMs), p(Sustain), p(ReleaseMs)),
            gain: if (gain_db as f64) <= MIN_GAIN_DB {
                0.0
            } else {
                10f32.powf(gain_db / 20.0)
            },
            fade_frames: (CHOKE_FADE_SECONDS * sr).round().max(1.0) as u32,
        }
    }
}

/// What the compiler knows about a sampler channel.
#[derive(Clone, Debug, PartialEq)]
pub struct SamplerC {
    pub mode: SampleMode,
    pub reverse: bool,
    pub root_key: u8,
    /// `None`: no sample chosen, or not decoded yet. Plays silence.
    pub sample: Option<SampleData>,
}

#[derive(Clone, Copy, Debug)]
struct Voice {
    active: bool,
    held: bool,
    id: u32,
    key: u8,
    vel: f32,
    seq: u64,
    /// Position in frames of the sample.
    pos: f64,
    env: Env,
    fade_left: u32,
    fade_total: u32,
}

impl Voice {
    const IDLE: Voice = Voice {
        active: false,
        held: false,
        id: 0,
        key: 0,
        vel: 0.0,
        seq: 0,
        pos: 0.0,
        env: Env::IDLE,
        fade_left: 0,
        fade_total: 0,
    };
}

/// One sampler channel: 16 voices. Lives in `Runtime`.
#[derive(Clone)]
pub struct Sampler {
    voices: [Voice; SYNTH_VOICES],
    counter: u64,
}

impl Default for Sampler {
    fn default() -> Sampler {
        Sampler::new()
    }
}

/// Catmull-Rom interpolation between `b` and `c` at `t` in 0..1.
#[inline]
fn cubic(a: f32, b: f32, c: f32, d: f32, t: f32) -> f32 {
    let t2 = t * t;
    let t3 = t2 * t;
    0.5 * ((2.0 * b)
        + (-a + c) * t
        + (2.0 * a - 5.0 * b + 4.0 * c - d) * t2
        + (-a + 3.0 * b - 3.0 * c + d) * t3)
}

/// Sample `ch` of the interleaved `data` at frame `i`, clamped to the data.
#[inline]
fn at(data: &[f32], channels: usize, frames: usize, i: i64, ch: usize) -> f32 {
    let i = i.clamp(0, frames as i64 - 1) as usize;
    data[i * channels + ch]
}

impl Sampler {
    pub fn new() -> Sampler {
        Sampler {
            voices: [Voice::IDLE; SYNTH_VOICES],
            counter: 0,
        }
    }

    pub fn reset(&mut self) {
        self.voices = [Voice::IDLE; SYNTH_VOICES];
    }

    pub fn is_active(&self) -> bool {
        self.voices.iter().any(|v| v.active)
    }

    pub fn active_voices(&self) -> usize {
        self.voices.iter().filter(|v| v.active).count()
    }

    /// Start frame of a voice for the trim range, honoring reverse.
    fn start_pos(c: &SamplerC, ctl: &SamplerCtl, frames: usize) -> f64 {
        let (s, e) = (ctl.start * frames as f64, ctl.end * frames as f64);
        if c.reverse { e - 1.0 } else { s }
    }

    pub fn note_on(&mut self, c: &SamplerC, ctl: &SamplerCtl, key: u8, vel: u8, id: u32) {
        let Some(s) = &c.sample else { return };
        let i = match self.voices.iter().position(|v| !v.active) {
            Some(i) => i,
            None => (0..SYNTH_VOICES)
                .min_by_key(|&i| self.voices[i].seq)
                .unwrap_or(0),
        };
        self.counter += 1;
        let mut v = Voice::IDLE;
        v.active = true;
        v.held = true;
        v.id = id;
        v.key = key;
        v.vel = vel as f32 / 127.0;
        v.seq = self.counter;
        v.pos = Sampler::start_pos(c, ctl, s.frames());
        v.env.gate_on();
        self.voices[i] = v;
    }

    /// Releases the voice of note `id` (pitched mode only; a one-shot
    /// plays on).
    pub fn note_off(&mut self, c: &SamplerC, id: u32) {
        if c.mode != SampleMode::Pitched {
            return;
        }
        for v in &mut self.voices {
            if v.active && v.held && v.id == id {
                v.held = false;
                v.env.release();
            }
        }
    }

    pub fn choke(&mut self, ctl: &SamplerCtl) {
        for v in &mut self.voices {
            if v.active && v.fade_left == 0 {
                v.fade_left = ctl.fade_frames;
                v.fade_total = ctl.fade_frames;
            }
        }
    }

    /// Adds `out_l.len()` frames. A mono sample writes `out_l` only.
    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &mut self,
        c: &SamplerC,
        ctl: &SamplerCtl,
        sr: f64,
        slot: u16,
        group: u8,
        events: &[SeqEvent],
        chokes: &[ChokeEvent],
        out_l: &mut [f32],
        out_r: &mut [f32],
    ) {
        let n = out_l.len();
        let mut at_ = 0usize;
        for (offset, ev) in channel_events(slot, group, events, chokes) {
            let off = (offset as usize).min(n);
            if off > at_ {
                self.segment(c, ctl, sr, &mut out_l[at_..off], &mut out_r[at_..off]);
                at_ = off;
            }
            match ev {
                ChannelEvent::Choke => self.choke(ctl),
                ChannelEvent::Note(e) if e.on => self.note_on(c, ctl, e.key, e.vel, e.id),
                ChannelEvent::Note(e) => self.note_off(c, e.id),
            }
        }
        if at_ < n {
            self.segment(c, ctl, sr, &mut out_l[at_..], &mut out_r[at_..]);
        }
    }

    fn segment(
        &mut self,
        c: &SamplerC,
        ctl: &SamplerCtl,
        sr: f64,
        out_l: &mut [f32],
        out_r: &mut [f32],
    ) {
        let Some(s) = &c.sample else {
            // Sample gone after a swap: end the voices quietly.
            for v in &mut self.voices {
                v.active = false;
            }
            return;
        };
        let frames = s.frames();
        let ch = s.channels as usize;
        let data: &[f32] = &s.data;
        let lo = (ctl.start * frames as f64).floor();
        let hi = (ctl.end * frames as f64).ceil().min(frames as f64);
        if frames == 0 || hi - lo < 2.0 {
            for v in &mut self.voices {
                v.active = false;
            }
            return;
        }
        let base = s.rate as f64 / sr;
        let dir = if c.reverse { -1.0 } else { 1.0 };
        for v in self.voices.iter_mut().filter(|v| v.active) {
            let pitch = match c.mode {
                SampleMode::OneShot => ctl.semitones,
                SampleMode::Pitched => v.key as f64 - c.root_key as f64 + ctl.semitones,
            };
            let step = dir * base * (pitch / 12.0).exp2();
            for k in 0..out_l.len() {
                if v.pos < lo || v.pos >= hi {
                    v.active = false;
                    break;
                }
                let a = v.env.next(&ctl.env);
                if v.env.stage == Stage::Idle {
                    v.active = false;
                    break;
                }
                let i = v.pos.floor() as i64;
                let t = (v.pos - i as f64) as f32;
                let mut g = a * v.vel * ctl.gain;
                if v.fade_left > 0 {
                    g *= v.fade_left as f32 / v.fade_total as f32;
                    v.fade_left -= 1;
                    if v.fade_left == 0 {
                        v.active = false;
                    }
                }
                let l = cubic(
                    at(data, ch, frames, i - 1, 0),
                    at(data, ch, frames, i, 0),
                    at(data, ch, frames, i + 1, 0),
                    at(data, ch, frames, i + 2, 0),
                    t,
                );
                out_l[k] += l * g;
                if ch == 2 {
                    let r = cubic(
                        at(data, ch, frames, i - 1, 1),
                        at(data, ch, frames, i, 1),
                        at(data, ch, frames, i + 1, 1),
                        at(data, ch, frames, i + 2, 1),
                        t,
                    );
                    out_r[k] += r * g;
                }
                v.pos += step;
                if !v.active {
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::beats::SamplerParams;

    const SR: f64 = 48000.0;
    const S0: ChannelSlot = ChannelSlot(0);

    fn ctl_with(f: impl Fn(&mut SamplerParams)) -> SamplerCtl {
        let t = ParamTable::new();
        let mut p = SamplerParams::default();
        f(&mut p);
        crate::write_sampler_params(&t, S0, &p);
        SamplerCtl::read(&t, S0, SR)
    }

    fn ramp(frames: usize) -> SampleData {
        SampleData::from_vec(1, 48000, (0..frames).map(|i| i as f32 / 1000.0).collect())
    }

    fn chan(mode: SampleMode, reverse: bool, s: SampleData) -> SamplerC {
        SamplerC {
            mode,
            reverse,
            root_key: 60,
            sample: Some(s),
        }
    }

    fn ev(offset: u32, key: u8, on: bool, id: u32) -> SeqEvent {
        SeqEvent {
            offset,
            slot: 0,
            key,
            vel: 127,
            on,
            id,
        }
    }

    fn run(
        s: &mut Sampler,
        c: &SamplerC,
        ctl: &SamplerCtl,
        events: &[SeqEvent],
        n: usize,
    ) -> Vec<f32> {
        let mut l = vec![0.0; n];
        let mut r = vec![0.0; n];
        s.render(c, ctl, SR, 0, 0, events, &[], &mut l, &mut r);
        l
    }

    #[test]
    fn one_shot_at_root_pitch_plays_the_samples_exactly() {
        let c = chan(SampleMode::OneShot, false, ramp(100));
        let ctl = ctl_with(|_| {});
        let mut s = Sampler::new();
        let out = run(&mut s, &c, &ctl, &[ev(10, 99, true, 1)], 200);
        assert!(out[..10].iter().all(|x| *x == 0.0));
        for i in 0..100 {
            // vel 127 and gain 0 dB: unity.
            assert!((out[10 + i] - i as f32 / 1000.0).abs() < 1e-6, "{i}");
        }
        assert!(out[110..].iter().all(|x| *x == 0.0));
        assert!(!s.is_active(), "one-shot ends with the sample");
    }

    #[test]
    fn one_shot_ignores_note_off_and_key() {
        let c = chan(SampleMode::OneShot, false, ramp(1000));
        let ctl = ctl_with(|_| {});
        let mut s = Sampler::new();
        let out = run(
            &mut s,
            &c,
            &ctl,
            &[ev(0, 80, true, 1), ev(20, 80, false, 1)],
            100,
        );
        assert!((out[50] - 0.05).abs() < 1e-6);
        assert_eq!(s.active_voices(), 1);
    }

    #[test]
    fn pitched_mode_follows_the_key_in_semitones() {
        let c = chan(SampleMode::Pitched, false, ramp(2000));
        let ctl = ctl_with(|_| {});
        let mut s = Sampler::new();
        let out = run(&mut s, &c, &ctl, &[ev(0, 72, true, 1)], 100);
        // An octave up reads two frames per output frame; the ramp is
        // linear, so cubic interpolation is exact.
        assert!((out[10] - 20.0 / 1000.0).abs() < 1e-5, "{}", out[10]);
        let mut s2 = Sampler::new();
        let out = run(&mut s2, &c, &ctl, &[ev(0, 48, true, 1)], 100);
        assert!((out[40] - 20.0 / 1000.0).abs() < 1e-5, "{}", out[40]);
    }

    #[test]
    fn pitched_note_off_releases_by_id_and_semitones_cents_apply() {
        let c = chan(SampleMode::Pitched, false, ramp(48000));
        let ctl = ctl_with(|p| p.release_ms = 5.0);
        let mut s = Sampler::new();
        run(
            &mut s,
            &c,
            &ctl,
            &[ev(0, 60, true, 1), ev(0, 60, true, 2)],
            100,
        );
        assert_eq!(s.active_voices(), 2);
        run(&mut s, &c, &ctl, &[ev(0, 60, false, 1)], 100);
        assert!(s.voices.iter().any(|v| v.active && !v.held));
        run(&mut s, &c, &ctl, &[], 2000);
        assert_eq!(s.active_voices(), 1, "only note 1 ended");
        let st = ctl_with(|p| {
            p.semitones = 12.0;
            p.cents = 0.0;
        });
        let mut s2 = Sampler::new();
        let out = run(&mut s2, &c, &st, &[ev(0, 60, true, 1)], 50);
        assert!((out[10] - 20.0 / 1000.0).abs() < 1e-5);
    }

    #[test]
    fn trim_and_reverse() {
        let ctl = ctl_with(|p| {
            p.start = 0.25;
            p.end = 0.75;
        });
        let c = chan(SampleMode::OneShot, false, ramp(400));
        let mut s = Sampler::new();
        let out = run(&mut s, &c, &ctl, &[ev(0, 60, true, 1)], 300);
        assert!((out[0] - 0.1).abs() < 1e-6, "starts at frame 100");
        assert!((out[199] - 0.299).abs() < 1e-6, "ends at frame 299");
        assert!(out[200..].iter().all(|x| *x == 0.0));

        let c = chan(SampleMode::OneShot, true, ramp(400));
        let mut s = Sampler::new();
        let out = run(&mut s, &c, &ctl, &[ev(0, 60, true, 1)], 300);
        assert!((out[0] - 0.299).abs() < 1e-6, "reverse starts at the end");
        assert!((out[199] - 0.100).abs() < 1e-6);
        assert!(out[200..].iter().all(|x| *x == 0.0));
    }

    #[test]
    fn adsr_shapes_the_voice() {
        let ones = SampleData::from_vec(1, 48000, vec![1.0; 48000]);
        let c = chan(SampleMode::Pitched, false, ones);
        let ctl = ctl_with(|p| {
            p.attack_ms = 10.0;
            p.decay_ms = 0.0;
            p.sustain = 0.5;
        });
        let mut s = Sampler::new();
        let out = run(&mut s, &c, &ctl, &[ev(0, 60, true, 1)], 2000);
        assert!(out[0] < 0.01);
        assert!((out[240] - 0.5).abs() < 0.02, "half way up the attack");
        assert!((out[1999] - 0.5).abs() < 1e-3, "sustain");
    }

    #[test]
    fn stereo_samples_keep_both_channels() {
        let d = SampleData::from_vec(
            2,
            48000,
            vec![0.5, -0.25, 0.5, -0.25, 0.5, -0.25, 0.5, -0.25],
        );
        let c = chan(SampleMode::OneShot, false, d);
        let ctl = ctl_with(|_| {});
        let mut s = Sampler::new();
        let mut l = vec![0.0; 4];
        let mut r = vec![0.0; 4];
        s.render(
            &c,
            &ctl,
            SR,
            0,
            0,
            &[ev(0, 60, true, 1)],
            &[],
            &mut l,
            &mut r,
        );
        assert_eq!(l[1], 0.5);
        assert_eq!(r[1], -0.25);
    }

    #[test]
    fn stream_rate_other_than_the_sample_rate_is_compensated() {
        // The data is at 24 kHz and the stream at 48 kHz: half a frame
        // per output frame.
        let d = SampleData::from_vec(1, 24000, (0..100).map(|i| i as f32 / 1000.0).collect());
        let c = chan(SampleMode::OneShot, false, d);
        let ctl = ctl_with(|_| {});
        let mut s = Sampler::new();
        let out = run(&mut s, &c, &ctl, &[ev(0, 60, true, 1)], 100);
        assert!((out[20] - 0.010).abs() < 1e-5);
    }

    #[test]
    fn missing_sample_is_silence_and_sixteen_voices_steal_the_oldest() {
        let none = SamplerC {
            mode: SampleMode::OneShot,
            reverse: false,
            root_key: 60,
            sample: None,
        };
        let ctl = ctl_with(|_| {});
        let mut s = Sampler::new();
        let out = run(&mut s, &none, &ctl, &[ev(0, 60, true, 1)], 64);
        assert!(out.iter().all(|x| *x == 0.0));
        assert!(!s.is_active());

        let c = chan(SampleMode::OneShot, false, ramp(48000));
        let mut evs: Vec<SeqEvent> = (0..17).map(|i| ev(i, 60, true, i + 1)).collect();
        evs.sort_by_key(|e| e.offset);
        run(&mut s, &c, &ctl, &evs, 64);
        assert_eq!(s.active_voices(), 16);
        assert!(s.voices.iter().all(|v| v.id != 1), "oldest was stolen");
    }
}
