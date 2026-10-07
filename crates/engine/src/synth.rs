// SPDX-License-Identifier: GPL-3.0-or-later
//! Built-in synth (SPEC 8): 16 voices, oldest-voice stealing, two polyBLEP
//! oscillators, a state-variable lowpass, amp and filter ADSR. All DSP is
//! per sample, so output does not depend on how a callback is split.

use crate::sequencer::SeqEvent;
use protocol::consts::{MIN_GAIN_DB, SYNTH_VOICES};
use protocol::engine::ParamTable;
use protocol::engine::{ChannelSlot, param_index};
use protocol::model::{SynthParam, Wave};

const TAU: f64 = std::f64::consts::TAU;
/// Natural log of 1000: envelope segments reach -60 dB in their time.
const LN_1000: f64 = 6.907_755_278_982_137;
/// Feedback-path offset, a portable backstop next to FTZ/DAZ (SPEC 3.2).
const DC: f32 = 1e-18;

/// Envelope coefficients for one ADSR, derived once per sub-block.
#[derive(Clone, Copy, Debug, Default)]
pub struct EnvCoefs {
    attack_step: f32,
    decay_coef: f32,
    sustain: f32,
    release_coef: f32,
}

impl EnvCoefs {
    pub(crate) fn new(
        sr: f64,
        attack_ms: f32,
        decay_ms: f32,
        sustain: f32,
        release_ms: f32,
    ) -> EnvCoefs {
        let coef = |ms: f32| -> f32 {
            if ms <= 0.0 {
                0.0
            } else {
                (-LN_1000 / (ms as f64 * 0.001 * sr)).exp() as f32
            }
        };
        EnvCoefs {
            attack_step: if attack_ms <= 0.0 {
                1.0
            } else {
                (1.0 / (attack_ms as f64 * 0.001 * sr)) as f32
            },
            decay_coef: coef(decay_ms),
            sustain: sustain.clamp(0.0, 1.0),
            release_coef: coef(release_ms),
        }
    }
}

/// All continuous synth values for one sub-block, read from the `ParamTable`.
#[derive(Clone, Copy, Debug)]
pub struct SynthCtl {
    semis1: f32,
    cents1: f32,
    semis2: f32,
    cents2: f32,
    mix: f32,
    cutoff: f32,
    res: f32,
    env_oct: f32,
    amp: EnvCoefs,
    filt: EnvCoefs,
    gain: f32,
}

