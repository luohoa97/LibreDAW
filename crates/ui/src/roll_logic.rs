// SPDX-License-Identifier: GPL-3.0-or-later
//! Piano roll interaction logic (SPEC 11, 13.1 item 4), separate from the
//! widget: hit testing, drag state, and the incremental edits a drag sends.
//!
//! A drag is a gesture: the first edit opens the undo entry and later edits
//! replace it (6). Each update sends only the difference from what was
//! already applied, so the document always matches the pointer and the whole
//! drag is one undo step.

use std::collections::HashSet;

use protocol::consts::MAX_TICK;
use protocol::edit::Edit;
use protocol::ids::NoteId;
use protocol::model::Note;

use crate::view_math::{Viewport, snap_round};

/// Width of the resize zone at a note's right edge, in pixels.
pub const EDGE_PX: f64 = 6.0;
/// Hit radius of a velocity stem, in pixels.
pub const STEM_HIT_PX: f64 = 6.0;
/// Space above and below a stem inside the velocity lane.
pub const VEL_PAD: f64 = 6.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    Body,
    RightEdge,
}

/// Rectangle of a note in widget coordinates: `(x, y, w, h)`.
pub fn note_rect(vp: &Viewport, n: &Note) -> (f64, f64, f64, f64) {
    let x = vp.tick_to_x(n.start as f64);
    let w = (n.len as f64 * vp.px_per_tick).max(2.0);
    (x, vp.key_to_y(n.key as i32), w, vp.row_h)
}

/// The topmost note under a point in the note grid. Later notes in the
/// slice win, matching draw order.
pub fn hit_note(notes: &[Note], vp: &Viewport, x: f64, y: f64) -> Option<(NoteId, Part)> {
    if !vp.in_grid(x, y) {
        return None;
    }
    for n in notes.iter().rev() {
        let (nx, ny, nw, nh) = note_rect(vp, n);
        if x >= nx && x < nx + nw && y >= ny && y < ny + nh {
            let edge = EDGE_PX.min(nw / 3.0);
            let part = if x >= nx + nw - edge {
                Part::RightEdge
            } else {
                Part::Body
            };
            return Some((n.id, part));
        }
    }
    None
}

/// Height of a stem's top for a velocity, in widget coordinates.
pub fn vel_to_y(vp: &Viewport, vel: u8) -> f64 {
    let usable = (vp.vel_h - 2.0 * VEL_PAD).max(1.0);
    vp.vel_top() + VEL_PAD + (1.0 - vel as f64 / 127.0) * usable
}

/// Velocity for a pointer height in the lane, 1 to 127.
pub fn y_to_vel(vp: &Viewport, y: f64) -> u8 {
    let usable = (vp.vel_h - 2.0 * VEL_PAD).max(1.0);
    let f = 1.0 - (y - vp.vel_top() - VEL_PAD) / usable;
    ((f * 127.0).round() as i64).clamp(1, 127) as u8
}

/// The stem nearest to a point in the velocity lane, if any is close.
pub fn hit_stem(notes: &[Note], vp: &Viewport, x: f64, y: f64) -> Option<NoteId> {
    if !vp.in_vel_lane(x, y) {
        return None;
    }
    let mut best: Option<(f64, NoteId)> = None;
    for n in notes {
        let sx = vp.tick_to_x(n.start as f64) + 1.0;
        let d = (sx - x).abs();
        if d <= STEM_HIT_PX && y >= vel_to_y(vp, n.vel) - STEM_HIT_PX {
            // Prefer the closer stem; ties go to the higher velocity so a
            // chord's tallest stem is reachable.
            let better = match best {
                None => true,
                Some((bd, _)) => d < bd,
            };
            if better {
                best = Some((d, n.id));
            }
        }
    }
    best.map(|b| b.1)
}

