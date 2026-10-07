// SPDX-License-Identifier: GPL-3.0-or-later
//! Writing document values into the `ControlTable` and `ParamTable`
//! (SPEC 4.3, 17.1). The GTK thread calls these after every document
//! replacement; the offline renderer calls them once.

use crate::compiled::Slots;
use protocol::beats::{Bass808Param, Bass808Params, BuiltinFx, SamplerParam, SamplerParams};
use protocol::consts::{MAX_INSERTS, MAX_SENDS};
use protocol::engine::{
    CTL_METRONOME_ENABLED, CTL_METRONOME_GAIN_DB, ChannelSlot, ControlTable, MixControl,
    ParamTable, TrackSlot, channel_control, fx_param_index, param_index, send_control,
    track_control,
};
use protocol::model::{Insert, Instrument, Mix, Project, SynthParam, SynthParams};

/// Writes the continuous values of the built-in effect at insert position
/// `pos` of a track.
pub fn write_fx_params(params: &ParamTable, track: TrackSlot, pos: usize, fx: &BuiltinFx) {
    for i in 0..fx.param_count() {
        if let Some(v) = fx.param(i) {
            params.set(fx_param_index(track, pos, i), v as f32);
        }
    }
}

/// Writes the continuous sampler values of a channel.
pub fn write_sampler_params(params: &ParamTable, slot: ChannelSlot, p: &SamplerParams) {
    for q in SamplerParam::ALL {
        params.set(param_index(slot, q.index()), p.get(*q) as f32);
    }
}

/// Writes the continuous 808 values of a channel.
pub fn write_bass808_params(params: &ParamTable, slot: ChannelSlot, p: &Bass808Params) {
    for q in Bass808Param::ALL {
        params.set(param_index(slot, q.index()), p.get(*q) as f32);
    }
}

/// Writes all 17 continuous synth values of a channel.
pub fn write_synth_params(params: &ParamTable, slot: ChannelSlot, p: &SynthParams) {
    for q in SynthParam::ALL {
        params.set(param_index(slot, q.index()), p.get(q) as f32);
    }
}

fn write_mix(controls: &ControlTable, m: &Mix, idx: impl Fn(MixControl) -> usize) {
    controls.set(idx(MixControl::VolumeDb), m.volume_db as f32);
    controls.set(idx(MixControl::Pan), m.pan as f32);
    controls.set(idx(MixControl::Mute), m.mute as u8 as f32);
    controls.set(idx(MixControl::Solo), m.solo as u8 as f32);
}

/// Writes every control and parameter value of `project` (tempo, metronome,
/// every channel and track fader, every built-in synth parameter).
pub fn write_controls(
    project: &Project,
    slots: &Slots,
    controls: &ControlTable,
    params: &ParamTable,
) {
    controls.set_tempo(project.tempo_bpm);
    controls.set(CTL_METRONOME_GAIN_DB, project.metronome.gain_db as f32);
    controls.set(
        CTL_METRONOME_ENABLED,
        project.metronome.enabled as u8 as f32,
    );
    for t in &project.tracks {
        if let Some(s) = slots.track_slot(t.id) {
            let ts = TrackSlot(s.0);
            write_mix(controls, &t.mix, |c| track_control(ts, c));
            for (i, snd) in t.sends.iter().enumerate().take(MAX_SENDS) {
                controls.set(send_control(ts, i), snd.level_db as f32);
            }
            for (pos, ins) in t.inserts.iter().enumerate().take(MAX_INSERTS) {
                if let Insert::Builtin { fx, .. } = ins {
                    write_fx_params(params, ts, pos, fx);
                }
            }
        }
    }
    for ch in &project.channels {
        let Some(s) = slots.channel_slot(ch.id) else {
            continue;
        };
        write_mix(controls, &ch.mix, |c| channel_control(s, c));
        if let Instrument::Synth(p) = &ch.instrument {
            write_synth_params(params, s, p);
        } else if let Instrument::Bass808(b) = &ch.instrument {
            write_bass808_params(params, s, &b.params);
        } else if let Instrument::Sampler(sm) = &ch.instrument {
            write_sampler_params(params, s, &sm.params);
        }
    }
}
