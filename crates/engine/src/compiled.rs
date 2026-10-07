// SPDX-License-Identifier: GPL-3.0-or-later
//! Slot assignment and the immutable compiled state (SPEC 4.1).

use crate::groove::{ratchet_part, swung_start};
use crate::sampler::SamplerC;
use crate::samples::SampleStore;
use protocol::consts::{MAX_CHANNELS, MAX_CHOKE_GROUP, MAX_SENDS, TRACK_SLOTS};
use protocol::engine::{ChannelSlot, SlotGen, TrackSlot};
use protocol::ids::{ChannelId, PatternId, TrackId};
use protocol::model::{Instrument, Project, Wave};

/// Returned when all 64 channel slots or 32 track slots are taken.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SlotsFull;

impl std::fmt::Display for SlotsFull {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("no free mixer or channel slot")
    }
}

impl std::error::Error for SlotsFull {}

#[derive(Clone, Copy, Debug)]
struct Entry<I: Copy> {
    id: Option<I>,
    gen_: SlotGen,
}

/// Maps entity ids to stable slot indexes with generations (SPEC 4.1).
/// Owned by the GTK thread. A slot's generation changes when the slot is
/// (re)assigned or when `bump_*` is called (instrument kind change); the audio
/// thread resets the slot's runtime state when it sees a new generation.
#[derive(Clone, Debug)]
pub struct Slots {
    channels: Vec<Entry<ChannelId>>,
    tracks: Vec<Entry<TrackId>>,
    counter: SlotGen,
}

impl Default for Slots {
    fn default() -> Slots {
        Slots::new()
    }
}

impl Slots {
    /// Only the master (slot 0) is assigned.
    pub fn new() -> Slots {
        let mut s = Slots {
            channels: vec![Entry { id: None, gen_: 0 }; MAX_CHANNELS],
            tracks: vec![Entry { id: None, gen_: 0 }; TRACK_SLOTS],
            counter: 0,
        };
        s.counter += 1;
        s.tracks[0] = Entry {
            id: Some(TrackId::MASTER),
            gen_: s.counter,
        };
        s
    }

    fn next_gen(&mut self) -> SlotGen {
        self.counter += 1;
        self.counter
    }

    pub fn alloc_channel(&mut self, id: ChannelId) -> Result<ChannelSlot, SlotsFull> {
        if let Some(s) = self.channel_slot(id) {
            return Ok(s);
        }
        let i = self
            .channels
            .iter()
            .position(|e| e.id.is_none())
            .ok_or(SlotsFull)?;
        let g = self.next_gen();
        self.channels[i] = Entry {
            id: Some(id),
            gen_: g,
        };
        Ok(ChannelSlot(i as u16))
    }

    pub fn free_channel(&mut self, id: ChannelId) {
        if let Some(s) = self.channel_slot(id) {
            self.channels[s.0 as usize].id = None;
        }
    }

    pub fn channel_slot(&self, id: ChannelId) -> Option<ChannelSlot> {
        self.channels
            .iter()
            .position(|e| e.id == Some(id))
            .map(|i| ChannelSlot(i as u16))
    }

    /// Generation of an assigned slot, 0 for a free one.
    pub fn channel_gen(&self, s: ChannelSlot) -> SlotGen {
        let e = &self.channels[s.0 as usize];
        if e.id.is_some() { e.gen_ } else { 0 }
    }

    /// Forces a reset of the channel's runtime state on the next swap.
    pub fn bump_channel(&mut self, id: ChannelId) {
        if let Some(s) = self.channel_slot(id) {
            self.channels[s.0 as usize].gen_ = self.next_gen();
        }
    }

    /// The master id always maps to slot 0.
    pub fn alloc_track(&mut self, id: TrackId) -> Result<TrackSlot, SlotsFull> {
        if let Some(s) = self.track_slot(id) {
            return Ok(s);
        }
        let i = self
            .tracks
            .iter()
            .skip(1)
            .position(|e| e.id.is_none())
            .ok_or(SlotsFull)?
            + 1;
        let g = self.next_gen();
        self.tracks[i] = Entry {
            id: Some(id),
            gen_: g,
        };
        Ok(TrackSlot(i as u16))
    }

    pub fn free_track(&mut self, id: TrackId) {
        if id == TrackId::MASTER {
            return;
        }
        if let Some(s) = self.track_slot(id) {
            self.tracks[s.0 as usize].id = None;
        }
    }

    pub fn track_slot(&self, id: TrackId) -> Option<TrackSlot> {
        self.tracks
            .iter()
            .position(|e| e.id == Some(id))
            .map(|i| TrackSlot(i as u16))
    }

