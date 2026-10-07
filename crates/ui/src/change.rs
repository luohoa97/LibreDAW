// SPDX-License-Identifier: GPL-3.0-or-later
//! Pure helpers that compare documents or look ahead at edits, used by the
//! engine glue: does this change need a recompile (4.3), which plugin
//! parameter events does an undo replay (17.1), which plugin instances does
//! an edit remove (7.5).

use std::collections::HashMap;
use std::sync::Arc;

use protocol::beats::BuiltinFx;
use protocol::edit::Edit;
use protocol::engine::PluginEvent;
use protocol::ids::InstanceId;
use protocol::model::{Channel, Insert, Instrument, Mix, Project, SynthParams, Track};

use crate::slots::SlotAllocator;
use protocol::model::ClapRef;

/// The CLAP plugin of an insert, if it is one.
pub fn clap_of(i: &Insert) -> Option<&ClapRef> {
    match i {
        Insert::Clap(r) => Some(r),
        Insert::Builtin { .. } => None,
    }
}

fn clap_mut(i: &mut Insert) -> Option<&mut ClapRef> {
    match i {
        Insert::Clap(r) => Some(r),
        Insert::Builtin { .. } => None,
    }
}

fn norm_channel(c: &Channel) -> Channel {
    let mut c = c.clone();
    c.name.clear();
    c.mix = Mix::default();
    match &mut c.instrument {
        Instrument::Synth(s) => {
            // Waveforms are structural; every continuous value is not.
            let (w1, w2) = (s.osc1.wave, s.osc2.wave);
            *s = SynthParams::default();
            s.osc1.wave = w1;
            s.osc2.wave = w2;
        }
        Instrument::Clap(r) => {
            r.params.clear();
            r.state_file = None;
            r.state_bytes = None;
            r.plugin_version.clear();
        }
        Instrument::Sampler(s) => {
            // Sample, mode and direction are structural; the knobs are not.
            s.params = Default::default();
        }
        Instrument::Bass808(b) => {
            // Mono is structural; the knobs are not.
            b.params = Default::default();
        }
        // An audio row has no sound settings; its clips are not compared here.
        Instrument::Audio => {}
    }
    c
}

/// A built-in effect with its continuous parameters reset; kind, curve,
/// ping-pong and sidechain source stay because they are structural.
fn norm_fx(fx: &mut BuiltinFx) {
    match fx {
        BuiltinFx::Eq { params } => *params = Default::default(),
        BuiltinFx::Compressor { params, .. } => *params = Default::default(),
        BuiltinFx::Saturator { params, .. } => *params = Default::default(),
        BuiltinFx::Reverb { params } => *params = Default::default(),
        BuiltinFx::Delay { params, .. } => *params = Default::default(),
        BuiltinFx::Limiter { params } => *params = Default::default(),
    }
}

fn norm_track(t: &Track) -> Track {
    let mut t = t.clone();
    t.name.clear();
    t.mix = Mix::default();
    for r in t.inserts.iter_mut().filter_map(clap_mut) {
        r.params.clear();
        r.state_file = None;
        r.state_bytes = None;
        r.plugin_version.clear();
    }
    for i in &mut t.inserts {
        if let Insert::Builtin { fx, .. } = i {
            norm_fx(fx);
        }
    }
    // Send levels are control values; the target and tap point are not.
    for s in &mut t.sends {
        s.level_db = 0.0;
    }
    t
}

/// True if going from `old` to `new` changes anything `Compiled` holds
/// (patterns, routing, instrument kinds, waveforms, plugin layout). A change
/// that only moves faders, tempo, the metronome, names, synth knobs, or
/// plugin parameter values returns false (4.3, 17.1).
pub fn needs_compile(old: &Project, new: &Project) -> bool {
    if old.time_sig_num != new.time_sig_num
        || old.patterns.len() != new.patterns.len()
        || old.channels.len() != new.channels.len()
        || old.tracks.len() != new.tracks.len()
        || old.clips != new.clips
        || old.loop_region != new.loop_region
    {
        return true;
    }
    if old
        .patterns
        .iter()
        .zip(&new.patterns)
        .any(|(a, b)| !Arc::ptr_eq(a, b) && a != b)
    {
        return true;
    }
    if old
        .channels
        .iter()
        .zip(&new.channels)
        .any(|(a, b)| !Arc::ptr_eq(a, b) && norm_channel(a) != norm_channel(b))
    {
        return true;
    }
    old.tracks
        .iter()
        .zip(&new.tracks)
        .any(|(a, b)| !Arc::ptr_eq(a, b) && norm_track(a) != norm_track(b))
}

