// SPDX-License-Identifier: GPL-3.0-or-later
//! Geometry and editing rules of the Timeline (SPEC 20), without GTK:
//! where rows, clips and the ruler are, what a point hits, how new clips
//! are placed, and whether a move or resize fits without overlapping
//! another clip on the same row (the document refuses overlaps; the
//! widget only offers what fits).

use protocol::consts::{MAX_TICK, PPQ};
use protocol::ids::{ChannelId, ClipId, ShapeId};
use protocol::model::Clip;

/// Ruler height: bar numbers on top, the loop strip under them, and the
/// slim Patterns lane at the bottom (SPEC 20.7).
pub const RULER_H: f64 = 54.0;
/// Top of the loop strip.
pub const LOOP_Y: f64 = 22.0;
/// The loop strip.
pub const LOOP_H: f64 = 10.0;
/// Top and height of the Patterns lane.
pub const PATTERN_Y: f64 = LOOP_Y + LOOP_H;
pub const PATTERN_H: f64 = RULER_H - PATTERN_Y;
/// Pointer zone of a fade handle at the top corners of an audio clip.
pub const HANDLE_R: f64 = 7.0;
/// Row height (touch: `ROW_H_TOUCH`).
pub const ROW_H: f64 = 48.0;
pub const ROW_H_TOUCH: f64 = 56.0;
/// Pointer zone at a clip's ends that resizes instead of moving.
pub const EDGE_PX: f64 = 8.0;
/// The smallest unit the grid snaps to: one 16th note.
pub const STEP_TICKS: u32 = PPQ / 4;
/// Zoom limits, pixels per tick.
pub const MIN_PX_PER_TICK: f64 = 0.004;
pub const MAX_PX_PER_TICK: f64 = 0.5;

/// What part of the timeline is visible and how big things are.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct View {
    pub px_per_tick: f64,
    /// Horizontal scroll in pixels.
    pub scroll_x: f64,
    /// Vertical scroll in pixels.
    pub scroll_y: f64,
    pub row_h: f64,
    pub width: f64,
    pub height: f64,
}

impl Default for View {
    fn default() -> View {
        // About 48 px per beat: a 4-bar loop fills 768 px.
        View {
            px_per_tick: 0.05,
            scroll_x: 0.0,
            scroll_y: 0.0,
            row_h: ROW_H,
            width: 800.0,
            height: 400.0,
        }
    }
}

impl View {
    pub fn tick_to_x(&self, tick: f64) -> f64 {
        tick * self.px_per_tick - self.scroll_x
    }

    pub fn x_to_tick(&self, x: f64) -> f64 {
        ((x + self.scroll_x) / self.px_per_tick).max(0.0)
    }

    /// Top of row `i`.
    pub fn row_y(&self, i: usize) -> f64 {
        RULER_H + i as f64 * self.row_h - self.scroll_y
    }

    /// The row under `y`, if any (rows below the last one are `None`).
    pub fn row_at(&self, y: f64, rows: usize) -> Option<usize> {
        if y < RULER_H {
            return None;
        }
        let i = ((y - RULER_H + self.scroll_y) / self.row_h).floor();
        (i >= 0.0 && (i as usize) < rows).then_some(i as usize)
    }

    /// Zooms time by `factor`, keeping the tick under `anchor_x` in place.
    pub fn zoom_x(&mut self, factor: f64, anchor_x: f64) {
        let t = self.x_to_tick(anchor_x);
        self.px_per_tick = (self.px_per_tick * factor).clamp(MIN_PX_PER_TICK, MAX_PX_PER_TICK);
        self.scroll_x = (t * self.px_per_tick - anchor_x).max(0.0);
    }

    /// Scroll range: content until `end_tick` plus one screen, and all rows.
    pub fn clamp_scroll(&mut self, end_tick: u32, rows: usize) {
        let content_w = end_tick as f64 * self.px_per_tick + self.width * 0.5;
        self.scroll_x = self.scroll_x.clamp(0.0, (content_w - self.width).max(0.0));
        let content_h = RULER_H + rows as f64 * self.row_h + self.row_h;
        self.scroll_y = self.scroll_y.clamp(0.0, (content_h - self.height).max(0.0));
    }

    /// Scrolls so `tick` is visible.
    pub fn reveal_tick(&mut self, tick: f64) {
        let x = self.tick_to_x(tick);
        let margin = 24.0;
        if x < margin {
            self.scroll_x = (self.scroll_x + x - margin).max(0.0);
        } else if x > self.width - margin {
            self.scroll_x += x - (self.width - margin);
        }
    }
}

