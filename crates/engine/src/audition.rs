// SPDX-License-Identifier: GPL-3.0-or-later
//! Sound-browser audition (SPEC 20.3): a dedicated preview voice set (one
//! synth voice group, one 808 voice, one sampler voice group) that sounds
//! straight into the master bus, independent of every instrument.
//!
//! Everything is preallocated. Synth and 808 parameters arrive by value in
//! the command and are copied into a private `ParamTable`; a sample arrives
//! through a wait-free inbox ring (`AuditionSample`, sent by
//! `Engine::audition` before the command) and the sample it replaces leaves
//! through a retire ring, so the audio thread neither locks the
//! `SampleStore` nor frees memory.

use crate::bass808::{Bass808, BassCtl};
use crate::sampler::{Sampler, SamplerC, SamplerCtl};
use crate::samples::SampleData;
use crate::synth::{Synth, SynthCtl};
use crate::tables::{write_bass808_params, write_sampler_params, write_synth_params};
use protocol::beats::{Bass808Params, SampleMode, SamplerParams};
use protocol::consts::MAX_BLOCK;
use protocol::engine::{AuditionSource, ChannelSlot, PREVIEW_MAX_SECONDS, ParamTable};
use protocol::model::{SynthParams, Wave};

/// Note id of the audition voices (they live in their own voice sets, so it
/// only has to differ between successive notes).
const ID_A: u32 = 0xF800_0001;
const ID_B: u32 = 0xF800_0002;

/// A decoded sample sent to the audio thread for `Audition::Sample`.
#[derive(Clone, Debug)]
pub struct AuditionSample {
    pub hash: [u8; 32],
    pub data: SampleData,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    None,
    Synth,
    Bass,
    Sample,
}

pub struct Audition {
    /// Private parameter table; the three voice sets read slot 0.
    params: ParamTable,
    synth: Synth,
    bass: Bass808,
    sampler: Sampler,
    kind: Kind,
    waves: (Wave, Wave),
    /// The sampler's content; `sample` is the held decoded sample.
    c: SamplerC,
    hash: Option<[u8; 32]>,
    id: u32,
    /// Frames until the automatic release; 0 when nothing is held.
    left: u32,
    held: bool,
    mono: Vec<f32>,
    l: Vec<f32>,
    r: Vec<f32>,
}

impl Audition {
    /// Allocates everything. Call off the audio thread.
    pub fn new() -> Audition {
        let a = Audition {
            params: ParamTable::new(),
            synth: Synth::new(),
            bass: Bass808::new(),
            sampler: Sampler::new(),
            kind: Kind::None,
            waves: (Wave::Sine, Wave::Sine),
            c: SamplerC {
                mode: SampleMode::OneShot,
                reverse: false,
                root_key: 60,
                sample: None,
            },
            hash: None,
            id: ID_A,
            left: 0,
            held: false,
            mono: vec![0.0; MAX_BLOCK],
            l: vec![0.0; MAX_BLOCK],
            r: vec![0.0; MAX_BLOCK],
        };
        write_sampler_params(&a.params, ChannelSlot(0), &SamplerParams::default());
        a
    }

    /// Whether any voice still sounds (including release tails).
    pub fn is_active(&self) -> bool {
        self.synth.is_active() || self.bass.is_active() || self.sampler.is_active()
    }

    /// Replaces the held sample; returns the previous one for the caller to
    /// retire off-thread. The sampler's voices play the old data, so they
    /// stop first.
    pub fn install_sample(&mut self, s: AuditionSample) -> Option<SampleData> {
        self.sampler.reset();
        self.hash = Some(s.hash);
        self.c.sample.replace(s.data)
    }

    /// Hash of the held sample.
    pub fn held_hash(&self) -> Option<[u8; 32]> {
        self.hash
    }

