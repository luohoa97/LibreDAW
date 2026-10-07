// SPDX-License-Identifier: GPL-3.0-or-later
//! Slot allocation (SPEC 4.1, 17.1): every channel and mixer track has a
//! stable slot index for its lifetime. A slot carries a generation; the
//! engine resets a slot's runtime state when the generation it sees in a
//! new `Compiled` differs from the one it holds.
//!
//! The allocation itself is the engine's `Slots`, held through
//! `engine_adapter`. This wrapper adds the lookups the UI needs, including
//! where each CLAP instance sits in the engine's plugin table.
//!
//! Generations come from one counter, so a slot that is freed and reused
//! never repeats an earlier generation. A channel that is removed and then
//! brought back by undo is a new lifetime and gets a fresh slot generation.

use std::collections::HashMap;

use protocol::engine::{ChannelSlot, PluginSlot, SlotGen, TrackSlot};
use protocol::ids::{ChannelId, InstanceId, TrackId};
use protocol::model::{Instrument, Project};

use crate::change::clap_of;
use crate::engine_adapter::Slots;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SlotError {
    /// More entities than slots. `validate` prevents this for any document
    /// `apply()` accepts.
    Full,
}

impl std::fmt::Display for SlotError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "no free slot")
    }
}

impl std::error::Error for SlotError {}

#[derive(Clone, Debug, Default)]
pub struct SlotAllocator {
    pub(crate) inner: Slots,
}

impl SlotAllocator {
    pub fn new() -> SlotAllocator {
        SlotAllocator {
            inner: Slots::new(),
        }
    }

    /// Frees slots of entities that are gone and allocates slots for new
    /// ones. Entities that stay keep their slot and generation.
    pub fn sync(&mut self, p: &Project) -> Result<(), SlotError> {
        self.inner.sync(p).map_err(|_| SlotError::Full)
    }

    pub fn channel_slot(&self, id: ChannelId) -> Option<(ChannelSlot, SlotGen)> {
        self.inner
            .channel_slot(id)
            .map(|s| (s, self.inner.channel_gen(s)))
    }

    pub fn track_slot(&self, id: TrackId) -> Option<(TrackSlot, SlotGen)> {
        self.inner
            .track_slot(id)
            .map(|s| (s, self.inner.track_gen(s)))
    }

    /// Where every CLAP instance of the project sits in the engine's plugin
    /// table. Insert slots depend on position, so inserting before an
    /// existing insert moves that insert to a new slot.
    pub fn plugin_slots(&self, p: &Project) -> HashMap<InstanceId, PluginSlot> {
        let mut m = HashMap::new();
        for c in &p.channels {
            if let (Instrument::Clap(r), Some((slot, _))) = (&c.instrument, self.channel_slot(c.id))
            {
                m.insert(r.instance, PluginSlot::Instrument(slot));
            }
        }
        for t in &p.tracks {
            if let Some((slot, _)) = self.track_slot(t.id) {
                for (i, r) in t
                    .inserts
                    .iter()
                    .enumerate()
                    .filter_map(|(i, x)| clap_of(x).map(|r| (i, r)))
                {
                    m.insert(
                        r.instance,
                        PluginSlot::Insert {
                            track: slot,
                            index: i as u8,
                        },
                    );
                }
            }
        }
        m
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use doc::document::{Document, apply};
    use protocol::consts::TRACK_SLOTS;
    use protocol::edit::{Edit, NewInstrument};
    use protocol::model::SynthParams;
    use std::collections::HashSet;

    /// A small seeded generator: enough edits to churn slots.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
    }

    fn random_edit(r: &mut Rng, d: &Document) -> Edit {
        let p = &d.project;
        match r.next() % 6 {
            0 | 1 => Edit::AddChannel {
                name: "c".into(),
                instrument: NewInstrument::Synth {
                    params: SynthParams::default(),
                },
                root_key: 60,
                track: TrackId::MASTER,
            },
            2 => Edit::AddTrack { name: "t".into() },
            3 if !p.channels.is_empty() => Edit::RemoveChannel {
                channel: p.channels[r.next() as usize % p.channels.len()].id,
            },
            4 if p.tracks.len() > 1 => Edit::RemoveTrack {
                track: p.tracks[1 + r.next() as usize % (p.tracks.len() - 1)].id,
            },
            _ => Edit::AddInsert {
                track: p.tracks[r.next() as usize % p.tracks.len()].id,
                index: 0,
                plugin_id: "a.b".into(),
            },
        }
    }

    fn add_channel(d: &Document) -> (Document, ChannelId) {
        let (d, ids) = apply(
            d,
            &Edit::AddChannel {
                name: "c".into(),
                instrument: NewInstrument::Synth {
                    params: SynthParams::default(),
                },
                root_key: 60,
                track: TrackId::MASTER,
            },
        )
        .unwrap();
        (d, ChannelId(ids[0]))
    }