/// What a displayed row is: an instrument's row of clips, or a lane that
/// draws one shape under its instrument (SPEC 24.2-1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Row {
    Instrument(ChannelId),
    Shape(ShapeId),
}

/// The display row of `channel`.
pub fn row_of(rows: &[Row], channel: ChannelId) -> Option<usize> {
    rows.iter().position(|r| *r == Row::Instrument(channel))
}

/// Where on a clip the pointer is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    Body,
    Start,
    End,
    /// The handle of an audio clip's fade in (top left).
    FadeIn,
    /// The handle of an audio clip's fade out (top right).
    FadeOut,
}

/// What a point hits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    /// The bar numbers: the playhead goes there.
    Ruler { tick: u32 },
    /// The loop strip under the bar numbers.
    Loop { tick: u32 },
    /// The Patterns lane under the loop strip.
    Pattern { tick: u32 },
    /// A shape lane under its instrument.
    Shape {
        row: usize,
        shape: ShapeId,
        tick: u32,
    },
    Clip {
        clip: ClipId,
        row: usize,
        part: Part,
    },
    /// An empty spot on a row.
    Lane { row: usize, tick: u32 },
    /// Below the last row.
    Below { tick: u32 },
}

/// The clip on `instrument`'s row that covers `tick`, if any.
pub fn clip_at(clips: &[Clip], instrument: ChannelId, tick: u32) -> Option<&Clip> {
    clips
        .iter()
        .find(|c| c.instrument == instrument && c.start <= tick && tick < c.end())
}

/// Where the fade handle of an audio clip sits: on the top edge, `fade`
/// ticks in from the near end.
pub fn handle_x(view: &View, c: &Clip, fade_in: bool) -> f64 {
    let (fi, fo) = c.audio.map_or((0, 0), |a| (a.fade_in, a.fade_out));
    if fade_in {
        view.tick_to_x(c.start as f64 + fi as f64) + HANDLE_R
    } else {
        view.tick_to_x(c.end() as f64 - fo as f64) - HANDLE_R
    }
}

pub fn hit(view: &View, rows: &[Row], clips: &[Clip], x: f64, y: f64) -> Hit {
    let tick = view.x_to_tick(x).round().min(MAX_TICK as f64) as u32;
    if y < LOOP_Y {
        return Hit::Ruler { tick };
    }
    if y < PATTERN_Y {
        return Hit::Loop { tick };
    }
    if y < RULER_H {
        return Hit::Pattern { tick };
    }
    let Some(row) = view.row_at(y, rows.len()) else {
        return Hit::Below { tick };
    };
    let inst = match rows[row] {
        Row::Instrument(c) => c,
        Row::Shape(shape) => return Hit::Shape { row, shape, tick },
    };
    // A thin clip is all body: its ends would leave nothing to grab.
    for c in clips.iter().filter(|c| c.instrument == inst) {
        let x0 = view.tick_to_x(c.start as f64);
        let x1 = view.tick_to_x(c.end() as f64);
        if x < x0 || x >= x1 {
            continue;
        }
        let wide = x1 - x0 > 3.0 * EDGE_PX;
        let edge = if wide { EDGE_PX } else { 0.0 };
        let top = view.row_y(row) + 3.0;
        let part = if c.audio.is_some() && wide && y < top + 2.0 * HANDLE_R + 2.0 {
            // The fade handles sit on the top edge and win over the ends.
            let near = |fade_in: bool| (x - handle_x(view, c, fade_in)).abs() <= HANDLE_R;
            if near(true) {
                Part::FadeIn
            } else if near(false) {
                Part::FadeOut
            } else {
                edge_part(x, x0, x1, edge)
            }
        } else {
            edge_part(x, x0, x1, edge)
        };
        return Hit::Clip {
            clip: c.id,
            row,
            part,
        };
    }
    Hit::Lane { row, tick }
}

fn edge_part(x: f64, x0: f64, x1: f64, edge: f64) -> Part {
    if x < x0 + edge {
        Part::Start
    } else if x >= x1 - edge {
        Part::End
    } else {
        Part::Body
    }
}

pub fn snap_floor(tick: u32, unit: u32) -> u32 {
    if unit == 0 { tick } else { tick / unit * unit }
}

pub fn snap_round(tick: i64, unit: u32) -> i64 {
    if unit == 0 {
        return tick;
    }
    let u = unit as i64;
    (tick + u.signum() * u / 2).div_euclid(u) * u
}

/// The grid unit for editing at this zoom: the smallest of step, beat
/// and bar that is at least 12 px wide, so snapping is as fine as one
/// can see.
pub fn snap_unit(px_per_tick: f64, bar_ticks: u32) -> u32 {
    for u in [STEP_TICKS, PPQ, bar_ticks] {
        if u as f64 * px_per_tick >= 12.0 {
            return u;
        }
    }
    bar_ticks
}

