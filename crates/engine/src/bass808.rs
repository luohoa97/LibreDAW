// SPDX-License-Identifier: GPL-3.0-or-later
//! 808 bass (SPEC 15.2, 17.2): a sine with a pitch envelope, a short click,
//! a long exponential decay, a saturator and a tone lowpass.
//!
//! Mono mode has last-note priority: a note-on while the gate is open is
//! legato (the pitch glides, the envelopes keep running) and a note-off for
//! a note that is not the current one is ignored. Poly mode gives every
//! note its own voice. Voices are released by note id.

use crate::sequencer::{ChannelEvent, ChokeEvent, SeqEvent, channel_events};
use protocol::beats::Bass808Param;
use protocol::consts::{MIN_GAIN_DB, SYNTH_VOICES};
use protocol::engine::{ChannelSlot, ParamTable, param_index};

const TAU: f64 = std::f64::consts::TAU;
const LN_1000: f64 = 6.907_755_278_982_137;
const DC: f32 = 1e-18;
/// Release after a note-off, seconds to reach -60 dB.
const RELEASE_SECONDS: f64 = 0.04;
/// Choked voices fade out over this long (17.2).
pub const CHOKE_FADE_SECONDS: f64 = 0.0015;
/// Time constant of the click burst.
const CLICK_SECONDS: f64 = 0.001;

/// Continuous 808 values for one sub-block.
#[derive(Clone, Copy, Debug)]
pub struct BassCtl {
    tune: f32,
    drop_semis: f32,
    drop_coef: f64,
    decay_coef: f32,
    click: f32,
    drive_gain: f32,
    drive_norm: f32,
    tone_a: f32,
    glide_frames: u32,
    gain: f32,
    release_coef: f32,
    click_coef: f32,
    fade_frames: u32,
}

fn exp_coef(ms: f64, sr: f64) -> f64 {
    (-LN_1000 / (ms.max(0.01) * 0.001 * sr)).exp()
}

impl BassCtl {
    /// Adds a pitch shape's offset (24.2-1).
    pub fn shift_semitones(&mut self, st: f32) {
        self.tune += st;
    }