impl SynthCtl {
    pub fn read(params: &ParamTable, slot: ChannelSlot, sr: f64) -> SynthCtl {
        let p = |q: SynthParam| params.get(param_index(slot, q.index()));
        use SynthParam::*;
        let gain_db = p(GainDb);
        SynthCtl {
            semis1: p(Osc1Semitones),
            cents1: p(Osc1Cents),
            semis2: p(Osc2Semitones),
            cents2: p(Osc2Cents),
            mix: p(OscMix).clamp(0.0, 1.0),
            cutoff: p(CutoffHz).clamp(20.0, 20000.0),
            res: p(Resonance).clamp(0.0, 1.0),
            env_oct: p(FilterEnvOctaves),
            amp: EnvCoefs::new(
                sr,
                p(AmpAttackMs),
                p(AmpDecayMs),
                p(AmpSustain),
                p(AmpReleaseMs),
            ),
            filt: EnvCoefs::new(
                sr,
                p(FilterAttackMs),
                p(FilterDecayMs),
                p(FilterSustain),
                p(FilterReleaseMs),
            ),
            gain: if (gain_db as f64) <= MIN_GAIN_DB {
                0.0
            } else {
                10f32.powf(gain_db / 20.0)
            },
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Stage {
    Idle,
    Attack,
    Decay,
    Sustain,
    Release,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Env {
    pub(crate) stage: Stage,
    pub(crate) v: f32,
}

impl Env {
    pub(crate) const IDLE: Env = Env {
        stage: Stage::Idle,
        v: 0.0,
    };

    pub(crate) fn gate_on(&mut self) {
        self.stage = Stage::Attack;
    }

    pub(crate) fn release(&mut self) {
        if self.stage != Stage::Idle {
            self.stage = Stage::Release;
        }
    }

    #[inline]
    pub(crate) fn next(&mut self, c: &EnvCoefs) -> f32 {
        match self.stage {
            Stage::Idle => {}
            Stage::Attack => {
                self.v += c.attack_step;
                if self.v >= 1.0 {
                    self.v = 1.0;
                    self.stage = Stage::Decay;
                }
            }
            Stage::Decay => {
                self.v = c.sustain + (self.v - c.sustain) * c.decay_coef + DC;
                if (self.v - c.sustain).abs() < 1e-4 {
                    self.v = c.sustain;
                    self.stage = Stage::Sustain;
                }
            }
            Stage::Sustain => self.v = c.sustain,
            Stage::Release => {
                self.v = self.v * c.release_coef + DC;
                if self.v < 1e-4 {
                    self.v = 0.0;
                    self.stage = Stage::Idle;
                }
            }
        }
        self.v
    }
}

#[derive(Clone, Copy, Debug)]
struct Voice {
    amp: Env,
    filt: Env,
    key: u8,
    held: bool,
    id: u32,
    vel: f32,
    /// Start order, for oldest-voice stealing.
    seq: u64,
    ph1: f64,
    ph2: f64,
    ic1: f32,
    ic2: f32,
}

impl Voice {
    const IDLE: Voice = Voice {
        amp: Env::IDLE,
        filt: Env::IDLE,
        key: 0,
        held: false,
        id: 0,
        vel: 0.0,
        seq: 0,
        ph1: 0.0,
        ph2: 0.0,
        ic1: 0.0,
        ic2: 0.0,
    };

    fn active(&self) -> bool {
        self.amp.stage != Stage::Idle
    }
}

fn poly_blep(t: f64, dt: f64) -> f64 {
    if t < dt {
        let t = t / dt;
        t + t - t * t - 1.0
    } else if t > 1.0 - dt {
        let t = (t - 1.0) / dt;
        t * t + t + t + 1.0
    } else {
        0.0
    }
}

#[inline]
fn osc(wave: Wave, phase: f64, dt: f64) -> f64 {
    match wave {
        Wave::Sine => (phase * TAU).sin(),
        Wave::Saw => 2.0 * phase - 1.0 - poly_blep(phase, dt),
        Wave::Square => {
            let naive = if phase < 0.5 { 1.0 } else { -1.0 };
            let mut p2 = phase + 0.5;
            if p2 >= 1.0 {
                p2 -= 1.0;
            }
            naive + poly_blep(phase, dt) - poly_blep(p2, dt)
        }
        Wave::Triangle => 4.0 * (phase - 0.5).abs() - 1.0,
    }
}

/// Pitch in Hz for a key plus semitone and cent offsets.
pub fn key_hz(key: u8, semitones: f32, cents: f32) -> f64 {
    440.0 * ((key as f64 - 69.0 + semitones as f64 + cents as f64 / 100.0) / 12.0).exp2()
}

/// One synth channel: 16 voices. Lives in `Runtime`.
#[derive(Clone)]
pub struct Synth {
    voices: [Voice; SYNTH_VOICES],
    counter: u64,
}

impl Default for Synth {
    fn default() -> Synth {
        Synth::new()
    }
}

impl Synth {
    pub fn new() -> Synth {
        Synth {
            voices: [Voice::IDLE; SYNTH_VOICES],
            counter: 0,
        }
    }

    pub fn reset(&mut self) {
        self.voices = [Voice::IDLE; SYNTH_VOICES];
    }

    pub fn is_active(&self) -> bool {
        self.voices.iter().any(Voice::active)
    }

    pub fn active_voices(&self) -> usize {
        self.voices.iter().filter(|v| v.active()).count()
    }

    pub fn note_on(&mut self, key: u8, vel: u8, id: u32) {
        let i = match self.voices.iter().position(|v| !v.active()) {
            Some(i) => i,
            None => {
                let mut best = 0;
                for (i, v) in self.voices.iter().enumerate() {
                    if v.seq < self.voices[best].seq {
                        best = i;
                    }
                }
                best
            }
        };
        self.counter += 1;
        let mut v = Voice::IDLE;
        v.key = key;
        v.held = true;
        v.id = id;
        v.vel = vel as f32 / 127.0;
        v.seq = self.counter;
        v.amp.gate_on();
        v.filt.gate_on();
        self.voices[i] = v;
    }

    /// Releases the voice started by note `id`. A voice of another note on
    /// the same key (a preview over a sequencer note) keeps sounding.
    pub fn note_off(&mut self, id: u32) {
        for v in &mut self.voices {
            if v.active() && v.held && v.id == id {
                v.held = false;
                v.amp.release();
                v.filt.release();
            }
        }
    }

    /// Adds `out.len()` mono frames. `events` holds this sub-block's events
    /// for all channels in time order; this synth uses those of `slot`.
    pub fn render(
        &mut self,
        ctl: &SynthCtl,
        waves: (Wave, Wave),
        sr: f64,
        slot: u16,
        events: &[SeqEvent],
        out: &mut [f32],
    ) {
        let n = out.len();
        let mut at = 0usize;
        for e in events.iter().filter(|e| e.slot == slot) {
            let off = (e.offset as usize).min(n);
            if off > at {
                self.render_segment(ctl, waves, sr, &mut out[at..off]);
                at = off;
            }
            if e.on {
                self.note_on(e.key, e.vel, e.id);
            } else {
                self.note_off(e.id);
            }
        }
        if at < n {
            self.render_segment(ctl, waves, sr, &mut out[at..]);
        }
    }

    fn render_segment(&mut self, ctl: &SynthCtl, waves: (Wave, Wave), sr: f64, out: &mut [f32]) {
        let max_fc = (sr * 0.45).clamp(20.0, 20000.0);
        let k = 2.0 - 1.95 * ctl.res;
        for v in self.voices.iter_mut().filter(|v| v.active()) {
            let dt1 = key_hz(v.key, ctl.semis1, ctl.cents1) / sr;
            let dt2 = key_hz(v.key, ctl.semis2, ctl.cents2) / sr;
            let level = v.vel * ctl.gain * 0.5;
            for o in out.iter_mut() {
                let a = v.amp.next(&ctl.amp);
                let f = v.filt.next(&ctl.filt);
                let s1 = osc(waves.0, v.ph1, dt1);
                let s2 = osc(waves.1, v.ph2, dt2);
                v.ph1 += dt1;
                if v.ph1 >= 1.0 {
                    v.ph1 -= 1.0;
                }
                v.ph2 += dt2;
                if v.ph2 >= 1.0 {
                    v.ph2 -= 1.0;
                }
                let x = ((1.0 - ctl.mix as f64) * s1 + ctl.mix as f64 * s2) as f32;

                let fc = (ctl.cutoff * (ctl.env_oct * f).exp2()).clamp(20.0, max_fc as f32);
                let g = (std::f32::consts::PI * fc / sr as f32).tan();
                let a1 = 1.0 / (1.0 + g * (g + k));
                let a2 = g * a1;
                let a3 = g * a2;
                let v3 = x - v.ic2;
                let v1 = a1 * v.ic1 + a2 * v3;
                let v2 = v.ic2 + a2 * v.ic1 + a3 * v3;
                v.ic1 = 2.0 * v1 - v.ic1 + DC;
                v.ic2 = 2.0 * v2 - v.ic2 + DC;
                *o += v2 * a * level;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table_with_defaults(slot: ChannelSlot) -> ParamTable {
        let t = ParamTable::new();
        crate::write_synth_params(&t, slot, &protocol::model::SynthParams::default());
        t
    }

    fn ev(offset: u32, key: u8, on: bool, id: u32) -> SeqEvent {
        SeqEvent {
            offset,
            slot: 0,
            key,
            vel: 100,
            on,
            id,
        }
    }

    #[test]
    fn stealing_takes_the_oldest_voice() {
        let mut s = Synth::new();
        for k in 0..16u8 {
            s.note_on(40 + k, 100, k as u32);
        }
        assert_eq!(s.active_voices(), 16);
        s.note_on(100, 100, 99);
        assert_eq!(s.active_voices(), 16);
        assert!(
            s.voices.iter().all(|v| v.key != 40),
            "oldest (key 40) was stolen"
        );
        assert!(s.voices.iter().any(|v| v.key == 100));
    }

    #[test]
    fn note_on_off_makes_sound_then_decays_to_idle() {
        let t = table_with_defaults(ChannelSlot(0));
        let ctl = SynthCtl::read(&t, ChannelSlot(0), 48000.0);
        let mut s = Synth::new();
        let mut out = vec![0.0f32; 4800];
        s.render(
            &ctl,
            (Wave::Saw, Wave::Square),
            48000.0,
            0,
            &[ev(0, 60, true, 1), ev(2400, 60, false, 1)],
            &mut out,
        );
        assert!(out[..2400].iter().any(|x| x.abs() > 0.01));
        assert!(s.is_active(), "still releasing");
        let mut tail = vec![0.0f32; 48000];
        s.render(&ctl, (Wave::Saw, Wave::Square), 48000.0, 0, &[], &mut tail);
        assert!(!s.is_active());
        assert!(out.iter().chain(&tail).all(|x| x.is_finite()));
    }

    #[test]
    fn each_waveform_is_bounded_and_nonzero() {
        for w in [Wave::Sine, Wave::Saw, Wave::Square, Wave::Triangle] {
            let dt = 440.0 / 48000.0;
            let mut ph = 0.0;
            let (mut lo, mut hi) = (f64::MAX, f64::MIN);
            for _ in 0..4800 {
                let x = osc(w, ph, dt);
                lo = lo.min(x);
                hi = hi.max(x);
                ph = (ph + dt) % 1.0;
            }
            assert!(
                lo < -0.9 && hi > 0.9 && lo > -1.6 && hi < 1.6,
                "{w:?} {lo} {hi}"
            );
        }
    }
}