/// Lines to draw: (tick, level) with level 2 at bars, 1 at beats, 0 at
/// steps, only where they are at least `min_px` apart.
pub fn grid_lines(view: &View, bar_ticks: u32, min_px: f64) -> Vec<(u32, u8)> {
    let first = view.x_to_tick(0.0) as u32;
    let last = view.x_to_tick(view.width).ceil() as u32;
    let mut out = Vec::new();
    for (unit, level) in [(STEP_TICKS, 0u8), (PPQ, 1), (bar_ticks, 2)] {
        if (unit as f64) * view.px_per_tick < min_px {
            continue;
        }
        let mut t = snap_floor(first, unit);
        while t <= last {
            // Coarser lines replace finer ones at the same tick.
            out.retain(|(x, _)| *x != t);
            out.push((t, level));
            t += unit;
        }
    }
    out.sort_unstable();
    out
}

/// Every `n`th bar gets a number, so labels never touch.
pub fn bar_label_every(view: &View, bar_ticks: u32, label_px: f64) -> u32 {
    let bar_px = bar_ticks as f64 * view.px_per_tick;
    let mut n = 1;
    while (n as f64) * bar_px < label_px && n < 1024 {
        n *= 2;
    }
    n
}

/// Where a click on an empty spot of a row puts a new clip: at the start
/// of the bar under the pointer, one bar long, shortened to end where the
/// next clip starts. `None` when there is no room for at least one step.
pub fn place_new_clip(
    clips: &[Clip],
    instrument: ChannelId,
    tick: u32,
    bar_ticks: u32,
) -> Option<(u32, u32)> {
    let start = snap_floor(tick, bar_ticks);
    if clip_at(clips, instrument, start).is_some() {
        // The bar starts inside a clip: start after it, at the step.
        let after = clips
            .iter()
            .filter(|c| c.instrument == instrument && c.start <= start && start < c.end())
            .map(|c| c.end())
            .max()?;
        return fit_from(
            clips,
            instrument,
            after.max(snap_floor(tick, STEP_TICKS)),
            bar_ticks,
        );
    }
    fit_from(clips, instrument, start, bar_ticks)
}

fn fit_from(clips: &[Clip], instrument: ChannelId, start: u32, len: u32) -> Option<(u32, u32)> {
    if clip_at(clips, instrument, start).is_some() {
        return None;
    }
    let next = clips
        .iter()
        .filter(|c| c.instrument == instrument && c.start > start)
        .map(|c| c.start)
        .min();
    let len = match next {
        Some(n) => len.min(n - start),
        None => len,
    };
    (len >= STEP_TICKS).then_some((start, len))
}

/// Whether moving `moving` by `dt` keeps every clip at or after tick 0
/// and no two clips on a row overlapping.
pub fn fits_move(clips: &[Clip], moving: &[ClipId], dt: i64) -> bool {
    let moved: Vec<Clip> = clips
        .iter()
        .map(|c| {
            if moving.contains(&c.id) {
                let s = c.start as i64 + dt;
                Clip {
                    start: s.clamp(-1, MAX_TICK as i64) as u32,
                    ..*c
                }
            } else {
                *c
            }
        })
        .collect();
    if clips
        .iter()
        .any(|c| moving.contains(&c.id) && (c.start as i64 + dt) < 0)
    {
        return false;
    }
    no_overlap(&moved)
}

/// Whether resizing `ids` by `dlen` (at the end, or at the start when
/// `from_start`) keeps them at least one step long, at or after 0, and
/// not overlapping.
pub fn fits_resize(clips: &[Clip], ids: &[ClipId], dlen: i64, from_start: bool) -> bool {
    let mut out = Vec::with_capacity(clips.len());
    for c in clips {
        if !ids.contains(&c.id) {
            out.push(*c);
            continue;
        }
        let len = c.len as i64 + dlen;
        if len < STEP_TICKS as i64 {
            return false;
        }
        let start = if from_start {
            c.start as i64 - dlen
        } else {
            c.start as i64
        };
        if start < 0 {
            return false;
        }
        out.push(Clip {
            start: start as u32,
            len: len as u32,
            ..*c
        });
    }
    no_overlap(&out)
}

