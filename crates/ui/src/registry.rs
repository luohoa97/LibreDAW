// SPDX-License-Identifier: GPL-3.0-or-later
//! The plugin registry (SPEC 9.1, 7.5): live CLAP instances, owned by the
//! GTK thread, and the attach/detach handshake with the audio thread.
//!
//! Lifecycle of an instance: created and activated here, then
//! `AttachPlugin` on the command ring. To remove it or move it to another
//! slot, send `DetachPlugin`, keep the instance alive, and only when the
//! audio thread's `DetachAck` arrives either attach it somewhere else or
//! drop it. The GTK thread never waits for the ack: `reconcile` is idempotent
//! and runs again after every document change and every ack.
//!
//! `plan` is the pure part (what to do next given what exists and what the
//! document wants) and carries the tests. `Registry` executes the plan with
//! real instances.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use protocol::consts::MAX_BLOCK;
use protocol::engine::{EngineCommand, PluginEvent, PluginSlot};
use protocol::ids::InstanceId;
use protocol::model::{ClapRef, Insert, Instrument, Project};

use crate::document::{next_generation, state_file_name};
use crate::engine_adapter::EngineLink;
use crate::plugin_adapter::{self, HostError, Instance, PluginDesc, PluginOut};
use crate::slots::SlotAllocator;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Created and activated, not known to the audio thread.
    Ready,
    Attached(PluginSlot),
    /// `DetachPlugin` sent; the audio thread may still use the plugin until
    /// the ack.
    Detaching(PluginSlot),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Create(InstanceId),
    Attach(InstanceId, PluginSlot),
    Detach(InstanceId),
    /// Safe to destroy: never attached, or the ack arrived.
    Drop(InstanceId),
}

/// Next steps toward `desired`, given the current phases. At most one
/// instance occupies a slot: an attach waits until the slot's previous
/// occupant has been acknowledged out.
pub fn plan(
    current: &[(InstanceId, Phase)],
    desired: &HashMap<InstanceId, PluginSlot>,
    failed: &HashSet<InstanceId>,
) -> Vec<Action> {
    let mut cur: Vec<_> = current.to_vec();
    cur.sort_by_key(|(id, _)| *id);
    let mut occupied: HashSet<usize> = cur
        .iter()
        .filter_map(|(_, p)| match p {
            Phase::Attached(s) | Phase::Detaching(s) => Some(s.index()),
            Phase::Ready => None,
        })
        .collect();
    let mut out = Vec::new();
    for (id, phase) in &cur {
        match (phase, desired.get(id)) {
            (Phase::Attached(s), d) if d != Some(s) => out.push(Action::Detach(*id)),
            (Phase::Ready, None) => out.push(Action::Drop(*id)),
            (Phase::Ready, Some(s)) => {
                if occupied.insert(s.index()) {
                    out.push(Action::Attach(*id, *s));
                }
            }
            _ => {}
        }
    }
    let have: HashSet<InstanceId> = cur.iter().map(|(id, _)| *id).collect();
    let mut want: Vec<_> = desired.keys().copied().collect();
    want.sort();
    for id in want {
        if !have.contains(&id) && !failed.contains(&id) {
            out.push(Action::Create(id));
        }
    }
    out
}

/// Something the user should hear about.
#[derive(Clone, Debug, PartialEq)]
pub enum Notice {
    /// The plugin is not installed or failed to load. Its reference stays in
    /// the document with its saved state (7.5).
    Unavailable {
        instance: InstanceId,
        plugin_id: String,
        reason: String,
    },
}

struct Rec {
    inst: Instance,
    phase: Phase,
}

pub struct Registry {
    recs: BTreeMap<InstanceId, Rec>,
    catalog: Vec<PluginDesc>,
    sample_rate: f64,
    failed: HashSet<InstanceId>,
}

pub fn clap_ref(p: &Project, id: InstanceId) -> Option<&ClapRef> {
    for c in &p.channels {
        if let Instrument::Clap(r) = &c.instrument
            && r.instance == id
        {
            return Some(r);
        }
    }
    for t in &p.tracks {
        for Insert::Clap(r) in &t.inserts {
            if r.instance == id {
                return Some(r);
            }
        }
    }
    None
}

impl Registry {
    pub fn new(catalog: Vec<PluginDesc>, sample_rate: f64) -> Registry {
        Registry {
            recs: BTreeMap::new(),
            catalog,
            sample_rate,
            failed: HashSet::new(),
        }
    }

    pub fn catalog(&self) -> &[PluginDesc] {
        &self.catalog
    }

    pub fn set_catalog(&mut self, catalog: Vec<PluginDesc>) {
        self.catalog = catalog;
        // Newly installed plugins may now load.
        self.failed.clear();
    }