fn plugin_params(p: &Project) -> HashMap<InstanceId, &[protocol::model::ParamValue]> {
    let mut m = HashMap::new();
    for c in &p.channels {
        if let Instrument::Clap(r) = &c.instrument {
            m.insert(r.instance, r.params.as_slice());
        }
    }
    for t in &p.tracks {
        for r in t.inserts.iter().filter_map(clap_of) {
            m.insert(r.instance, r.params.as_slice());
        }
    }
    m
}

/// Parameter events that bring plugins from `old`'s values to `new`'s
/// (undo and redo replay differences as events, never `state.load` while
/// processing, 17.1). Only instances present in both documents and known to
/// the slot table are included.
pub fn param_diffs(old: &Project, new: &Project, slots: &SlotAllocator) -> Vec<PluginEvent> {
    let (o, n) = (plugin_params(old), plugin_params(new));
    let places = slots.plugin_slots(new);
    let mut out = Vec::new();
    let mut ids: Vec<_> = n.keys().copied().collect();
    ids.sort();
    for id in ids {
        let (Some(slot), Some(old_vals)) = (places.get(&id), o.get(&id)) else {
            continue;
        };
        for v in n[&id] {
            let same = old_vals.iter().any(|x| x.id == v.id && x.value == v.value);
            if !same {
                out.push(PluginEvent {
                    slot: *slot,
                    param_id: v.id,
                    value: v.value,
                });
            }
        }
    }
    out
}