/// Whether a linked copy of `ids` fits right after the selection (Ctrl+D):
/// returns the offset to use.
pub fn duplicate_offset(clips: &[Clip], ids: &[ClipId]) -> Option<i64> {
    let chosen: Vec<&Clip> = clips.iter().filter(|c| ids.contains(&c.id)).collect();
    let first = chosen.iter().map(|c| c.start).min()?;
    let last = chosen.iter().map(|c| c.end()).max()?;
    let dt = (last - first) as i64;
    let mut all: Vec<Clip> = clips.to_vec();
    for c in &chosen {
        all.push(Clip {
            id: ClipId(u32::MAX - c.id.0),
            start: (c.start as i64 + dt) as u32,
            ..**c
        });
    }
    no_overlap(&all).then_some(dt)
}

fn no_overlap(clips: &[Clip]) -> bool {
    let mut v: Vec<&Clip> = clips.iter().collect();
    v.sort_by_key(|c| (c.instrument, c.start));
    v.windows(2)
        .all(|w| w[0].instrument != w[1].instrument || w[0].end() <= w[1].start)
}

/// The last tick anything uses: the end of the last clip or the loop.
pub fn end_tick(clips: &[Clip], loop_end: u32) -> u32 {
    clips
        .iter()
        .map(|c| c.end())
        .max()
        .unwrap_or(0)
        .max(loop_end)
}

/// Where the content's notes fall inside a clip, for the preview drawn on
/// it: for each repeat of a content `content_len` long, starting `offset`
/// ticks in, the clip-relative start of that repeat.
pub fn repeats(clip_len: u32, content_len: u32, offset: u32) -> Vec<i64> {
    if content_len == 0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut t = -((offset % content_len) as i64);
    while t < clip_len as i64 {
        out.push(t);
        t += content_len as i64;
    }
    out
}