    pub fn read(params: &ParamTable, slot: ChannelSlot, sr: f64) -> BassCtl {
        let p = |q: Bass808Param| params.get(param_index(slot, q.index()));
        use Bass808Param::*;
        let drive = p(Drive).clamp(0.0, 1.0);
        let g = 1.0 + drive * 12.0;
        let gain_db = p(GainDb);
        BassCtl {
            tune: p(Tune),
            drop_semis: p(DropSemitones).max(0.0),
            drop_coef: exp_coef(p(DropMs) as f64, sr),
            decay_coef: exp_coef(p(DecayMs) as f64, sr) as f32,
            click: p(Click).clamp(0.0, 1.0),
            drive_gain: g,
            drive_norm: if drive > 0.0 { 1.0 / g.tanh() } else { 1.0 },
            tone_a: (1.0 - (-TAU * (p(ToneHz) as f64).clamp(20.0, sr * 0.45) / sr).exp()) as f32,
            glide_frames: ((p(GlideMs) as f64).max(0.0) * 0.001 * sr).round() as u32,
            gain: if (gain_db as f64) <= MIN_GAIN_DB {
                0.0
            } else {
                10f32.powf(gain_db / 20.0)
            },
            release_coef: exp_coef(RELEASE_SECONDS * 1000.0, sr) as f32,
            click_coef: (-1.0 / (CLICK_SECONDS * sr)).exp() as f32,
            fade_frames: (CHOKE_FADE_SECONDS * sr).round().max(1.0) as u32,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Voice {
    active: bool,
    held: bool,
    id: u32,
    vel: f32,
    seq: u64,
    phase: f64,
    /// Current pitch in semitones (MIDI key units) without the drop.
    st: f64,
    st_target: f64,
    st_step: f64,
    glide_left: u32,
    drop: f64,
    amp: f32,
    click_env: f32,
    noise: u32,
    lp: f32,
    /// Choke fade: frames left and the total, 0 = not choked.
    fade_left: u32,
    fade_total: u32,
}

impl Voice {
    const IDLE: Voice = Voice {
        active: false,
        held: false,
        id: 0,
        vel: 0.0,
        seq: 0,
        phase: 0.0,
        st: 0.0,
        st_target: 0.0,
        st_step: 0.0,
        glide_left: 0,
        drop: 0.0,
        amp: 0.0,
        click_env: 0.0,
        noise: 0x1234_5678,
        lp: 0.0,
        fade_left: 0,
        fade_total: 0,
    };
}

/// One 808 channel: one voice in mono mode, 16 in poly mode.
#[derive(Clone)]
pub struct Bass808 {
    voices: [Voice; SYNTH_VOICES],
    counter: u64,
}

impl Default for Bass808 {
    fn default() -> Bass808 {
        Bass808::new()
    }
}

impl Bass808 {
    pub fn new() -> Bass808 {
        Bass808 {
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

    fn start(&mut self, i: usize, ctl: &BassCtl, key: u8, vel: u8, id: u32, keep_phase: bool) {
        self.counter += 1;
        let old = self.voices[i];
        let mut v = Voice::IDLE;
        v.active = true;
        v.held = true;
        v.id = id;
        v.vel = vel as f32 / 127.0;
        v.seq = self.counter;
        v.st = key as f64;
        v.st_target = key as f64;
        v.drop = ctl.drop_semis as f64;
        v.amp = 1.0;
        v.click_env = 1.0;
        v.noise = old.noise;
        if keep_phase && old.active {
            v.phase = old.phase;
            v.lp = old.lp;
        }
        self.voices[i] = v;
    }

    pub fn note_on(&mut self, ctl: &BassCtl, mono: bool, key: u8, vel: u8, id: u32) {
        if mono {
            let v = self.voices[0];
            if v.active && v.held && v.fade_left == 0 {
                // Legato: glide, no envelope retrigger.
                let v = &mut self.voices[0];
                v.id = id;
                v.vel = vel as f32 / 127.0;
                v.st_target = key as f64;
                if ctl.glide_frames == 0 {
                    v.st = v.st_target;
                    v.glide_left = 0;
                } else {
                    v.glide_left = ctl.glide_frames;
                    v.st_step = (v.st_target - v.st) / ctl.glide_frames as f64;
                }
            } else {
                self.start(0, ctl, key, vel, id, true);
            }
            return;
        }
        let i = match self.voices.iter().position(|v| !v.active) {
            Some(i) => i,
            None => (0..SYNTH_VOICES)
                .min_by_key(|&i| self.voices[i].seq)
                .unwrap_or(0),
        };
        self.start(i, ctl, key, vel, id, false);
    }

    /// Releases the voice of note `id`. In mono mode only the current note
    /// counts: the off of an overridden note is ignored.
    pub fn note_off(&mut self, id: u32) {
        for v in &mut self.voices {
            if v.active && v.held && v.id == id {
                v.held = false;
            }
        }
    }

    /// Fades every voice out over the choke time.
    pub fn choke(&mut self, ctl: &BassCtl) {
        for v in &mut self.voices {
            if v.active && v.fade_left == 0 {
                v.fade_left = ctl.fade_frames;
                v.fade_total = ctl.fade_frames;
            }
        }
    }

    /// Adds `out.len()` mono frames. See `channel_events` for the event rules.
    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &mut self,
        ctl: &BassCtl,
        mono: bool,
        sr: f64,
        slot: u16,
        group: u8,
        events: &[SeqEvent],
        chokes: &[ChokeEvent],
        out: &mut [f32],
    ) {
        let n = out.len();
        let mut at = 0usize;
        for (offset, ev) in channel_events(slot, group, events, chokes) {
            let off = (offset as usize).min(n);
            if off > at {
                self.segment(ctl, mono, sr, &mut out[at..off]);
                at = off;
            }
            match ev {
                ChannelEvent::Choke => self.choke(ctl),
                ChannelEvent::Note(e) if e.on => self.note_on(ctl, mono, e.key, e.vel, e.id),
                ChannelEvent::Note(e) => self.note_off(e.id),
            }
        }
        if at < n {
            self.segment(ctl, mono, sr, &mut out[at..]);
        }
    }

    fn segment(&mut self, ctl: &BassCtl, mono: bool, sr: f64, out: &mut [f32]) {
        let count = if mono { 1 } else { SYNTH_VOICES };
        for v in self.voices[..count].iter_mut().filter(|v| v.active) {
            for o in out.iter_mut() {
                if v.glide_left > 0 {
                    v.glide_left -= 1;
                    v.st = if v.glide_left == 0 {
                        v.st_target
                    } else {
                        v.st + v.st_step
                    };
                }
                let st = v.st + v.drop + ctl.tune as f64;
                let hz = 440.0 * ((st - 69.0) / 12.0).exp2();
                v.phase += hz / sr;
                v.phase -= v.phase.floor();
                let mut x = (v.phase * TAU).sin() as f32;
                v.noise ^= v.noise << 13;
                v.noise ^= v.noise >> 17;
                v.noise ^= v.noise << 5;
                let nz = (v.noise as f32 / u32::MAX as f32) * 2.0 - 1.0;
                x += nz * v.click_env * ctl.click * 0.5;
                v.click_env *= ctl.click_coef;
                v.drop *= ctl.drop_coef;
                x *= v.amp;
                v.amp *= if v.held {
                    ctl.decay_coef
                } else {
                    ctl.release_coef
                };
                if ctl.drive_gain > 1.0 {
                    x = (x * ctl.drive_gain).tanh() * ctl.drive_norm;
                }
                v.lp += ctl.tone_a * (x - v.lp) + DC;
                let mut y = v.lp * v.vel * ctl.gain;
                if v.fade_left > 0 {
                    y *= v.fade_left as f32 / v.fade_total as f32;
                    v.fade_left -= 1;
                    if v.fade_left == 0 {
                        v.active = false;
                    }
                }
                *o += y;
                if v.amp < 1e-4 {
                    v.active = false;
                    v.lp = 0.0;
                }
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
    use crate::testutil::{band_energy, key_hz as hz_of};
    use protocol::beats::Bass808Params;

    const SR: f64 = 48000.0;
    const S0: ChannelSlot = ChannelSlot(0);

    fn ctl_with(f: impl Fn(&mut Bass808Params)) -> BassCtl {
        let t = ParamTable::new();
        let mut p = Bass808Params::default();
        f(&mut p);
        crate::write_bass808_params(&t, S0, &p);
        BassCtl::read(&t, S0, SR)
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

    fn render(
        b: &mut Bass808,
        ctl: &BassCtl,
        mono: bool,
        events: &[SeqEvent],
        n: usize,
    ) -> Vec<f32> {
        let mut out = vec![0.0; n];
        b.render(ctl, mono, SR, 0, 0, events, &[], &mut out);
        out
    }

    /// Frequency from upward zero crossings in `x`.
    fn zc_hz(x: &[f32]) -> f64 {
        let mut first = None;
        let mut last = 0.0;
        let mut count = 0;
        for i in 1..x.len() {
            if x[i - 1] < 0.0 && x[i] >= 0.0 {
                let f = i as f64 - 1.0 + (-x[i - 1] / (x[i] - x[i - 1])) as f64;
                if first.is_none() {
                    first = Some(f);
                } else {
                    count += 1;
                }
                last = f;
            }
        }
        count as f64 * SR / (last - first.unwrap())
    }

    fn plain(p: &mut Bass808Params) {
        p.drop_semitones = 0.0;
        p.click = 0.0;
        p.drive = 0.0;
        p.tone_hz = 20000.0;
        p.decay_ms = 10000.0;
        p.glide_ms = 100.0;
    }

    #[test]
    fn glide_reaches_the_target_pitch_in_glide_ms() {
        let ctl = ctl_with(plain);
        let mut b = Bass808::new();
        // Key 36 for 0.2 s, then legato to key 48 with a 100 ms glide.
        let t0 = 9600u32;
        let out = render(
            &mut b,
            &ctl,
            true,
            &[ev(0, 36, true, 1), ev(t0, 48, true, 2)],
            t0 as usize + 24000,
        );
        let before = zc_hz(&out[4800..t0 as usize]);
        assert!((before - hz_of(36.0)).abs() < 0.5, "{before}");
        // Just after the glide ends (100 ms = 4800 frames) it is on target.
        let g = t0 as usize + 4800;
        let after = zc_hz(&out[g + 480..g + 9600]);
        assert!(
            (after - hz_of(48.0)).abs() < 0.8,
            "{after} vs {}",
            hz_of(48.0)
        );
        // Halfway through it is strictly between the two pitches.
        let mid = zc_hz(&out[t0 as usize + 1800..t0 as usize + 3000]);
        assert!(mid > hz_of(36.0) * 1.1 && mid < hz_of(48.0) * 0.95, "{mid}");
        // The state reaches the exact target after glide_ms frames.
        let mut b2 = Bass808::new();
        render(
            &mut b2,
            &ctl,
            true,
            &[ev(0, 36, true, 1), ev(100, 48, true, 2)],
            100,
        );
        assert!(b2.voices[0].glide_left > 0);
        render(&mut b2, &ctl, true, &[], 4800);
        assert_eq!(b2.voices[0].st, 48.0);
        assert_eq!(b2.voices[0].glide_left, 0);
    }

    #[test]
    fn legato_does_not_retrigger_the_envelope_but_a_gap_does() {
        let ctl = ctl_with(|p| {
            plain(p);
            p.decay_ms = 1000.0;
        });
        let mut b = Bass808::new();
        render(&mut b, &ctl, true, &[ev(0, 36, true, 1)], 24000);
        let amp_before = b.voices[0].amp;
        assert!(amp_before < 0.1 && amp_before > 0.0);
        render(&mut b, &ctl, true, &[ev(0, 40, true, 2)], 1);
        let amp_after = b.voices[0].amp;
        assert!(
            amp_after <= amp_before,
            "no retrigger: {amp_before} -> {amp_after}"
        );
        assert!(b.voices[0].held && b.voices[0].id == 2);
        // Release, then a new note: the envelope restarts.
        render(&mut b, &ctl, true, &[ev(0, 40, false, 2)], 2400);
        assert!(!b.voices[0].held);
        render(&mut b, &ctl, true, &[ev(0, 43, true, 3)], 1);
        assert!(b.voices[0].amp > 0.99);
    }

    #[test]
    fn note_off_of_a_non_current_note_is_ignored() {
        let ctl = ctl_with(plain);
        let mut b = Bass808::new();
        render(
            &mut b,
            &ctl,
            true,
            &[ev(0, 36, true, 1), ev(10, 40, true, 2)],
            100,
        );
        render(&mut b, &ctl, true, &[ev(0, 36, false, 1)], 100);
        assert!(
            b.voices[0].held,
            "note 1 was overridden; its off is ignored"
        );
        render(&mut b, &ctl, true, &[ev(0, 40, false, 2)], 100);
        assert!(!b.voices[0].held);
        // A note-off that arrives after the voice was released changes nothing.
        render(&mut b, &ctl, true, &[ev(0, 36, false, 1)], 10);
        assert_eq!(b.active_voices(), 1);
    }

    #[test]
    fn poly_mode_plays_notes_on_separate_voices() {
        let ctl = ctl_with(plain);
        let mut b = Bass808::new();
        render(
            &mut b,
            &ctl,
            false,
            &[ev(0, 36, true, 1), ev(0, 43, true, 2)],
            480,
        );
        assert_eq!(b.active_voices(), 2);
        render(&mut b, &ctl, false, &[ev(0, 36, false, 1)], 480);
        assert!(b.voices.iter().filter(|v| v.active && v.held).count() == 1);
    }

    #[test]
    fn c1_spectrum_is_mostly_below_150_hz() {
        let ctl = ctl_with(|_| {});
        let mut b = Bass808::new();
        let out = render(&mut b, &ctl, true, &[ev(0, 24, true, 1)], 48000);
        assert!(out.iter().all(|x| x.is_finite()));
        let n = 32768;
        let low = band_energy(&out[2000..2000 + n], SR, 0.0, 150.0);
        let all = band_energy(&out[2000..2000 + n], SR, 0.0, SR / 2.0);
        assert!(low / all > 0.9, "low fraction {}", low / all);
        // And it really sounds: the fundamental is at 32.7 Hz.
        let f0 = band_energy(&out[2000..2000 + n], SR, 25.0, 45.0);
        assert!(f0 / all > 0.5, "fundamental fraction {}", f0 / all);
    }

    #[test]
    fn choke_fades_to_silence_in_1_5_ms() {
        let ctl = ctl_with(plain);
        let mut b = Bass808::new();
        render(&mut b, &ctl, true, &[ev(0, 36, true, 1)], 1000);
        let chokes = [ChokeEvent {
            offset: 100,
            group: 1,
            source: 7,
        }];
        let mut out = vec![0.0; 400];
        b.render(&ctl, true, SR, 0, 1, &[], &chokes, &mut out);
        assert!(out[..100].iter().any(|x| x.abs() > 0.05));
        let fade = (CHOKE_FADE_SECONDS * SR).round() as usize;
        assert!(out[100 + fade..].iter().all(|x| *x == 0.0));
        assert!(!b.is_active());
    }
}
