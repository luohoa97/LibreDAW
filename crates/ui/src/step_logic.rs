// SPDX-License-Identifier: GPL-3.0-or-later
//! Layout and cursor logic of the step grid (SPEC 5.2, 11), without GTK.

use protocol::model::{Channel, Note, Pattern};

/// Where things are in the step grid. Rows are channels, columns steps.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StepLayout {
    pub name_w: f64,
    /// Space before the first cell (after the name column, if any).
    pub left_pad: f64,
    pub header_h: f64,
    pub row_h: f64,
    pub cell_w: f64,
    pub cell_gap: f64,
    /// Extra space after every group of `group` steps (a beat).
    pub group_gap: f64,
    pub group: u32,
    /// Extra space after every `bar` steps (on top of the beat gap); 0 for
    /// none.
    pub bar_gap: f64,
    pub bar: u32,
}

impl Default for StepLayout {
    fn default() -> StepLayout {
        StepLayout {
            name_w: 150.0,
            left_pad: 6.0,
            header_h: 20.0,
            row_h: 30.0,
            cell_w: 26.0,
            cell_gap: 3.0,
            group_gap: 8.0,
            group: 4,
            bar_gap: 0.0,
            bar: 16,
        }
    }
}

impl StepLayout {
    /// The clip editor's Steps view (owner visual spec): no name area,
    /// cells 32 high (44 for touch) and at least 28 wide (36 for touch),
    /// 4 px between cells, 10 px between beats and 16 px between bars, a
    /// ruler for the beat numbers. Spacing groups beats; no lines.
    pub fn cells_only(row_h: f64, touch: bool) -> StepLayout {
        StepLayout {
            name_w: 0.0,
            left_pad: 4.0,
            header_h: RULER_H,
            row_h,
            cell_w: if touch { 36.0 } else { 28.0 },
            cell_gap: 4.0,
            group_gap: 6.0,
            group: 4,
            bar_gap: 6.0,
            bar: 16,
        }
    }

    /// The layout with cells widened to fill `width` (docs/ui-design.md
    /// 4.2), never below the base width and never above `MAX_CELL_W`.
    pub fn fitted(&self, width: f64, steps: u32) -> StepLayout {
        if steps == 0 {
            return *self;
        }
        let groups = (steps - 1) / self.group.max(1);
        let bars = (steps - 1).checked_div(self.bar).unwrap_or(0);
        let avail = width
            - self.name_w
            - self.left_pad
            - 8.0
            - groups as f64 * self.group_gap
            - bars as f64 * self.bar_gap;
        let cw = (avail - (steps - 1) as f64 * self.cell_gap) / steps as f64;
        StepLayout {
            cell_w: cw.clamp(self.cell_w, MAX_CELL_W.max(self.cell_w)),
            ..*self
        }
    }

    /// Height of a cell inside a row: the row less 8 px of air.
    pub fn cell_h(&self) -> f64 {
        (self.row_h - 8.0).max(8.0)
    }
}

/// The layout the step grid and the lane editor both use at `width`: base
/// cells for the size class, widened to fill the width.
pub fn grid_layout(row_h: f64, touch: bool, width: f64, steps: u32) -> StepLayout {
    StepLayout::cells_only(row_h, touch).fitted(width, steps)
}

/// Row height of the Steps view: 32 px cells (44 for touch) and 8 px of
/// air.
pub fn row_h(touch: bool) -> f64 {
    if touch { 52.0 } else { 40.0 }
}

/// Where in a clip content of `len` ticks the playhead is: the first clip
/// that plays `pattern` and covers `tick` gives the position (its start
/// and offset). `None` when no such clip is playing there.
pub fn content_pos(
    clips: &[protocol::model::Clip],
    pattern: protocol::ids::PatternId,
    len: u32,
    tick: u64,
) -> Option<u32> {
    if len == 0 {
        return None;
    }
    clips
        .iter()
        .find(|c| c.pattern == pattern && (c.start as u64) <= tick && tick < c.end() as u64)
        .map(|c| ((tick - c.start as u64 + c.offset as u64) % len as u64) as u32)
}