    /// Starts or releases an audition. A new one replaces the previous.
    pub fn command(
        &mut self,
        sample_rate: f64,
        source: &AuditionSource,
        key: u8,
        vel: u8,
        on: bool,
    ) {
        if key > 127 {
            return;
        }
        if !on {
            self.release();
            return;
        }
        // Replace: release or choke whatever sounds.
        self.release_for_replace(sample_rate);
        let vel = vel.clamp(1, 127);
        self.id = if self.id == ID_A { ID_B } else { ID_A };
        let slot = ChannelSlot(0);
        match source {
            AuditionSource::Synth(p) => {
                self.start_synth(p, slot);
                self.synth.note_on(key, vel, self.id);
                self.kind = Kind::Synth;
            }
            AuditionSource::Bass808(p) => {
                self.start_bass(p, slot);
                let ctl = BassCtl::read(&self.params, slot, sample_rate);
                self.bass.note_on(&ctl, true, key, vel, self.id);
                self.kind = Kind::Bass;
            }
            AuditionSource::Sample { hash } => {
                // A sample that was not delivered (or is not decoded)
                // plays silence.
                if self.hash == Some(*hash) && self.c.sample.is_some() {
                    let ctl = SamplerCtl::read(&self.params, slot, sample_rate);
                    self.sampler.note_on(&self.c, &ctl, key, vel, self.id);
                    self.kind = Kind::Sample;
                } else {
                    self.kind = Kind::None;
                    return;
                }
            }
        }
        self.held = true;
        self.left = ((PREVIEW_MAX_SECONDS * sample_rate).round() as u32).max(1);
    }

    fn start_synth(&mut self, p: &SynthParams, slot: ChannelSlot) {
        write_synth_params(&self.params, slot, p);
        self.waves = (p.osc1.wave, p.osc2.wave);
    }

    fn start_bass(&mut self, p: &Bass808Params, slot: ChannelSlot) {
        write_bass808_params(&self.params, slot, p);
    }

    /// Releases the held note (note-off; the tail rings out).
    pub fn release(&mut self) {
        if !self.held {
            return;
        }
        self.held = false;
        self.left = 0;
        self.synth.note_off(self.id);
        self.bass.note_off(self.id);
        self.sampler.note_off(&self.c, self.id);
    }

    fn release_for_replace(&mut self, sample_rate: f64) {
        self.release();
        // The previous 808 and sampler notes fade out quickly instead of
        // ringing under the new sound.
        let slot = ChannelSlot(0);
        if self.bass.is_active() {
            let ctl = BassCtl::read(&self.params, slot, sample_rate);
            self.bass.choke(&ctl);
        }
        if self.sampler.is_active() {
            let ctl = SamplerCtl::read(&self.params, slot, sample_rate);
            self.sampler.choke(&ctl);
        }
    }

    /// Adds the next `out_l.len()` frames to the buses.
    pub fn render(&mut self, sample_rate: f64, out_l: &mut [f32], out_r: &mut [f32]) {
        let n = out_l.len().min(MAX_BLOCK);
        if self.held {
            if (self.left as usize) <= n {
                self.release();
                if self.kind == Kind::Sample && self.sampler.is_active() {
                    // A long one-shot is cut (with a short fade) too.
                    let ctl = SamplerCtl::read(&self.params, ChannelSlot(0), sample_rate);
                    self.sampler.choke(&ctl);
                }
            } else {
                self.left -= n as u32;
            }
        }
        let slot = ChannelSlot(0);
        if self.synth.is_active() {
            let ctl = SynthCtl::read(&self.params, slot, sample_rate);
            self.mono[..n].fill(0.0);
            self.synth
                .render(&ctl, self.waves, sample_rate, 0, &[], &mut self.mono[..n]);
            for i in 0..n {
                out_l[i] += self.mono[i];
                out_r[i] += self.mono[i];
            }
        }
        if self.bass.is_active() {
            let ctl = BassCtl::read(&self.params, slot, sample_rate);
            self.mono[..n].fill(0.0);
            self.bass
                .render(&ctl, true, sample_rate, 0, 0, &[], &[], &mut self.mono[..n]);
            for i in 0..n {
                out_l[i] += self.mono[i];
                out_r[i] += self.mono[i];
            }
        }
        if self.sampler.is_active() {
            let ctl = SamplerCtl::read(&self.params, slot, sample_rate);
            self.l[..n].fill(0.0);
            self.r[..n].fill(0.0);
            self.sampler.render(
                &self.c,
                &ctl,
                sample_rate,
                0,
                0,
                &[],
                &[],
                &mut self.l[..n],
                &mut self.r[..n],
            );
            let mono = self.c.sample.as_ref().is_none_or(|d| d.channels == 1);
            for i in 0..n {
                out_l[i] += self.l[i];
                out_r[i] += if mono { self.l[i] } else { self.r[i] };
            }
        }
    }
}

impl Default for Audition {
    fn default() -> Audition {
        Audition::new()
    }
}