/// Instances that applying `edits` to `p` removes. The GTK thread captures
/// their state before the edit is applied (7.5 (a)), so undoing the removal
/// restores the plugin with its patch.
pub fn removed_instances(p: &Project, edits: &[Edit]) -> Vec<InstanceId> {
    let mut out = Vec::new();
    for e in edits {
        match e {
            Edit::RemoveChannel { channel } => {
                if let Some(Instrument::Clap(r)) = p.channel(*channel).map(|c| &c.instrument) {
                    out.push(r.instance);
                }
            }
            Edit::RemoveTrack { track } => {
                if let Some(t) = p.track(*track) {
                    out.extend(t.inserts.iter().filter_map(clap_of).map(|r| r.instance));
                }
            }
            Edit::RemoveInsert { instance, .. } => out.push(*instance),
            _ => {}
        }
    }
    out.sort();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use doc::document::{Document, apply, apply_batch};
    use protocol::edit::{MixValue, NewInstrument, NewNote};
    use protocol::engine::PluginSlot;
    use protocol::ids::{ChannelId, PatternId, TrackId};
    use protocol::model::{SynthParam, Wave};

    fn doc() -> (Document, ChannelId, PatternId) {
        let (d, ids) = apply_batch(
            &Document::new(),
            &[
                Edit::AddChannel {
                    name: "c".into(),
                    instrument: NewInstrument::Synth {
                        params: SynthParams::default(),
                    },
                    root_key: 60,
                    track: TrackId::MASTER,
                },
                Edit::AddPattern {
                    instrument: ChannelId(1),
                    name: "p".into(),
                    length_steps: 16,
                },
            ],
        )
        .unwrap();
        (d, ChannelId(ids[0]), PatternId(ids[1]))
    }

    fn compile_after(d: &Document, e: Edit) -> bool {
        let (n, _) = apply(d, &e).unwrap();
        needs_compile(&d.project, &n.project)
    }

    #[test]
    fn control_values_do_not_recompile() {
        let (d, c, _) = doc();
        assert!(!compile_after(&d, Edit::SetTempo { bpm: 99.0 }));
        assert!(!compile_after(
            &d,
            Edit::SetMetronome {
                enabled: true,
                gain_db: -3.0
            }
        ));
        assert!(!compile_after(
            &d,
            Edit::SetChannelMix {
                channel: c,
                value: MixValue::VolumeDb(-5.0)
            }
        ));
        assert!(!compile_after(
            &d,
            Edit::SetTrackMix {
                track: TrackId::MASTER,
                value: MixValue::Mute(true)
            }
        ));
        assert!(!compile_after(
            &d,
            Edit::SetSynthParam {
                channel: c,
                param: SynthParam::CutoffHz,
                value: 900.0
            }
        ));
        assert!(!compile_after(
            &d,
            Edit::RenameChannel {
                channel: c,
                name: "z".into()
            }
        ));
    }

    #[test]
    fn structural_changes_recompile() {
        let (d, c, p) = doc();
        assert!(compile_after(&d, Edit::SetTimeSigNum { num: 3 }));
        assert!(compile_after(
            &d,
            Edit::SetStep {
                pattern: p,
                step: 0,
                on: true,
                vel: None
            }
        ));
        assert!(compile_after(
            &d,
            Edit::AddNotes {
                pattern: p,
                notes: vec![NewNote {
                    start: 0,
                    len: 5,
                    key: 1,
                    vel: 1
                }]
            }
        ));
        assert!(compile_after(
            &d,
            Edit::SetSynthWave {
                channel: c,
                osc: 1,
                wave: Wave::Sine
            }
        ));
        assert!(compile_after(
            &d,
            Edit::SetRootKey {
                channel: c,
                key: 40
            }
        ));
        assert!(compile_after(&d, Edit::AddTrack { name: "t".into() }));
        assert!(compile_after(&d, Edit::RemoveChannel { channel: c }));
        assert!(compile_after(&d, Edit::RemovePattern { pattern: p }));
        assert!(compile_after(
            &d,
            Edit::AddInsert {
                track: TrackId::MASTER,
                index: 0,
                plugin_id: "a.b".into()
            }
        ));
    }

    #[test]
    fn undo_of_a_fader_does_not_recompile_either() {
        let (d, c, _) = doc();
        let (d2, _) = apply(
            &d,
            &Edit::SetChannelMix {
                channel: c,
                value: MixValue::Pan(0.3),
            },
        )
        .unwrap();
        assert!(!needs_compile(&d2.project, &d.project));
    }

    fn with_plugins() -> (Document, InstanceId, InstanceId, TrackId) {
        let (d, ids) = apply_batch(
            &Document::new(),
            &[
                Edit::AddChannel {
                    name: "p".into(),
                    instrument: NewInstrument::Clap {
                        plugin_id: "a.b".into(),
                        preset: None,
                    },
                    root_key: 60,
                    track: TrackId::MASTER,
                },
                Edit::AddInsert {
                    track: TrackId::MASTER,
                    index: 0,
                    plugin_id: "c.d".into(),
                },
            ],
        )
        .unwrap();
        (d, InstanceId(ids[1]), InstanceId(ids[2]), TrackId::MASTER)
    }

    #[test]
    fn plugin_params_do_not_recompile_but_diffs_replay() {
        let (d, a, b, _) = with_plugins();
        let (d2, _) = apply_batch(
            &d,
            &[
                Edit::SetPluginParam {
                    instance: a,
                    param_id: 5,
                    value: 0.5,
                },
                Edit::SetPluginParam {
                    instance: b,
                    param_id: 1,
                    value: 0.25,
                },
            ],
        )
        .unwrap();
        assert!(!needs_compile(&d.project, &d2.project));
        let mut s = SlotAllocator::new();
        s.sync(&d2.project).unwrap();
        let ev = param_diffs(&d.project, &d2.project, &s);
        assert_eq!(ev.len(), 2);
        let on_a = ev.iter().find(|e| e.param_id == 5).unwrap();
        assert!(matches!(on_a.slot, PluginSlot::Instrument(_)));
        assert_eq!(on_a.value, 0.5);
        let on_b = ev.iter().find(|e| e.param_id == 1).unwrap();
        assert!(matches!(on_b.slot, PluginSlot::Insert { index: 0, .. }));
        // Nothing to replay when nothing changed, or going the other way
        // for values that were never set.
        assert!(param_diffs(&d2.project, &d2.project, &s).is_empty());
        assert!(param_diffs(&d2.project, &d.project, &s).is_empty());
        // A changed value replays the new one.
        let (d3, _) = apply(
            &d2,
            &Edit::SetPluginParam {
                instance: a,
                param_id: 5,
                value: 0.9,
            },
        )
        .unwrap();
        let back = param_diffs(&d3.project, &d2.project, &s);
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].value, 0.5);
    }

    #[test]
    fn removal_look_ahead() {
        let (d, a, b, t) = with_plugins();
        let c = d.project.channels[0].id;
        assert_eq!(
            removed_instances(&d.project, &[Edit::RemoveChannel { channel: c }]),
            vec![a]
        );
        assert_eq!(
            removed_instances(
                &d.project,
                &[Edit::RemoveInsert {
                    track: t,
                    instance: b
                }]
            ),
            vec![b]
        );
        assert!(removed_instances(&d.project, &[Edit::SetTempo { bpm: 99.0 }]).is_empty());
        // RemoveTrack of a non-master track with inserts.
        let (d2, ids) = apply(&d, &Edit::AddTrack { name: "t".into() }).unwrap();
        let t2 = TrackId(ids[0]);
        let (d2, ids) = apply(
            &d2,
            &Edit::AddInsert {
                track: t2,
                index: 0,
                plugin_id: "x.y".into(),
            },
        )
        .unwrap();
        assert_eq!(
            removed_instances(&d2.project, &[Edit::RemoveTrack { track: t2 }]),
            vec![InstanceId(ids[0])]
        );
        let _ = ChannelId(0);
    }
}