/// How a step cell is filled (owner visual spec): the part of the cell
/// height filled with the instrument color, from the bottom: the whole
/// cell at velocity 127, about a third at 1.
pub fn vel_fill(vel: u8) -> f64 {
    0.35 + 0.65 * (vel.clamp(1, 127) as f64 - 1.0) / 126.0
}

/// An accent dot marks loud steps.
pub fn is_accent(vel: u8) -> bool {
    vel >= 120
}

/// Height of the step ruler (and of the spacer above the header column).
pub const RULER_H: f64 = 24.0;

/// Widest a step cell grows when the editor has room (owner visual spec).
pub const MAX_CELL_W: f64 = 44.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    None,
    /// The channel name area of a row.
    Name(usize),
    Cell {
        row: usize,
        step: u32,
    },
}

impl StepLayout {
    /// Left edge of a step's cell.
    pub fn cell_x(&self, step: u32) -> f64 {
        let groups = (step / self.group.max(1)) as f64;
        let bars = step.checked_div(self.bar).unwrap_or(0) as f64;
        self.name_w
            + self.left_pad
            + step as f64 * (self.cell_w + self.cell_gap)
            + groups * self.group_gap
            + bars * self.bar_gap
    }

    /// X position of a fractional step (for the playhead).
    pub fn step_pos_x(&self, step: f64) -> f64 {
        let whole = step.floor().max(0.0);
        let frac = step - whole;
        self.cell_x(whole as u32) + frac * (self.cell_w + self.cell_gap)
    }

    pub fn row_y(&self, row: usize) -> f64 {
        self.header_h + row as f64 * self.row_h
    }

    /// Size needed for `rows` channels and `steps` steps.
    pub fn content_size(&self, rows: usize, steps: u32) -> (f64, f64) {
        let w = if steps == 0 {
            self.name_w
        } else {
            self.cell_x(steps - 1) + self.cell_w + 8.0
        };
        (w, self.row_y(rows) + 4.0)
    }

    pub fn hit(&self, x: f64, y: f64, rows: usize, steps: u32) -> Hit {
        if y < self.header_h || x < 0.0 {
            return Hit::None;
        }
        let row = ((y - self.header_h) / self.row_h) as usize;
        if row >= rows {
            return Hit::None;
        }
        if x < self.name_w {
            return Hit::Name(row);
        }
        // Binary search is overkill for 64 steps; scan.
        for s in 0..steps {
            let x0 = self.cell_x(s);
            if x >= x0 && x < x0 + self.cell_w {
                return Hit::Cell { row, step: s };
            }
        }
        Hit::None
    }

    /// The step whose column is nearest to `x`, for painting across gaps.
    pub fn nearest_step(&self, x: f64, steps: u32) -> Option<u32> {
        if steps == 0 {
            return None;
        }
        let mut best = (f64::MAX, 0);
        for s in 0..steps {
            let c = self.cell_x(s) + self.cell_w / 2.0;
            let d = (c - x).abs();
            if d < best.0 {
                best = (d, s);
            }
        }
        Some(best.1)
    }
}

/// State of one step cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cell {
    Off,
    /// A step note with its lanes: velocity, pitch offset in semitones, and
    /// ratchet count (1 = no ratchet).
    On {
        vel: u8,
        off: i8,
        repeat: u8,
    },
}

impl Cell {
    /// A plain step: no pitch offset, no ratchet.
    pub fn on(vel: u8) -> Cell {
        Cell::On {
            vel,
            off: 0,
            repeat: 1,
        }
    }

    pub fn is_on(&self) -> bool {
        matches!(self, Cell::On { .. })
    }
}

/// A row as the grid shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RowView {
    pub cells: Vec<Cell>,
    /// Some note of this channel in this pattern is not a step note (5.2):
    /// the row is read-only and shows the "piano roll data" marker.
    pub piano_roll_data: bool,
}

/// The notes `channel` has in `pattern`: all of them when the content is
/// that instrument's (SPEC 20.2), none otherwise.
pub fn notes_for(pattern: &Pattern, channel: protocol::ids::ChannelId) -> &[Note] {
    if pattern.instrument == channel {
        &pattern.notes
    } else {
        &[]
    }
}