/// "Bar 3" style accessible position of a tick (1-based bars and beats).
pub fn spoken(tick: u32, bar_ticks: u32) -> String {
    let bar = tick / bar_ticks + 1;
    let beat = (tick % bar_ticks) / PPQ + 1;
    format!("bar {bar}, beat {beat}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::ids::PatternId;

    const BAR: u32 = 4 * PPQ;

    fn clip(id: u32, inst: u32, start: u32, len: u32) -> Clip {
        Clip {
            id: ClipId(id),
            instrument: ChannelId(inst),
            pattern: PatternId(100 + inst),
            start,
            len,
            offset: 0,
            muted: false,
            audio: None,
            group: None,
        }
    }

    #[test]
    fn view_maps_ticks_and_rows() {
        let mut v = View::default();
        assert_eq!(v.tick_to_x(0.0), 0.0);
        assert_eq!(v.x_to_tick(v.tick_to_x(960.0)), 960.0);
        assert_eq!(v.row_at(RULER_H + 1.0, 3), Some(0));
        assert_eq!(v.row_at(RULER_H + ROW_H * 2.5, 3), Some(2));
        assert_eq!(v.row_at(RULER_H + ROW_H * 3.5, 3), None);
        assert_eq!(v.row_at(5.0, 3), None);
        // Zoom keeps the tick under the anchor.
        let before = v.x_to_tick(300.0);
        v.zoom_x(2.0, 300.0);
        assert!((v.x_to_tick(300.0) - before).abs() < 1e-6);
    }

    #[test]
    fn hits() {
        let v = View::default();
        let rows = [Row::Instrument(ChannelId(1)), Row::Instrument(ChannelId(2))];
        let clips = [clip(10, 1, 0, BAR)];
        let x_mid = v.tick_to_x(BAR as f64 / 2.0);
        let y0 = v.row_y(0) + 10.0;
        assert_eq!(
            hit(&v, &rows, &clips, x_mid, y0),
            Hit::Clip {
                clip: ClipId(10),
                row: 0,
                part: Part::Body
            }
        );
        let x_end = v.tick_to_x(BAR as f64) - 2.0;
        assert!(matches!(
            hit(&v, &rows, &clips, x_end, y0),
            Hit::Clip {
                part: Part::End,
                ..
            }
        ));
        assert!(matches!(
            hit(&v, &rows, &clips, 1.0, y0),
            Hit::Clip {
                part: Part::Start,
                ..
            }
        ));
        assert!(matches!(
            hit(&v, &rows, &clips, x_mid, v.row_y(1) + 5.0),
            Hit::Lane { row: 1, .. }
        ));
        assert!(matches!(
            hit(&v, &rows, &clips, x_mid, 2.0),
            Hit::Ruler { .. }
        ));
        assert!(matches!(
            hit(&v, &rows, &clips, x_mid, LOOP_Y + 3.0),
            Hit::Loop { .. }
        ));
        assert!(matches!(
            hit(&v, &rows, &clips, x_mid, RULER_H - 2.0),
            Hit::Pattern { .. }
        ));
        assert!(matches!(
            hit(&v, &rows, &clips, x_mid, v.row_y(2) + 5.0),
            Hit::Below { .. }
        ));
    }

    #[test]
    fn new_clips_start_on_the_bar_and_never_overlap() {
        let i = ChannelId(1);
        let clips = [clip(10, 1, BAR, BAR), clip(11, 1, 3 * BAR + BAR / 2, BAR)];
        // Empty bar 1: a one-bar clip at its start.
        assert_eq!(place_new_clip(&clips, i, 100, BAR), Some((0, BAR)));
        // Bar 3 is free until the next clip half a bar in: shortened.
        assert_eq!(
            place_new_clip(&clips, i, 3 * BAR + 10, BAR),
            Some((3 * BAR, BAR / 2))
        );
        // Inside a clip's bar: after that clip.
        assert_eq!(
            place_new_clip(&clips, i, BAR + 10, BAR),
            Some((2 * BAR, BAR))
        );
        // Another row is unaffected.
        assert_eq!(
            place_new_clip(&clips, ChannelId(2), BAR, BAR),
            Some((BAR, BAR))
        );
    }

    #[test]
    fn moves_and_resizes_that_fit() {
        let clips = [
            clip(1, 1, 0, BAR),
            clip(2, 1, 2 * BAR, BAR),
            clip(3, 2, 0, BAR),
        ];
        assert!(fits_move(&clips, &[ClipId(1)], BAR as i64));
        assert!(
            !fits_move(&clips, &[ClipId(1)], 2 * BAR as i64),
            "onto clip 2"
        );
        assert!(!fits_move(&clips, &[ClipId(1)], -1), "before 0");
        assert!(fits_move(&clips, &[ClipId(1), ClipId(2)], 4 * BAR as i64));
        assert!(fits_move(&clips, &[ClipId(3)], 2 * BAR as i64), "other row");
        assert!(fits_resize(&clips, &[ClipId(1)], BAR as i64, false));
        assert!(!fits_resize(&clips, &[ClipId(1)], BAR as i64 + 1, false));
        assert!(
            !fits_resize(&clips, &[ClipId(1)], -(BAR as i64), false),
            "too short"
        );
        assert!(!fits_resize(&clips, &[ClipId(2)], 1, true) || clips[1].start > 0);
        assert!(fits_resize(&clips, &[ClipId(2)], BAR as i64, true));
        assert!(!fits_resize(&clips, &[ClipId(2)], BAR as i64 + 1, true));
    }

    #[test]
    fn duplicates_go_right_after_the_selection() {
        let clips = [clip(1, 1, 0, BAR), clip(2, 2, 0, 2 * BAR)];
        assert_eq!(
            duplicate_offset(&clips, &[ClipId(1), ClipId(2)]),
            Some(2 * BAR as i64)
        );
        let blocked = [clip(1, 1, 0, BAR), clip(2, 1, BAR, BAR)];
        assert_eq!(duplicate_offset(&blocked, &[ClipId(1)]), None);
        assert_eq!(duplicate_offset(&blocked, &[ClipId(2)]), Some(BAR as i64));
    }

    #[test]
    fn snapping_follows_the_zoom() {
        assert_eq!(snap_unit(0.05, BAR), STEP_TICKS);
        assert_eq!(snap_unit(0.02, BAR), PPQ);
        assert_eq!(snap_unit(0.004, BAR), BAR);
        assert_eq!(snap_round(130, 240), 240);
        assert_eq!(snap_round(100, 240), 0);
        assert_eq!(snap_round(-130, 240), -240);
        assert_eq!(snap_floor(1000, 960), 960);
    }

    #[test]
    fn grid_and_labels() {
        let v = View {
            width: 400.0,
            ..View::default()
        };
        let lines = grid_lines(&v, BAR, 8.0);
        assert_eq!(lines[0], (0, 2), "a bar line at 0");
        assert!(lines.contains(&(PPQ, 1)));
        assert!(lines.contains(&(STEP_TICKS, 0)));
        assert_eq!(bar_label_every(&v, BAR, 30.0), 1);
        let far = View {
            px_per_tick: 0.004,
            ..v
        };
        assert!(bar_label_every(&far, BAR, 30.0) >= 2);
    }

    #[test]
    fn looping_clips_repeat_their_content() {
        assert_eq!(repeats(4 * 960, 960, 0), vec![0, 960, 1920, 2880]);
        assert_eq!(repeats(1000, 960, 480), vec![-480, 480]);
        assert!(repeats(1000, 0, 0).is_empty());
        assert_eq!(spoken(BAR + PPQ, BAR), "bar 2, beat 2");
    }
}