/// Notes whose rectangle touches the box `(x0, y0)-(x1, y1)`.
pub fn notes_in_box(notes: &[Note], vp: &Viewport, a: (f64, f64), b: (f64, f64)) -> Vec<NoteId> {
    let (lx, hx) = (a.0.min(b.0), a.0.max(b.0));
    let (ly, hy) = (a.1.min(b.1), a.1.max(b.1));
    notes
        .iter()
        .filter(|n| {
            let (nx, ny, nw, nh) = note_rect(vp, n);
            nx < hx && nx + nw > lx && ny < hy && ny + nh > ly
        })
        .map(|n| n.id)
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DragKind {
    Move,
    Resize,
    Velocity,
}

/// An open drag. All amounts are totals since the press.
#[derive(Clone, Debug)]
pub struct Drag {
    pub kind: DragKind,
    pub ids: Vec<NoteId>,
    /// Pointer position at press, in ticks and key rows.
    pub anchor_tick: f64,
    pub anchor_key: i32,
    pub applied_dt: i64,
    pub applied_dkey: i32,
    pub applied_dlen: i64,
    pub applied_vel: Option<u8>,
    /// Bounds of the dragged notes at press, to keep moves inside range.
    pub min_start: u32,
    pub max_start: u32,
    pub min_key: u8,
    pub max_key: u8,
    pub min_len: u32,
    pub max_end: u32,
    pub pattern_ticks: u32,
}

impl Drag {
    /// Starts a drag of `ids` (taken from `notes`).
    pub fn new(
        kind: DragKind,
        ids: Vec<NoteId>,
        notes: &[Note],
        anchor_tick: f64,
        anchor_key: i32,
        pattern_ticks: u32,
    ) -> Drag {
        let set: HashSet<NoteId> = ids.iter().copied().collect();
        let mut d = Drag {
            kind,
            ids,
            anchor_tick,
            anchor_key,
            applied_dt: 0,
            applied_dkey: 0,
            applied_dlen: 0,
            applied_vel: None,
            min_start: u32::MAX,
            max_start: 0,
            min_key: 127,
            max_key: 0,
            min_len: u32::MAX,
            max_end: 0,
            pattern_ticks,
        };
        for n in notes.iter().filter(|n| set.contains(&n.id)) {
            d.min_start = d.min_start.min(n.start);
            d.max_start = d.max_start.max(n.start);
            d.min_key = d.min_key.min(n.key);
            d.max_key = d.max_key.max(n.key);
            d.min_len = d.min_len.min(n.len);
            d.max_end = d.max_end.max(n.start + n.len);
        }
        if d.min_start == u32::MAX {
            d.min_start = 0;
            d.min_len = 1;
        }
        d
    }

    /// The edit that brings the document to the pointer position, or `None`
    /// if nothing changed since the last update.
    pub fn update(&mut self, vp: &Viewport, x: f64, y: f64, snap: u32) -> Option<Edit> {
        match self.kind {
            DragKind::Move => {
                let raw = vp.x_to_tick(x) - self.anchor_tick;
                let lo = -(self.min_start as i64);
                let hi = self.pattern_ticks as i64 - 1 - self.max_start as i64;
                let dt = snap_round(raw, snap).clamp(lo, hi.max(lo));
                let dk_raw = vp.y_to_key(y) - self.anchor_key;
                let dk = dk_raw.clamp(-(self.min_key as i32), 127 - self.max_key as i32);
                let (ddt, ddk) = (dt - self.applied_dt, dk - self.applied_dkey);
                if ddt == 0 && ddk == 0 {
                    return None;
                }
                self.applied_dt = dt;
                self.applied_dkey = dk;
                Some(Edit::MoveNotes {
                    pattern: protocol::ids::PatternId(0), // filled by the caller
                    notes: self.ids.clone(),
                    dt: ddt,
                    dkey: ddk as i16,
                })
            }
            DragKind::Resize => {
                let raw = vp.x_to_tick(x) - self.anchor_tick;
                let lo = -(self.min_len as i64 - 1);
                let hi = (MAX_TICK as i64 - self.max_end as i64).max(lo);
                let dl = snap_round(raw, snap).clamp(lo, hi);
                let d = dl - self.applied_dlen;
                if d == 0 {
                    return None;
                }
                self.applied_dlen = dl;
                Some(Edit::ResizeNotes {
                    pattern: protocol::ids::PatternId(0),
                    notes: self.ids.clone(),
                    dlen: d,
                })
            }
            DragKind::Velocity => {
                let v = y_to_vel(vp, y);
                if self.applied_vel == Some(v) {
                    return None;
                }
                self.applied_vel = Some(v);
                Some(Edit::SetNoteVelocity {
                    pattern: protocol::ids::PatternId(0),
                    notes: self.ids.clone(),
                    vel: v,
                })
            }
        }
    }
}

/// Sets the pattern id of a drag edit (the drag does not know it).
pub fn with_pattern(e: Edit, pattern: protocol::ids::PatternId) -> Edit {
    match e {
        Edit::MoveNotes {
            notes, dt, dkey, ..
        } => Edit::MoveNotes {
            pattern,
            notes,
            dt,
            dkey,
        },
        Edit::ResizeNotes { notes, dlen, .. } => Edit::ResizeNotes {
            pattern,
            notes,
            dlen,
        },
        Edit::SetNoteVelocity { notes, vel, .. } => Edit::SetNoteVelocity {
            pattern,
            notes,
            vel,
        },
        other => other,
    }
}

/// Selection after a click on `hit`: shift toggles, a plain click on an
/// unselected note selects only it, a plain click on a selected note keeps
/// the selection (so a group can be dragged).
pub fn click_selection(current: &[NoteId], hit: NoteId, shift: bool) -> Vec<NoteId> {
    let selected = current.contains(&hit);
    match (shift, selected) {
        (true, true) => current.iter().copied().filter(|n| *n != hit).collect(),
        (true, false) => {
            let mut v = current.to_vec();
            v.push(hit);
            v
        }
        (false, true) => current.to_vec(),
        (false, false) => vec![hit],
    }
}

/// Drops ids that no longer exist.
pub fn prune_selection(selection: &mut Vec<NoteId>, notes: &[Note]) {
    let have: HashSet<NoteId> = notes.iter().map(|n| n.id).collect();
    selection.retain(|n| have.contains(n));
}

/// The note under the keyboard cursor: starts at the cursor tick, same key.
pub fn note_at_cursor(notes: &[Note], tick: u32, key: u8) -> Option<NoteId> {
    notes
        .iter()
        .find(|n| n.key == key && n.start <= tick && tick < n.start + n.len)
        .map(|n| n.id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::ids::PatternId;

    fn note(id: u32, start: u32, len: u32, key: u8, vel: u8) -> Note {
        Note {
            id: NoteId(id),
            start,
            len,
            key,
            vel,
            off: 0,
            repeat: 1,
        }
    }

    fn vp() -> Viewport {
        Viewport {
            px_per_tick: 0.1,
            row_h: 10.0,
            scroll_x: 0.0,
            scroll_y: 600.0,
            key_w: 50.0,
            ruler_h: 20.0,
            vel_h: 60.0,
            width: 800.0,
            height: 700.0,
        }
    }

    #[test]
    fn hits_body_and_right_edge() {
        let v = vp();
        let n = [note(1, 240, 480, 60, 100)];
        let (x, y, w, h) = note_rect(&v, &n[0]);
        assert_eq!(w, 48.0);
        assert_eq!(
            hit_note(&n, &v, x + 10.0, y + h / 2.0),
            Some((NoteId(1), Part::Body))
        );
        assert_eq!(
            hit_note(&n, &v, x + w - 2.0, y + 1.0),
            Some((NoteId(1), Part::RightEdge))
        );
        assert_eq!(hit_note(&n, &v, x + w + 1.0, y + 1.0), None);
        assert_eq!(hit_note(&n, &v, x + 5.0, y - 1.0), None);
        // Not through the keyboard column or the lane.
        assert_eq!(hit_note(&n, &v, 10.0, y + 1.0), None);
    }

    #[test]
    fn tiny_notes_keep_a_body_zone() {
        let v = vp();
        let n = [note(1, 0, 20, 60, 100)]; // 2 px wide, drawn 2 px
        let (x, y, _, _) = note_rect(&v, &n[0]);
        let hit = hit_note(&n, &v, x + 0.5, y + 1.0);
        assert_eq!(hit.map(|h| h.0), Some(NoteId(1)));
    }

    #[test]
    fn overlapping_notes_pick_the_later_one() {
        let v = vp();
        let n = [note(1, 0, 480, 60, 100), note(2, 0, 480, 60, 100)];
        let (x, y, _, _) = note_rect(&v, &n[0]);
        assert_eq!(hit_note(&n, &v, x + 5.0, y + 1.0).unwrap().0, NoteId(2));
    }

    #[test]
    fn velocity_mapping_round_trips() {
        let v = vp();
        for vel in [1u8, 32, 64, 100, 127] {
            assert_eq!(y_to_vel(&v, vel_to_y(&v, vel)), vel);
        }
        assert_eq!(y_to_vel(&v, v.vel_top() - 100.0), 127);
        assert_eq!(y_to_vel(&v, v.vel_top() + 1000.0), 1);
        assert!(vel_to_y(&v, 127) < vel_to_y(&v, 1));
    }

    #[test]
    fn stems_are_picked_by_nearness() {
        let v = vp();
        let n = [note(1, 0, 100, 60, 100), note(2, 100, 100, 62, 50)];
        let y = vel_to_y(&v, 100) + 3.0;
        let sx1 = v.tick_to_x(0.0) + 1.0;
        let sx2 = v.tick_to_x(100.0) + 1.0;
        assert_eq!(hit_stem(&n, &v, sx1, y), Some(NoteId(1)));
        assert_eq!(
            hit_stem(&n, &v, sx2 + 1.0, y.max(vel_to_y(&v, 50) + 3.0)),
            Some(NoteId(2))
        );
        assert_eq!(
            hit_stem(&n, &v, sx1 + 20.0, y),
            None,
            "too far from any stem"
        );
        assert_eq!(
            hit_stem(&n, &v, sx1, v.vel_top() - 5.0),
            None,
            "not in the lane"
        );
    }

    #[test]
    fn box_selection() {
        let v = vp();
        let n = [
            note(1, 0, 100, 60, 100),
            note(2, 2000, 100, 60, 100),
            note(3, 0, 100, 80, 100),
        ];
        let a = (v.tick_to_x(-10.0), v.key_to_y(61));
        let b = (v.tick_to_x(500.0), v.key_to_y(59) + 10.0);
        assert_eq!(notes_in_box(&n, &v, a, b), vec![NoteId(1)]);
        assert_eq!(notes_in_box(&n, &v, b, a), vec![NoteId(1)], "corner order");
    }

    fn drag_of(
        kind: DragKind,
        notes: &[Note],
        ids: &[u32],
        v: &Viewport,
        px: f64,
        py: f64,
    ) -> Drag {
        Drag::new(
            kind,
            ids.iter().map(|i| NoteId(*i)).collect(),
            notes,
            v.x_to_tick(px),
            v.y_to_key(py),
            3840,
        )
    }

    #[test]
    fn move_drag_sends_only_differences_and_snaps() {
        let v = vp();
        let notes = [note(1, 480, 240, 60, 100)];
        let (px, py) = (v.tick_to_x(500.0), v.key_to_y(60) + 3.0);
        let mut d = drag_of(DragKind::Move, &notes, &[1], &v, px, py);
        // No movement yet.
        assert!(d.update(&v, px, py, 240).is_none());
        // Drag 300 ticks right (rounds to 240) and one row up.
        let e = d.update(&v, px + 30.0, py - 10.0, 240).unwrap();
        assert!(matches!(
            e,
            Edit::MoveNotes {
                dt: 240,
                dkey: 1,
                ..
            }
        ));
        // Same position again: nothing.
        assert!(d.update(&v, px + 30.0, py - 10.0, 240).is_none());
        // Further right by one more snap: only the extra 240.
        let e = d.update(&v, px + 54.0, py - 10.0, 240).unwrap();
        assert!(matches!(
            e,
            Edit::MoveNotes {
                dt: 240,
                dkey: 0,
                ..
            }
        ));
        // Back to the start: the negative of everything applied.
        let e = d.update(&v, px, py, 240).unwrap();
        assert!(matches!(
            e,
            Edit::MoveNotes {
                dt: -480,
                dkey: -1,
                ..
            }
        ));
        assert_eq!((d.applied_dt, d.applied_dkey), (0, 0));
    }

    #[test]
    fn move_drag_stays_inside_the_pattern_and_key_range() {
        let v = vp();
        let notes = [note(1, 240, 240, 1, 100), note(2, 3000, 240, 126, 100)];
        let (px, py) = (v.tick_to_x(250.0), v.key_to_y(1) + 3.0);
        let mut d = drag_of(DragKind::Move, &notes, &[1, 2], &v, px, py);
        // Far left and far down: clamped to what keeps both notes legal.
        let e = d.update(&v, px - 5000.0, py + 5000.0, 240).unwrap();
        let Edit::MoveNotes { dt, dkey, .. } = e else {
            panic!()
        };
        assert_eq!(dt, -240, "first note stops at tick 0");
        assert_eq!(dkey, -1, "first note stops at key 0");
        // Far right and up: second note stops before the pattern end / key 127.
        let e = d.update(&v, px + 50000.0, py - 50000.0, 240).unwrap();
        let Edit::MoveNotes { dt, dkey, .. } = e else {
            panic!()
        };
        assert_eq!(d.applied_dt, 3840 - 1 - 3000);
        assert_eq!(d.applied_dkey, 127 - 126);
        assert!(dt > 0 && dkey > 0);
    }

    #[test]
    fn resize_drag_clamps_to_one_tick() {
        let v = vp();
        let notes = [note(1, 0, 480, 60, 100)];
        let (px, py) = (v.tick_to_x(480.0) - 1.0, v.key_to_y(60) + 3.0);
        let mut d = drag_of(DragKind::Resize, &notes, &[1], &v, px, py);
        let e = d.update(&v, px + 24.0, py, 240).unwrap();
        assert!(matches!(e, Edit::ResizeNotes { dlen: 240, .. }));
        let e = d.update(&v, px - 5000.0, py, 240).unwrap();
        // Total clamps to -(len - 1); incremental is that minus 240.
        assert_eq!(d.applied_dlen, -479);
        assert!(matches!(e, Edit::ResizeNotes { dlen, .. } if dlen == -479 - 240));
    }

    #[test]
    fn velocity_drag_sets_absolute_values_once() {
        let v = vp();
        let notes = [note(1, 0, 100, 60, 100), note(2, 100, 100, 60, 40)];
        let mut d = drag_of(
            DragKind::Velocity,
            &notes,
            &[1, 2],
            &v,
            60.0,
            vel_to_y(&v, 100),
        );
        let y = vel_to_y(&v, 80);
        let e = d.update(&v, 60.0, y, 240).unwrap();
        assert!(matches!(e, Edit::SetNoteVelocity { vel: 80, .. }));
        assert!(d.update(&v, 61.0, y, 240).is_none());
        let e = d.update(&v, 60.0, v.vel_top() - 50.0, 240).unwrap();
        assert!(matches!(e, Edit::SetNoteVelocity { vel: 127, .. }));
    }

    #[test]
    fn with_pattern_fills_the_pattern() {
        let e = with_pattern(
            Edit::ResizeNotes {
                pattern: PatternId(0),
                notes: vec![NoteId(1)],
                dlen: 5,
            },
            PatternId(9),
        );
        assert!(matches!(
            e,
            Edit::ResizeNotes {
                pattern: PatternId(9),
                ..
            }
        ));
    }

    #[test]
    fn click_selection_rules() {
        let sel = [NoteId(1), NoteId(2)];
        assert_eq!(click_selection(&sel, NoteId(3), false), vec![NoteId(3)]);
        assert_eq!(click_selection(&sel, NoteId(2), false), sel.to_vec());
        assert_eq!(
            click_selection(&sel, NoteId(3), true),
            vec![NoteId(1), NoteId(2), NoteId(3)]
        );
        assert_eq!(click_selection(&sel, NoteId(1), true), vec![NoteId(2)]);
    }

    #[test]
    fn selection_is_pruned_and_cursor_finds_notes() {
        let notes = [note(1, 0, 240, 60, 100), note(3, 480, 240, 62, 100)];
        let mut sel = vec![NoteId(1), NoteId(2), NoteId(3)];
        prune_selection(&mut sel, &notes);
        assert_eq!(sel, vec![NoteId(1), NoteId(3)]);
        assert_eq!(note_at_cursor(&notes, 100, 60), Some(NoteId(1)));
        assert_eq!(note_at_cursor(&notes, 240, 60), None);
        assert_eq!(note_at_cursor(&notes, 480, 62), Some(NoteId(3)));
        assert_eq!(note_at_cursor(&notes, 480, 61), None);
    }
}