/// Builds the row of `channel` in `pattern` from the document's notes.
pub fn row_view(pattern: &Pattern, channel: &Channel) -> RowView {
    let mut cells = vec![Cell::Off; pattern.length_steps as usize];
    let mut foreign = false;
    for n in notes_for(pattern, channel.id) {
        if n.is_step_note(channel.root_key, pattern) {
            let i = (n.start / pattern.step_ticks) as usize;
            if let Some(c) = cells.get_mut(i) {
                *c = match *c {
                    Cell::On { vel, off, repeat } => Cell::On {
                        vel: vel.max(n.vel),
                        off,
                        repeat,
                    },
                    Cell::Off => Cell::On {
                        vel: n.vel,
                        off: n.off,
                        repeat: n.repeat.max(1),
                    },
                };
            }
        } else {
            foreign = true;
        }
    }
    RowView {
        cells,
        piano_roll_data: foreign,
    }
}

/// Description for the accessible label of the cursor cell.
pub fn cell_label(channel_name: &str, step: u32, cell: Cell, read_only: bool) -> String {
    let state = match cell {
        Cell::Off => "off".to_string(),
        Cell::On { vel, off, repeat } => {
            let mut s = format!("on, volume {vel}");
            if off != 0 {
                s.push_str(&format!(", pitch {off:+} semitones"));
            }
            if repeat > 1 {
                s.push_str(&format!(", repeats {repeat}"));
            }
            s
        }
    };
    let ro = if read_only {
        ", read only, notes from Piano"
    } else {
        ""
    };
    format!("{channel_name}, square {}, {state}{ro}", step + 1)
}

/// Moves a cursor by arrow keys, staying inside the grid.
pub fn move_cursor(
    cursor: (usize, u32),
    d_row: i32,
    d_step: i32,
    rows: usize,
    steps: u32,
) -> (usize, u32) {
    if rows == 0 || steps == 0 {
        return (0, 0);
    }
    let r = (cursor.0 as i32 + d_row).clamp(0, rows as i32 - 1) as usize;
    let s = (cursor.1 as i32 + d_step).clamp(0, steps as i32 - 1) as u32;
    (r, s)
}