    pub fn find_desc(&self, plugin_id: &str) -> Option<&PluginDesc> {
        self.catalog.iter().find(|d| d.id == plugin_id)
    }

    pub fn len(&self) -> usize {
        self.recs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.recs.is_empty()
    }

    pub fn phase(&self, id: InstanceId) -> Option<Phase> {
        self.recs.get(&id).map(|r| r.phase)
    }

    pub fn instance_mut(&mut self, id: InstanceId) -> Option<&mut Instance> {
        self.recs.get_mut(&id).map(|r| &mut r.inst)
    }

    /// Which instance sits in a slot (attached or still detaching).
    pub fn instance_at(&self, slot: PluginSlot) -> Option<InstanceId> {
        self.recs.iter().find_map(|(id, r)| match r.phase {
            Phase::Attached(s) | Phase::Detaching(s) if s == slot => Some(*id),
            _ => None,
        })
    }

    /// Creates, attaches, detaches, and drops instances so the registry
    /// matches `project`. Call after every document replacement and after
    /// every `DetachAck`.
    pub fn reconcile(
        &mut self,
        project: &Project,
        slots: &SlotAllocator,
        link: &mut EngineLink,
    ) -> Vec<Notice> {
        let desired = slots.plugin_slots(project);
        let mut notices = Vec::new();
        // Creating makes new `Ready` records that may attach in the same call,
        // so run the plan twice.
        for _ in 0..2 {
            let cur: Vec<_> = self.recs.iter().map(|(i, r)| (*i, r.phase)).collect();
            let actions = plan(&cur, &desired, &self.failed);
            if actions.is_empty() {
                break;
            }
            let mut created = false;
            for a in actions {
                match a {
                    Action::Create(id) => {
                        created = true;
                        if let Some(r) = clap_ref(project, id) {
                            match self.create(r) {
                                Ok(inst) => {
                                    self.recs.insert(
                                        id,
                                        Rec {
                                            inst,
                                            phase: Phase::Ready,
                                        },
                                    );
                                }
                                Err(reason) => {
                                    self.failed.insert(id);
                                    notices.push(Notice::Unavailable {
                                        instance: id,
                                        plugin_id: r.plugin_id.clone(),
                                        reason,
                                    });
                                }
                            }
                        }
                    }
                    Action::Attach(id, slot) => {
                        let h = self.recs[&id].inst.handle();
                        if link
                            .command(EngineCommand::AttachPlugin { slot, handle: h })
                            .is_ok()
                        {
                            self.recs.get_mut(&id).expect("rec").phase = Phase::Attached(slot);
                        }
                    }
                    Action::Detach(id) => {
                        if let Phase::Attached(slot) = self.recs[&id].phase
                            && link.command(EngineCommand::DetachPlugin { slot }).is_ok()
                        {
                            self.recs.get_mut(&id).expect("rec").phase = Phase::Detaching(slot);
                        }
                    }
                    Action::Drop(id) => {
                        self.recs.remove(&id);
                        self.failed.remove(&id);
                    }
                }
            }
            if !created {
                break;
            }
        }
        // Forget failures of instances that left the document.
        self.failed.retain(|id| desired.contains_key(id));
        notices
    }

    fn create(&self, r: &ClapRef) -> Result<Instance, String> {
        let desc = self
            .find_desc(&r.plugin_id)
            .ok_or_else(|| format!("plugin {} is not installed", r.plugin_id))?;
        let mut inst = Instance::create(desc).map_err(|e: HostError| e.to_string())?;
        inst.activate(self.sample_rate, MAX_BLOCK as u32)
            .map_err(|e| e.to_string())?;
        if let Some(bytes) = &r.state_bytes {
            // A state the plugin rejects is not fatal: it starts from its
            // defaults and the user sees the notice.
            inst.load_state(bytes).map_err(|e| e.to_string())?;
        }
        if !r.params.is_empty() {
            let evs: Vec<PluginEvent> = r
                .params
                .iter()
                .map(|v| PluginEvent {
                    slot: PluginSlot::Instrument(protocol::engine::ChannelSlot(0)),
                    param_id: v.id,
                    value: v.value,
                })
                .collect();
            inst.flush_params(&evs);
            // Echoes from the flush are not user changes.
            let _ = plugin_adapter::take_changes(&mut inst);
        }
        Ok(inst)
    }

    /// The audio thread has let go of the plugin in `slot`. Returns true if
    /// a record was waiting for it, so the caller should reconcile.
    pub fn on_detach_ack(&mut self, slot: PluginSlot) -> bool {
        for r in self.recs.values_mut() {
            if r.phase == Phase::Detaching(slot) {
                r.phase = Phase::Ready;
                return true;
            }
        }
        false
    }