    #[test]
    fn master_is_slot_zero_from_the_start() {
        let s = SlotAllocator::new();
        assert_eq!(s.track_slot(TrackId::MASTER), Some((TrackSlot::MASTER, 1)));
    }

    #[test]
    fn slots_are_stable_while_the_entity_lives() {
        let mut s = SlotAllocator::new();
        let (d, a) = add_channel(&Document::new());
        s.sync(&d.project).unwrap();
        let first = s.channel_slot(a).unwrap();
        let (d, b) = add_channel(&d);
        s.sync(&d.project).unwrap();
        assert_eq!(s.channel_slot(a), Some(first));
        assert_ne!(s.channel_slot(b).unwrap().0, first.0);
        // Removing b and syncing does not move a.
        let (d, _) = apply(&d, &Edit::RemoveChannel { channel: b }).unwrap();
        s.sync(&d.project).unwrap();
        assert_eq!(s.channel_slot(a), Some(first));
        assert_eq!(s.channel_slot(b), None);
    }

    #[test]
    fn reused_slot_gets_a_new_generation() {
        let mut s = SlotAllocator::new();
        let (d, a) = add_channel(&Document::new());
        s.sync(&d.project).unwrap();
        let (slot_a, gen_a) = s.channel_slot(a).unwrap();
        let (d, _) = apply(&d, &Edit::RemoveChannel { channel: a }).unwrap();
        s.sync(&d.project).unwrap();
        let (d, b) = add_channel(&d);
        s.sync(&d.project).unwrap();
        let (slot_b, gen_b) = s.channel_slot(b).unwrap();
        assert_eq!(slot_a, slot_b, "lowest free slot is reused");
        assert!(gen_b > gen_a);
    }

    #[test]
    fn restored_entity_after_removal_is_a_new_lifetime() {
        let mut s = SlotAllocator::new();
        let (d, a) = add_channel(&Document::new());
        s.sync(&d.project).unwrap();
        let g1 = s.channel_slot(a).unwrap().1;
        s.sync(&Document::new().project).unwrap(); // as after undo
        s.sync(&d.project).unwrap(); // as after redo
        assert!(s.channel_slot(a).unwrap().1 > g1);
    }

    #[test]
    fn track_slots_skip_master_and_fill_up() {
        let mut s = SlotAllocator::new();
        let mut d = Document::new();
        for i in 0..protocol::consts::MAX_TRACKS {
            d = apply(
                &d,
                &Edit::AddTrack {
                    name: format!("t{i}"),
                },
            )
            .unwrap()
            .0;
        }
        s.sync(&d.project).unwrap();
        let slots: HashSet<u16> = d
            .project
            .tracks
            .iter()
            .map(|t| s.track_slot(t.id).unwrap().0.0)
            .collect();
        assert_eq!(slots.len(), TRACK_SLOTS);
        assert!(slots.iter().all(|x| (*x as usize) < TRACK_SLOTS));
    }

    #[test]
    fn plugin_slots_follow_insert_position() {
        let mut s = SlotAllocator::new();
        let d = Document::new();
        let add = |d: &Document, idx: u8| {
            let (d, ids) = apply(
                d,
                &Edit::AddInsert {
                    track: TrackId::MASTER,
                    index: idx,
                    plugin_id: "a.b".into(),
                },
            )
            .unwrap();
            (d, InstanceId(ids[0]))
        };
        let (d, i1) = add(&d, 0);
        s.sync(&d.project).unwrap();
        let m = s.plugin_slots(&d.project);
        assert_eq!(
            m[&i1],
            PluginSlot::Insert {
                track: TrackSlot::MASTER,
                index: 0
            }
        );
        let (d, i2) = add(&d, 0);
        let m = s.plugin_slots(&d.project);
        assert_eq!(
            m[&i2],
            PluginSlot::Insert {
                track: TrackSlot::MASTER,
                index: 0
            }
        );
        assert_eq!(
            m[&i1],
            PluginSlot::Insert {
                track: TrackSlot::MASTER,
                index: 1
            }
        );
    }

    #[test]
    fn random_documents_always_fit_and_map_uniquely() {
        for seed in 1..=15u64 {
            let mut r = Rng(0x1357_9BDF_2468_ACE0 ^ seed.wrapping_mul(0x9E37_79B9_7F4A_7C15));
            let mut d = Document::new();
            let mut s = SlotAllocator::new();
            for _ in 0..300 {
                if let Ok((nd, _)) = apply(&d, &random_edit(&mut r, &d)) {
                    d = nd;
                }
                s.sync(&d.project).unwrap();
                let cs: HashSet<_> = d
                    .project
                    .channels
                    .iter()
                    .map(|c| s.channel_slot(c.id).unwrap().0)
                    .collect();
                assert_eq!(cs.len(), d.project.channels.len());
                let ps = s.plugin_slots(&d.project);
                let idx: HashSet<_> = ps.values().map(|p| p.index()).collect();
                assert_eq!(idx.len(), ps.len(), "plugin slots are distinct");
            }
        }
    }
}
