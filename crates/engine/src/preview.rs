// SPDX-License-Identifier: GPL-3.0-or-later
//! Note preview (audition): notes the user plays by clicking a step or a
//! piano key. They own a note-id range above every sequencer id, are kept in
//! a small fixed table, and auto-release after `PREVIEW_MAX_SECONDS`.

use crate::sequencer::SeqEvent;

/// Preview note ids live in `PREVIEW_ID_BASE..=u32::MAX`.
pub const PREVIEW_ID_BASE: u32 = 0xF000_0000;
/// Simultaneous preview notes (more are dropped).
pub const PREVIEW_CAP: usize = 32;
/// Seconds a previewed channel stays audible past the release, so solo
/// elsewhere does not cut the release tail.
pub const TAIL_SECONDS: f64 = 2.0;

#[derive(Clone, Copy)]
pub struct PreviewNote {
    pub active: bool,
    pub slot: u16,
    pub key: u8,
    pub id: u32,
    /// Frames until the automatic release.
    pub left: u32,
}

pub struct Previews {
    pub notes: [PreviewNote; PREVIEW_CAP],
    next_id: u32,
}

impl Previews {
    pub const fn new() -> Previews {
        Previews {
            notes: [PreviewNote {
                active: false,
                slot: 0,
                key: 0,
                id: 0,
                left: 0,
            }; PREVIEW_CAP],
            next_id: PREVIEW_ID_BASE,
        }
    }

    pub fn fresh_id(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id = if id == u32::MAX {
            PREVIEW_ID_BASE
        } else {
            id + 1
        };
        id
    }

    /// Index of the active note on (`slot`, `key`).
    pub fn find(&self, slot: u16, key: u8) -> Option<usize> {
        self.notes
            .iter()
            .position(|n| n.active && n.slot == slot && n.key == key)
    }

    pub fn free_index(&self) -> Option<usize> {
        self.notes.iter().position(|n| !n.active)
    }

    /// Bit per channel slot with an active preview note.
    pub fn active_slots(&self) -> u64 {
        self.notes
            .iter()
            .filter(|n| n.active)
            .fold(0, |m, n| m | (1u64 << n.slot))
    }
}

impl Default for Previews {
    fn default() -> Previews {
        Previews::new()
    }
}

/// Inserts `e` keeping `events` ordered by offset (after equal offsets).
/// Never grows the vector past its capacity.
pub fn insert_sorted(events: &mut Vec<SeqEvent>, e: SeqEvent) {
    if events.len() >= events.capacity() {
        return;
    }
    let at = events
        .iter()
        .position(|x| x.offset > e.offset)
        .unwrap_or(events.len());
    events.insert(at, e);
}