/// Notes of a channel in a pattern, as a slice (shared helper).
pub fn channel_notes(pattern: &Pattern, channel: &Channel) -> Vec<Note> {
    notes_for(pattern, channel.id).to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::ids::{ChannelId, NoteId, PatternId, TrackId};
    use protocol::model::{Instrument, Mix, SynthParams};

    fn chan(root: u8) -> Channel {
        Channel {
            id: ChannelId(1),
            name: "Kick".into(),
            root_key: root,
            track: TrackId::MASTER,
            mix: Mix::default(),
            instrument: Instrument::Synth(SynthParams::default()),
            choke_group: 0,
        }
    }

    fn pat(notes: Vec<Note>) -> Pattern {
        let mut p = Pattern::new(PatternId(2), "A".into(), ChannelId(1));
        p.notes = notes;
        p
    }

    fn n(id: u32, start: u32, len: u32, key: u8, vel: u8) -> Note {
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

    #[test]
    fn cells_are_spaced_with_group_gaps() {
        let l = StepLayout::default();
        let d01 = l.cell_x(1) - l.cell_x(0);
        let d34 = l.cell_x(4) - l.cell_x(3);
        assert_eq!(d01, l.cell_w + l.cell_gap);
        assert_eq!(d34, l.cell_w + l.cell_gap + l.group_gap);
    }

    #[test]
    fn cells_only_layout_has_no_name_column() {
        let l = StepLayout::cells_only(40.0, false);
        assert_eq!(l.cell_x(0), 4.0);
        assert_eq!(l.header_h, RULER_H);
        assert_eq!(l.row_y(0), RULER_H);
        // A cell fits inside its row with room for the gap.
        assert!(l.cell_h() + 4.0 <= l.row_h);
        assert!(matches!(
            l.hit(l.cell_x(2) + 1.0, RULER_H + 5.0, 3, 16),
            Hit::Cell { row: 0, step: 2 }
        ));
        // Touch cells are bigger.
        assert!(StepLayout::cells_only(44.0, true).cell_w > l.cell_w);
        // Groups of four steps are spaced apart.
        assert!(l.cell_x(4) - l.cell_x(3) > l.cell_w + l.cell_gap);
    }

    #[test]
    fn cells_grow_to_fill_the_width_within_limits() {
        let base = StepLayout::cells_only(40.0, false);
        let (w, _) = base.content_size(1, 16);
        // No spare room: the base width.
        assert_eq!(base.fitted(w, 16).cell_w, base.cell_w);
        // Plenty of room: capped.
        assert_eq!(base.fitted(5000.0, 16).cell_w, MAX_CELL_W);
        // In between: the content ends at the available width.
        let f = base.fitted(w + 160.0, 16);
        assert!(f.cell_w > base.cell_w && f.cell_w < MAX_CELL_W);
        let (fw, _) = f.content_size(1, 16);
        assert!((fw - (w + 160.0)).abs() < 1e-6);
        // Too narrow never shrinks below the base.
        assert_eq!(base.fitted(10.0, 16).cell_w, base.cell_w);
        assert_eq!(base.fitted(500.0, 0), base);
    }

    #[test]
    fn hit_testing() {
        let l = StepLayout::default();
        let (rows, steps) = (3, 16);
        assert_eq!(l.hit(10.0, 5.0, rows, steps), Hit::None, "header");
        assert_eq!(l.hit(10.0, l.row_y(1) + 3.0, rows, steps), Hit::Name(1));
        let x = l.cell_x(5) + 2.0;
        assert_eq!(
            l.hit(x, l.row_y(2) + 3.0, rows, steps),
            Hit::Cell { row: 2, step: 5 }
        );
        // The gap between cells hits nothing.
        let gap = l.cell_x(0) + l.cell_w + 1.0;
        assert_eq!(l.hit(gap, l.row_y(0) + 3.0, rows, steps), Hit::None);
        assert_eq!(l.hit(x, l.row_y(3) + 3.0, rows, steps), Hit::None, "below");
        assert_eq!(
            l.hit(l.cell_x(16) + 1.0, l.row_y(0) + 3.0, rows, steps),
            Hit::None,
            "past the last step"
        );
    }

    #[test]
    fn nearest_step_bridges_gaps() {
        let l = StepLayout::default();
        let x = l.cell_x(3) + l.cell_w + 1.0; // in the group gap after step 3
        assert_eq!(l.nearest_step(x, 16), Some(3));
        assert_eq!(l.nearest_step(-100.0, 16), Some(0));
        assert_eq!(l.nearest_step(1e6, 16), Some(15));
        assert_eq!(l.nearest_step(10.0, 0), None);
    }

    #[test]
    fn content_size_grows_with_rows_and_steps() {
        let l = StepLayout::default();
        let (w16, h2) = l.content_size(2, 16);
        let (w32, h5) = l.content_size(5, 32);
        assert!(w32 > w16 && h5 > h2);
        assert!(w16 > l.cell_x(15));
        assert_eq!(l.content_size(0, 0).0, l.name_w);
    }

    #[test]
    fn playhead_position_is_continuous_inside_a_step() {
        let l = StepLayout::default();
        assert_eq!(l.step_pos_x(2.0), l.cell_x(2));
        let mid = l.step_pos_x(2.5);
        assert!(mid > l.cell_x(2) && mid < l.cell_x(3));
    }

    #[test]
    fn row_shows_step_notes_and_marks_foreign_notes() {
        let c = chan(36);
        let p = pat(vec![n(1, 0, 240, 36, 90), n(2, 480, 240, 36, 120)]);
        let r = row_view(&p, &c);
        assert_eq!(r.cells.len(), 16);
        assert_eq!(r.cells[0], Cell::on(90));
        assert_eq!(r.cells[1], Cell::Off);
        assert_eq!(r.cells[2], Cell::on(120));
        assert!(!r.piano_roll_data);

        // A note of another length or key makes the row read-only (5.2).
        for extra in [
            n(3, 5, 240, 36, 100),
            n(3, 0, 100, 36, 100),
            n(3, 0, 240, 40, 100),
        ] {
            let p = pat(vec![n(1, 480, 240, 36, 90), extra]);
            assert!(row_view(&p, &c).piano_roll_data, "{extra:?}");
        }
    }

    #[test]
    fn row_stays_editable_after_root_key_and_step_changes() {
        // 5.2 test, seen from the view: the same steps stay on.
        use doc::document::{Document, apply};
        use protocol::edit::{Edit, NewInstrument};
        let d = Document::new();
        let (d, ids) = apply(
            &d,
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
        let ch = ChannelId(ids[0]);
        let (mut d, ids) = apply(
            &d,
            &Edit::AddPattern {
                instrument: ch,
                name: "p".into(),
                length_steps: 16,
            },
        )
        .unwrap();
        let pat_id = PatternId(ids[0]);
        for s in [0u8, 4, 7] {
            d = apply(
                &d,
                &Edit::SetStep {
                    pattern: pat_id,
                    step: s,
                    on: true,
                    vel: None,
                },
            )
            .unwrap()
            .0;
        }
        let on = |d: &Document| {
            let p = d.project.pattern(pat_id).unwrap();
            let c = d.project.channel(ch).unwrap();
            let r = row_view(p, c);
            assert!(!r.piano_roll_data);
            r.cells
                .iter()
                .enumerate()
                .filter(|(_, c)| matches!(c, Cell::On { .. }))
                .map(|(i, _)| i)
                .collect::<Vec<_>>()
        };
        assert_eq!(on(&d), vec![0, 4, 7]);
        d = apply(
            &d,
            &Edit::SetRootKey {
                channel: ch,
                key: 38,
            },
        )
        .unwrap()
        .0;
        assert_eq!(on(&d), vec![0, 4, 7]);
        d = apply(
            &d,
            &Edit::SetStepTicks {
                pattern: pat_id,
                step_ticks: 120,
            },
        )
        .unwrap()
        .0;
        assert_eq!(on(&d), vec![0, 4, 7]);
    }

    #[test]
    fn row_shows_the_step_lanes() {
        use doc::document::{Document, apply_batch};
        use protocol::edit::{Edit, NewInstrument};
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
        let (ch, pat) = (ChannelId(ids[0]), PatternId(ids[1]));
        let (d, _) = apply_batch(
            &d,
            &[
                Edit::SetStep {
                    pattern: pat,
                    step: 2,
                    on: true,
                    vel: Some(90),
                },
                Edit::SetStepLanes {
                    pattern: pat,
                    step: 2,
                    vel: Some(64),
                    off: Some(-5),
                    repeat: Some(4),
                },
            ],
        )
        .unwrap();
        let r = row_view(
            d.project.pattern(pat).unwrap(),
            d.project.channel(ch).unwrap(),
        );
        assert_eq!(
            r.cells[2],
            Cell::On {
                vel: 64,
                off: -5,
                repeat: 4
            }
        );
        assert!(!r.piano_roll_data, "a pitched step is still a step note");
        assert_eq!(
            cell_label("c", 2, r.cells[2], false),
            "c, square 3, on, volume 64, pitch -5 semitones, repeats 4"
        );
    }

    #[test]
    fn cursor_moves_and_clamps() {
        assert_eq!(move_cursor((0, 0), -1, -1, 3, 16), (0, 0));
        assert_eq!(move_cursor((2, 15), 1, 1, 3, 16), (2, 15));
        assert_eq!(move_cursor((1, 5), 1, -2, 3, 16), (2, 3));
        assert_eq!(move_cursor((0, 0), 1, 1, 0, 16), (0, 0));
    }

    #[test]
    fn accessible_label_text() {
        assert_eq!(
            cell_label("Kick", 4, Cell::on(100), false),
            "Kick, square 5, on, volume 100"
        );
        assert_eq!(
            cell_label("Kick", 0, Cell::Off, true),
            "Kick, square 1, off, read only, notes from Piano"
        );
    }
}