    pub fn track_gen(&self, s: TrackSlot) -> SlotGen {
        let e = &self.tracks[s.0 as usize];
        if e.id.is_some() { e.gen_ } else { 0 }
    }

    pub fn bump_track(&mut self, id: TrackId) {
        if let Some(s) = self.track_slot(id) {
            self.tracks[s.0 as usize].gen_ = self.next_gen();
        }
    }

    /// Assigns slots to every channel and track of `project` and frees the
    /// slots of entities that are gone. Existing assignments keep their slot
    /// and generation.
    pub fn sync(&mut self, project: &Project) -> Result<(), SlotsFull> {
        for i in 0..MAX_CHANNELS {
            if let Some(id) = self.channels[i].id
                && project.channel(id).is_none()
            {
                self.channels[i].id = None;
            }
        }
        for i in 1..TRACK_SLOTS {
            if let Some(id) = self.tracks[i].id
                && project.track(id).is_none()
            {
                self.tracks[i].id = None;
            }
        }
        for t in &project.tracks {
            self.alloc_track(t.id)?;
        }
        for c in &project.channels {
            self.alloc_channel(c.id)?;
        }
        Ok(())
    }
}

/// Structural instrument description of a channel (SPEC 17.1).
#[derive(Clone, Debug, PartialEq)]
pub enum InstrumentC {
    Synth {
        osc1: Wave,
        osc2: Wave,
    },
    /// A CLAP instrument; the plugin sits in `PluginSlot::Instrument`.
    Clap,
    /// Sample playback (15.1).
    Sampler(SamplerC),
    /// 808 bass (15.2); silent until the 808 lands.
    Bass808 {
        mono: bool,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct ChannelC {
    pub instrument: InstrumentC,
    pub track: TrackSlot,
    /// 0 = none, else 1 to 16 (15.1).
    pub choke_group: u8,
    /// Key a step plays; the pitched sampler tracks pitch from it.
    pub root_key: u8,
}

/// A send from a track to another, as the audio thread uses it (15.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SendC {
    /// Target track slot.
    pub to: u16,
    pub pre_fader: bool,
    /// Position in `Track::sends`, for `send_control`.
    pub index: u8,
}

/// Per track routing and effects.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TrackC {
    pub sends: Vec<SendC>,
}

/// A note ready for the audio thread. `end` is clamped to the pattern end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NoteC {
    pub start: u32,
    pub end: u32,
    pub id: u32,
    pub key: u8,
    pub vel: u8,
}

#[derive(Clone, Debug)]
pub struct PatternC {
    pub id: PatternId,
    pub len_ticks: u32,
    /// Indexed by channel slot, sorted by `(start, key, id)`.
    pub notes: Vec<Vec<NoteC>>,
    /// Channel slots that have at least one note.
    pub active_slots: Vec<u16>,
}

/// Immutable engine state built off the audio thread (SPEC 4.1). It holds
/// structure only: continuous values live in the `ControlTable` and
/// `ParamTable`.
#[derive(Clone, Debug)]
pub struct Compiled {
    pub sample_rate: f64,
    pub time_sig_num: u8,
    /// 0 for a free slot.
    pub channel_gen: [SlotGen; MAX_CHANNELS],
    pub track_gen: [SlotGen; TRACK_SLOTS],
    pub channels: Vec<Option<ChannelC>>,
    pub tracks_present: [bool; TRACK_SLOTS],
    /// Indexed by track slot.
    pub tracks: Vec<TrackC>,
    /// Present track slots, every track after the tracks that feed it
    /// (sends, sidechain keys, the implicit edge to the master); the master
    /// is last (17.2).
    pub order: Vec<u16>,
    /// Sorted by id.
    pub patterns: Vec<PatternC>,
}

impl Compiled {
    pub fn pattern(&self, id: PatternId) -> Option<&PatternC> {
        self.patterns
            .binary_search_by_key(&id, |p| p.id)
            .ok()
            .map(|i| &self.patterns[i])
    }
}

/// Builds the compiled state. Runs on the compiler thread. Entities without
/// a slot in `slots` are skipped (call `Slots::sync` first).
pub fn compile(project: &Project, slots: &Slots, sample_rate: f64) -> Box<Compiled> {
    compile_with(project, slots, sample_rate, None)
}