    /// Runs the 10 ms housekeeping (4.4, 9.1): `poll_main_thread` on every
    /// instance, and collects what plugins reported outside processing.
    pub fn poll(&mut self) -> PollResult {
        let mut out = PollResult::default();
        for (id, r) in &mut self.recs {
            r.inst.poll_main_thread();
            for o in plugin_adapter::take_changes(&mut r.inst) {
                out.changes.push((*id, o));
            }
            if r.inst.take_dirty() {
                out.dirty.push(*id);
            }
        }
        out
    }

    /// Reads every parameter value from every instance (4.4: after the
    /// `EventRing` overflowed).
    pub fn read_all_params(&mut self) -> Vec<(InstanceId, u32, f64, f64)> {
        let mut out = Vec::new();
        for (id, r) in &mut self.recs {
            for p in r.inst.params() {
                if let Some(v) = r.inst.param_value(p.id) {
                    out.push((*id, p.id, v, p.default));
                }
            }
        }
        out
    }

    /// Captures fresh state (`state.save`) for one instance (7.5). Returns
    /// the new blob name and bytes if the bytes differ from the document's
    /// last capture, else `None`. `Ok(None)` also for an instance this
    /// registry does not have (a placeholder keeps its old blob).
    pub fn capture(
        &mut self,
        id: InstanceId,
        current: &ClapRef,
    ) -> Result<Option<Captured>, HostError> {
        let Some(r) = self.recs.get_mut(&id) else {
            return Ok(None);
        };
        let bytes = r.inst.save_state()?;
        if current.state_bytes.as_deref() == Some(bytes.as_slice()) {
            return Ok(None);
        }
        Ok(Some((
            state_file_name(id, next_generation(current)),
            Arc::from(bytes),
        )))
    }

    pub fn show_gui(&mut self, id: InstanceId, title: &str) -> Result<(), HostError> {
        match self.recs.get_mut(&id) {
            Some(r) => r.inst.show_gui(title),
            None => Err(HostError::NotActive),
        }
    }

    pub fn gui_open(&self, id: InstanceId) -> bool {
        self.recs.get(&id).is_some_and(|r| r.inst.gui_open())
    }

    /// Destroys every instance. Only after the audio stream has been
    /// stopped (4.7), so nothing is still processing.
    pub fn drop_all(&mut self) {
        self.recs.clear();
    }
}

/// A new state blob: its file name and bytes.
pub type Captured = (String, Arc<[u8]>);

#[derive(Debug, Default)]
pub struct PollResult {
    /// Parameter values and gestures plugins reported.
    pub changes: Vec<(InstanceId, PluginOut)>,
    /// Plugins that called `state.mark_dirty`.
    pub dirty: Vec<InstanceId>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::engine::{ChannelSlot, TrackSlot};

    fn ch(i: u16) -> PluginSlot {
        PluginSlot::Instrument(ChannelSlot(i))
    }
    fn ins(t: u16, i: u8) -> PluginSlot {
        PluginSlot::Insert {
            track: TrackSlot(t),
            index: i,
        }
    }
    fn id(i: u32) -> InstanceId {
        InstanceId(i)
    }
    fn want(v: &[(u32, PluginSlot)]) -> HashMap<InstanceId, PluginSlot> {
        v.iter().map(|(i, s)| (id(*i), *s)).collect()
    }

    #[test]
    fn new_instances_are_created_then_attached() {
        let d = want(&[(1, ch(0)), (2, ins(0, 0))]);
        let a = plan(&[], &d, &HashSet::new());
        assert_eq!(a, vec![Action::Create(id(1)), Action::Create(id(2))]);
        let cur = [(id(1), Phase::Ready), (id(2), Phase::Ready)];
        let a = plan(&cur, &d, &HashSet::new());
        assert_eq!(
            a,
            vec![
                Action::Attach(id(1), ch(0)),
                Action::Attach(id(2), ins(0, 0))
            ]
        );
        let cur = [
            (id(1), Phase::Attached(ch(0))),
            (id(2), Phase::Attached(ins(0, 0))),
        ];
        assert!(plan(&cur, &d, &HashSet::new()).is_empty());
    }

    #[test]
    fn removal_detaches_waits_for_the_ack_then_drops() {
        let d = want(&[]);
        let cur = [(id(1), Phase::Attached(ch(0)))];
        assert_eq!(plan(&cur, &d, &HashSet::new()), vec![Action::Detach(id(1))]);
        // Detaching: nothing to do until the ack.
        let cur = [(id(1), Phase::Detaching(ch(0)))];
        assert!(plan(&cur, &d, &HashSet::new()).is_empty());
        // After the ack the record is Ready and not wanted.
        let cur = [(id(1), Phase::Ready)];
        assert_eq!(plan(&cur, &d, &HashSet::new()), vec![Action::Drop(id(1))]);
    }

