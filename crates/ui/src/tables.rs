// SPDX-License-Identifier: GPL-3.0-or-later
//! Writes every control value and native parameter of a document into the
//! `ControlTable` and `ParamTable` (SPEC 4.3, 17.1). Called on the GTK
//! thread after every document replacement, so the tables always hold the
//! current document's values and a compiled state can never revert a fader.

use protocol::engine::{
    CTL_METRONOME_ENABLED, CTL_METRONOME_GAIN_DB, ControlTable, MixControl, ParamTable,
    channel_control, param_index, track_control,
};
use protocol::model::{Instrument, Mix, Project, SynthParam};

use crate::slots::SlotAllocator;

fn flag(b: bool) -> f32 {
    if b { 1.0 } else { 0.0 }
}

fn write_mix(t: &ControlTable, idx: impl Fn(MixControl) -> usize, m: &Mix) {
    t.set(idx(MixControl::VolumeDb), m.volume_db as f32);
    t.set(idx(MixControl::Pan), m.pan as f32);
    t.set(idx(MixControl::Mute), flag(m.mute));
    t.set(idx(MixControl::Solo), flag(m.solo));
}

/// A few hundred relaxed stores. Entities without a slot are skipped.
pub fn write_all(controls: &ControlTable, params: &ParamTable, p: &Project, slots: &SlotAllocator) {
    controls.set_tempo(p.tempo_bpm);
    controls.set(CTL_METRONOME_GAIN_DB, p.metronome.gain_db as f32);
    controls.set(CTL_METRONOME_ENABLED, flag(p.metronome.enabled));
    for t in &p.tracks {
        if let Some((slot, _)) = slots.track_slot(t.id) {
            write_mix(controls, |c| track_control(slot, c), &t.mix);
        }
    }
    for c in &p.channels {
        let Some((slot, _)) = slots.channel_slot(c.id) else {
            continue;
        };
        write_mix(controls, |k| channel_control(slot, k), &c.mix);
        if let Instrument::Synth(s) = &c.instrument {
            for sp in SynthParam::ALL {
                params.set(param_index(slot, sp.index()), s.get(sp) as f32);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::{Document, apply, apply_batch};
    use protocol::edit::{Edit, MixValue, NewInstrument};
    use protocol::ids::{ChannelId, TrackId};
    use protocol::model::SynthParams;

    #[test]
    fn writes_mix_synth_metronome_and_tempo_by_slot() {
        let d = Document::new();
        let (d, ids) = apply_batch(
            &d,
            &[
                Edit::AddTrack { name: "t".into() },
                Edit::SetTempo { bpm: 133.5 },
                Edit::SetMetronome {
                    enabled: true,
                    gain_db: -9.0,
                },
            ],
        )
        .unwrap();
        let t = TrackId(ids[0]);
        let (d, ids) = apply(
            &d,
            &Edit::AddChannel {
                name: "c".into(),
                instrument: NewInstrument::Synth {
                    params: SynthParams::default(),
                },
                root_key: 60,
                track: t,
            },
        )
        .unwrap();
        let c = ChannelId(ids[0]);
        let (d, _) = apply_batch(
            &d,
            &[
                Edit::SetChannelMix {
                    channel: c,
                    value: MixValue::VolumeDb(-12.0),
                },
                Edit::SetChannelMix {
                    channel: c,
                    value: MixValue::Pan(0.5),
                },
                Edit::SetChannelMix {
                    channel: c,
                    value: MixValue::Mute(true),
                },
                Edit::SetTrackMix {
                    track: t,
                    value: MixValue::Solo(true),
                },
                Edit::SetSynthParam {
                    channel: c,
                    param: SynthParam::CutoffHz,
                    value: 1234.0,
                },
            ],
        )
        .unwrap();
        let mut s = SlotAllocator::new();
        s.sync(&d.project).unwrap();
        let (ct, pt) = (ControlTable::new(), ParamTable::new());
        write_all(&ct, &pt, &d.project, &s);
        let (cs, _) = s.channel_slot(c).unwrap();
        let (ts, _) = s.track_slot(t).unwrap();
        assert_eq!(ct.get(channel_control(cs, MixControl::VolumeDb)), -12.0);
        assert_eq!(ct.get(channel_control(cs, MixControl::Pan)), 0.5);
        assert_eq!(ct.get(channel_control(cs, MixControl::Mute)), 1.0);
        assert_eq!(ct.get(channel_control(cs, MixControl::Solo)), 0.0);
        assert_eq!(ct.get(track_control(ts, MixControl::Solo)), 1.0);
        assert_eq!(ct.get(track_control(ts, MixControl::Mute)), 0.0);
        assert_eq!(ct.get(CTL_METRONOME_GAIN_DB), -9.0);
        assert_eq!(ct.get(CTL_METRONOME_ENABLED), 1.0);
        assert_eq!(ct.tempo(), 133.5);
        assert_eq!(
            pt.get(param_index(cs, SynthParam::CutoffHz.index())),
            1234.0
        );
        assert_eq!(
            pt.get(param_index(cs, SynthParam::AmpSustain.index())),
            SynthParams::default().amp_env.sustain as f32
        );
    }

    #[test]
    fn rewriting_after_undo_restores_values() {
        let (d0, ids) = apply(&Document::new(), &Edit::AddTrack { name: "t".into() }).unwrap();
        let t = TrackId(ids[0]);
        let (d1, _) = apply(
            &d0,
            &Edit::SetTrackMix {
                track: t,
                value: MixValue::VolumeDb(-20.0),
            },
        )
        .unwrap();
        let mut s = SlotAllocator::new();
        s.sync(&d1.project).unwrap();
        let (ct, pt) = (ControlTable::new(), ParamTable::new());
        write_all(&ct, &pt, &d1.project, &s);
        let (ts, _) = s.track_slot(t).unwrap();
        assert_eq!(ct.get(track_control(ts, MixControl::VolumeDb)), -20.0);
        write_all(&ct, &pt, &d0.project, &s);
        assert_eq!(ct.get(track_control(ts, MixControl::VolumeDb)), 0.0);
    }
}