/// As `compile`, with the decoded samples of `store`. A sampler whose sample
/// is not in the store (not requested, still loading, failed) compiles as
/// silence; recompile when `SampleStore::poll` reports it.
pub fn compile_with(
    project: &Project,
    slots: &Slots,
    sample_rate: f64,
    store: Option<&SampleStore>,
) -> Box<Compiled> {
    let mut c = Compiled {
        sample_rate,
        time_sig_num: project.time_sig_num.max(1),
        channel_gen: [0; MAX_CHANNELS],
        track_gen: [0; TRACK_SLOTS],
        channels: vec![None; MAX_CHANNELS],
        tracks_present: [false; TRACK_SLOTS],
        tracks: vec![TrackC::default(); TRACK_SLOTS],
        order: Vec::new(),
        patterns: Vec::with_capacity(project.patterns.len()),
    };
    for t in &project.tracks {
        if let Some(s) = slots.track_slot(t.id) {
            c.track_gen[s.0 as usize] = slots.track_gen(s);
            c.tracks_present[s.0 as usize] = true;
            let tc = &mut c.tracks[s.0 as usize];
            for (index, snd) in t.sends.iter().enumerate().take(MAX_SENDS) {
                if let Some(to) = slots.track_slot(snd.to) {
                    tc.sends.push(SendC {
                        to: to.0,
                        pre_fader: snd.pre_fader,
                        index: index as u8,
                    });
                }
            }
        }
    }
    for ch in &project.channels {
        let Some(s) = slots.channel_slot(ch.id) else {
            continue;
        };
        let i = s.0 as usize;
        c.channel_gen[i] = slots.channel_gen(s);
        let track = slots.track_slot(ch.track).unwrap_or(TrackSlot::MASTER);
        let instrument = match &ch.instrument {
            Instrument::Synth(p) => InstrumentC::Synth {
                osc1: p.osc1.wave,
                osc2: p.osc2.wave,
            },
            Instrument::Clap(_) => InstrumentC::Clap,
            Instrument::Sampler(sm) => InstrumentC::Sampler(SamplerC {
                mode: sm.mode,
                reverse: sm.reverse,
                root_key: ch.root_key,
                sample: sm
                    .sample
                    .as_deref()
                    .and_then(|h| store.and_then(|st| st.get(h))),
            }),
            Instrument::Bass808(b) => InstrumentC::Bass808 { mono: b.mono },
        };
        c.channels[i] = Some(ChannelC {
            instrument,
            track,
            choke_group: if ch.choke_group <= MAX_CHOKE_GROUP {
                ch.choke_group
            } else {
                0
            },
            root_key: ch.root_key,
        });
    }
    for p in &project.patterns {
        let len = p.length_ticks().max(1);
        let mut notes: Vec<Vec<NoteC>> = vec![Vec::new(); MAX_CHANNELS];
        let mut active = Vec::new();
        for cn in &p.notes {
            let Some(s) = slots.channel_slot(cn.channel) else {
                continue;
            };
            let root_key = project.channel(cn.channel).map_or(60, |ch| ch.root_key);
            let v = &mut notes[s.0 as usize];
            for n in &cn.notes {
                if n.start >= len || n.key > 127 {
                    continue;
                }
                // Swing moves step notes on odd steps (17.2).
                let step_note = n.is_step_note(root_key, p);
                let start = if step_note {
                    swung_start(n.start, p.step_ticks, p.swing)
                } else {
                    n.start
                };
                let vel = n.vel.clamp(1, 127);
                if n.repeat <= 1 {
                    v.push(NoteC {
                        start,
                        end: (start + n.len).min(len),
                        id: n.id.0,
                        key: n.key,
                        vel,
                    });
                    continue;
                }
                // Ratchets become ordinary notes (17.2).
                for i in 0..n.repeat {
                    let (off, l) = ratchet_part(n.len, n.repeat, i);
                    let st = start + off;
                    if st >= len {
                        break;
                    }
                    v.push(NoteC {
                        start: st,
                        end: (st + l).min(len),
                        id: n.id.0,
                        key: n.key,
                        vel,
                    });
                }
            }
            v.sort_by_key(|n| (n.start, n.key, n.id));
            if !v.is_empty() {
                active.push(s.0);
            }
        }
        active.sort_unstable();
        c.patterns.push(PatternC {
            id: p.id,
            len_ticks: len,
            notes,
            active_slots: active,
        });
    }
    c.patterns.sort_by_key(|p| p.id);
    c.order = topo_order(&c);
    Box::new(c)
}

