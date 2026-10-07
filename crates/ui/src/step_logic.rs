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
    /// Extra space after every group of `group` steps.
    pub group_gap: f64,
    pub group: u32,
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
        }
    }
}

impl StepLayout {
    /// The layout of the grid next to a separate channel header column
    /// (docs/ui-design.md 3.3): no name area, cells 28 x 32 (36 x 44 for
    /// touch), 24 px ruler, rows `row_h` high.
    pub fn cells_only(row_h: f64, touch: bool) -> StepLayout {
        StepLayout {
            name_w: 0.0,
            left_pad: 4.0,
            header_h: RULER_H,
            row_h,
            cell_w: if touch { 36.0 } else { 28.0 },
            cell_gap: 2.0,
            group_gap: 8.0,
            group: 4,
        }
    }

    /// Height of a cell inside a row.
    pub fn cell_h(&self) -> f64 {
        (self.row_h - 8.0).max(8.0)
    }
}

/// Height of the step ruler (and of the spacer above the header column).
pub const RULER_H: f64 = 24.0;

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
        self.name_w
            + self.left_pad
            + step as f64 * (self.cell_w + self.cell_gap)
            + groups * self.group_gap
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
    On { vel: u8 },
}

/// A row as the grid shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RowView {
    pub cells: Vec<Cell>,
    /// Some note of this channel in this pattern is not a step note (5.2):
    /// the row is read-only and shows the "piano roll data" marker.
    pub piano_roll_data: bool,
}

/// Builds the row of `channel` in `pattern` from the document's notes.
pub fn row_view(pattern: &Pattern, channel: &Channel) -> RowView {
    let mut cells = vec![Cell::Off; pattern.length_steps as usize];
    let mut foreign = false;
    for n in pattern.notes_of(channel.id) {
        if n.is_step_note(channel.root_key, pattern) {
            let i = (n.start / pattern.step_ticks) as usize;
            if let Some(c) = cells.get_mut(i) {
                *c = match *c {
                    Cell::On { vel } => Cell::On {
                        vel: vel.max(n.vel),
                    },
                    Cell::Off => Cell::On { vel: n.vel },
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
        Cell::On { vel } => format!("on, velocity {vel}"),
    };
    let ro = if read_only {
        ", read only, piano roll data"
    } else {
        ""
    };
    format!("{channel_name}, step {}, {state}{ro}", step + 1)
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
    pattern.notes_of(channel.id).to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::ids::{ChannelId, NoteId, PatternId, TrackId};
    use protocol::model::{ChannelNotes, Instrument, Mix, SynthParams};

    fn chan(root: u8) -> Channel {
        Channel {
            id: ChannelId(1),
            name: "Kick".into(),
            root_key: root,
            track: TrackId::MASTER,
            mix: Mix::default(),
            instrument: Instrument::Synth(SynthParams::default()),
        }
    }

    fn pat(notes: Vec<Note>) -> Pattern {
        let mut p = Pattern::new(PatternId(2), "A".into());
        p.notes = vec![ChannelNotes {
            channel: ChannelId(1),
            notes,
        }];
        p
    }

    fn n(id: u32, start: u32, len: u32, key: u8, vel: u8) -> Note {
        Note {
            id: NoteId(id),
            start,
            len,
            key,
            vel,
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
        assert_eq!(r.cells[0], Cell::On { vel: 90 });
        assert_eq!(r.cells[1], Cell::Off);
        assert_eq!(r.cells[2], Cell::On { vel: 120 });
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
        use crate::document::{Document, apply};
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
                    channel: ch,
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
    fn cursor_moves_and_clamps() {
        assert_eq!(move_cursor((0, 0), -1, -1, 3, 16), (0, 0));
        assert_eq!(move_cursor((2, 15), 1, 1, 3, 16), (2, 15));
        assert_eq!(move_cursor((1, 5), 1, -2, 3, 16), (2, 3));
        assert_eq!(move_cursor((0, 0), 1, 1, 0, 16), (0, 0));
    }

    #[test]
    fn accessible_label_text() {
        assert_eq!(
            cell_label("Kick", 4, Cell::On { vel: 100 }, false),
            "Kick, step 5, on, velocity 100"
        );
        assert_eq!(
            cell_label("Kick", 0, Cell::Off, true),
            "Kick, step 1, off, read only, piano roll data"
        );
    }
}