    #[test]
    fn a_moved_insert_detaches_then_reattaches_in_its_new_slot() {
        // Insert 1 was at index 0; inserting 2 before it pushes it to index 1.
        let d = want(&[(2, ins(0, 0)), (1, ins(0, 1))]);
        let cur = [(id(1), Phase::Attached(ins(0, 0)))];
        let a = plan(&cur, &d, &HashSet::new());
        assert_eq!(a, vec![Action::Detach(id(1)), Action::Create(id(2))]);
        // 2 is created but cannot take slot (0,0) while 1 may still use it.
        let cur = [(id(1), Phase::Detaching(ins(0, 0))), (id(2), Phase::Ready)];
        assert!(plan(&cur, &d, &HashSet::new()).is_empty());
        // Ack: 1 is Ready. Both can attach, to different slots.
        let cur = [(id(1), Phase::Ready), (id(2), Phase::Ready)];
        let a = plan(&cur, &d, &HashSet::new());
        assert_eq!(
            a,
            vec![
                Action::Attach(id(1), ins(0, 1)),
                Action::Attach(id(2), ins(0, 0))
            ]
        );
    }

    #[test]
    fn a_reused_slot_waits_for_the_old_occupant() {
        // Channel removed, new channel takes its slot at once.
        let d = want(&[(9, ch(3))]);
        let cur = [(id(4), Phase::Detaching(ch(3))), (id(9), Phase::Ready)];
        assert!(plan(&cur, &d, &HashSet::new()).is_empty());
        let cur = [(id(4), Phase::Ready), (id(9), Phase::Ready)];
        let a = plan(&cur, &d, &HashSet::new());
        assert_eq!(a, vec![Action::Drop(id(4)), Action::Attach(id(9), ch(3))]);
    }

    #[test]
    fn two_ready_instances_never_claim_the_same_slot() {
        let d = want(&[(1, ch(0)), (2, ch(0))]);
        let cur = [(id(1), Phase::Ready), (id(2), Phase::Ready)];
        let a = plan(&cur, &d, &HashSet::new());
        assert_eq!(a, vec![Action::Attach(id(1), ch(0))]);
    }

    #[test]
    fn failed_instances_are_not_retried() {
        let d = want(&[(1, ch(0))]);
        let failed: HashSet<_> = [id(1)].into();
        assert!(plan(&[], &d, &failed).is_empty());
    }

    #[test]
    fn undo_of_removal_creates_the_instance_again() {
        // Gone from the registry, wanted by the document again.
        let d = want(&[(1, ch(0))]);
        assert_eq!(plan(&[], &d, &HashSet::new()), vec![Action::Create(id(1))]);
    }

    #[test]
    fn registry_without_plugins_reconciles_to_nothing() {
        use crate::document::Document;
        let mut reg = Registry::new(Vec::new(), 48000.0);
        let mut slots = SlotAllocator::new();
        let d = Document::new();
        slots.sync(&d.project).unwrap();
        let mut link = EngineLink::stub(48000.0);
        assert!(reg.reconcile(&d.project, &slots, &mut link).is_empty());
        assert!(link.commands.is_empty());
        assert!(reg.is_empty());
    }

    #[test]
    fn missing_plugin_becomes_a_notice_once() {
        use crate::document::{Document, apply};
        use protocol::edit::{Edit, NewInstrument};
        use protocol::ids::TrackId;
        let (d, _) = apply(
            &Document::new(),
            &Edit::AddChannel {
                name: "p".into(),
                instrument: NewInstrument::Clap {
                    plugin_id: "not.installed".into(),
                },
                root_key: 60,
                track: TrackId::MASTER,
            },
        )
        .unwrap();
        let mut reg = Registry::new(Vec::new(), 48000.0);
        let mut slots = SlotAllocator::new();
        slots.sync(&d.project).unwrap();
        let mut link = EngineLink::stub(48000.0);
        let n = reg.reconcile(&d.project, &slots, &mut link);
        assert_eq!(n.len(), 1);
        assert!(
            matches!(&n[0], Notice::Unavailable { plugin_id, .. } if plugin_id == "not.installed")
        );
        // Not retried every tick.
        assert!(reg.reconcile(&d.project, &slots, &mut link).is_empty());
        assert!(link.commands.is_empty());
        // The reference stays in the document untouched.
        assert!(clap_ref(&d.project, InstanceId(2)).is_some());
    }
}