/// Processing order of the present tracks (17.2): every track comes after
/// the tracks that feed it, the master last. Edges: sends, and the implicit
/// track-to-master edge. `apply()` rejects cycles; a cycle that gets here
/// anyway is broken by slot order instead of dropping tracks.
pub fn topo_order(c: &Compiled) -> Vec<u16> {
    let present: Vec<usize> = (1..TRACK_SLOTS).filter(|&t| c.tracks_present[t]).collect();
    let mut indeg = [0u32; TRACK_SLOTS];
    for &t in &present {
        for s in &c.tracks[t].sends {
            let to = s.to as usize;
            if to != 0 && to != t && c.tracks_present[to] {
                indeg[to] += 1;
            }
        }
    }
    let mut done = [false; TRACK_SLOTS];
    let mut out = Vec::with_capacity(present.len() + 1);
    while out.len() < present.len() {
        // Lowest slot with no unprocessed feeder; on a cycle, lowest left.
        let next = present
            .iter()
            .copied()
            .find(|&t| !done[t] && indeg[t] == 0)
            .or_else(|| present.iter().copied().find(|&t| !done[t]))
            .expect("a track is left");
        done[next] = true;
        out.push(next as u16);
        for s in &c.tracks[next].sends {
            let to = s.to as usize;
            if to != 0 && to != next && c.tracks_present[to] && indeg[to] > 0 {
                indeg[to] -= 1;
            }
        }
    }
    if c.tracks_present[0] {
        out.push(0);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::model::{Mix, Send, Track};
    use std::sync::Arc;

    #[test]
    fn slots_assign_lowest_free_and_bump_generation() {
        let mut s = Slots::new();
        let a = s.alloc_channel(ChannelId(5)).unwrap();
        let b = s.alloc_channel(ChannelId(6)).unwrap();
        assert_eq!((a.0, b.0), (0, 1));
        let g = s.channel_gen(a);
        assert_eq!(s.alloc_channel(ChannelId(5)).unwrap(), a);
        assert_eq!(s.channel_gen(a), g);
        s.free_channel(ChannelId(5));
        assert_eq!(s.channel_gen(a), 0);
        let c = s.alloc_channel(ChannelId(7)).unwrap();
        assert_eq!(c, a);
        assert_ne!(s.channel_gen(c), g, "reuse must change the generation");
        let g2 = s.channel_gen(c);
        s.bump_channel(ChannelId(7));
        assert_ne!(s.channel_gen(c), g2);
    }

    #[test]
    fn master_is_slot_zero_and_tracks_fill_up() {
        let mut s = Slots::new();
        assert_eq!(s.track_slot(TrackId::MASTER), Some(TrackSlot::MASTER));
        for i in 1..=32u32 {
            assert_eq!(s.alloc_track(TrackId(i)).unwrap().0 as u32, i);
        }
        assert_eq!(s.alloc_track(TrackId(99)), Err(SlotsFull));
    }

    fn rtrack(id: u32, sends: &[(u32, bool)]) -> Arc<Track> {
        Arc::new(Track {
            id: TrackId(id),
            name: format!("t{id}"),
            mix: Mix::default(),
            inserts: Vec::new(),
            sends: sends
                .iter()
                .map(|&(to, pre)| Send {
                    to: TrackId(to),
                    level_db: -6.0,
                    pre_fader: pre,
                })
                .collect(),
        })
    }

    fn compiled_of(tracks: Vec<Arc<Track>>) -> Box<Compiled> {
        let mut p = Project::empty();
        p.tracks.extend(tracks);
        let mut slots = Slots::new();
        slots.sync(&p).unwrap();
        compile(&p, &slots, 48000.0)
    }

    #[test]
    fn sends_come_before_their_return_and_master_is_last() {
        // Track 1 sends to return 3 and 2; track 2 is a return that sends to 3.
        let c = compiled_of(vec![
            rtrack(1, &[(2, false), (3, true)]),
            rtrack(2, &[(3, false)]),
            rtrack(3, &[]),
        ]);
        assert_eq!(c.order, vec![1, 2, 3, 0]);
        assert_eq!(c.tracks[1].sends.len(), 2);
        assert_eq!(c.tracks[1].sends[1].to, 3);
        assert!(c.tracks[1].sends[1].pre_fader);
        assert_eq!(c.tracks[1].sends[1].index, 1);
    }

    #[test]
    fn order_respects_edges_against_slot_order() {
        // The return has the lowest slot, the source the highest.
        let c = compiled_of(vec![rtrack(1, &[]), rtrack(2, &[(1, false)])]);
        assert_eq!(c.order, vec![2, 1, 0]);
    }

    #[test]
    fn a_cycle_still_processes_every_track() {
        let c = compiled_of(vec![rtrack(1, &[(2, false)]), rtrack(2, &[(1, false)])]);
        assert_eq!(c.order.len(), 3);
        assert_eq!(*c.order.last().unwrap(), 0);
    }
}
