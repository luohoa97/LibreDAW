// SPDX-License-Identifier: GPL-3.0-or-later
//! Geometry shared by the piano roll and the step grid (SPEC 11): the map
//! between musical positions (ticks, keys) and pixels, scrolling, zooming,
//! and snapping. Plain functions on plain structs, so the widget wrappers
//! stay thin and this is testable without GTK.

use protocol::consts::PPQ;

/// Pixels per tick limits: one beat is `PPQ` ticks.
pub const MIN_PX_PER_TICK: f64 = 0.01;
pub const MAX_PX_PER_TICK: f64 = 1.2;
pub const MIN_ROW_H: f64 = 6.0;
pub const MAX_ROW_H: f64 = 40.0;

/// Where things are inside the roll widget. Pixel positions are widget
/// coordinates; scroll offsets are in content pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Viewport {
    pub px_per_tick: f64,
    pub row_h: f64,
    pub scroll_x: f64,
    pub scroll_y: f64,
    /// Width of the keyboard column.
    pub key_w: f64,
    /// Height of the bar ruler.
    pub ruler_h: f64,
    /// Height of the velocity lane under the grid.
    pub vel_h: f64,
    /// Widget size.
    pub width: f64,
    pub height: f64,
}

impl Default for Viewport {
    fn default() -> Viewport {
        Viewport {
            px_per_tick: 0.12,
            row_h: 16.0,
            scroll_x: 0.0,
            // Start around C4 (key 60).
            scroll_y: (127.0 - 72.0) * 16.0,
            key_w: 56.0,
            ruler_h: 22.0,
            vel_h: 64.0,
            width: 800.0,
            height: 500.0,
        }
    }
}

impl Viewport {
    /// Top of the note grid.
    pub fn grid_top(&self) -> f64 {
        self.ruler_h
    }

    /// Bottom of the note grid (top of the velocity lane).
    pub fn grid_bottom(&self) -> f64 {
        (self.height - self.vel_h).max(self.ruler_h + 1.0)
    }

    pub fn grid_height(&self) -> f64 {
        self.grid_bottom() - self.grid_top()
    }

    pub fn grid_width(&self) -> f64 {
        (self.width - self.key_w).max(1.0)
    }

    pub fn vel_top(&self) -> f64 {
        self.grid_bottom()
    }

    pub fn in_grid(&self, x: f64, y: f64) -> bool {
        x >= self.key_w && y >= self.grid_top() && y < self.grid_bottom()
    }

    pub fn in_vel_lane(&self, x: f64, y: f64) -> bool {
        x >= self.key_w && y >= self.vel_top()
    }

    pub fn tick_to_x(&self, tick: f64) -> f64 {
        self.key_w + tick * self.px_per_tick - self.scroll_x
    }

    pub fn x_to_tick(&self, x: f64) -> f64 {
        (x - self.key_w + self.scroll_x) / self.px_per_tick
    }

    /// Top edge of a key's row. Key 127 is at the top.
    pub fn key_to_y(&self, key: i32) -> f64 {
        self.grid_top() + (127 - key) as f64 * self.row_h - self.scroll_y
    }

    /// The key whose row contains `y` (may be outside 0..=127).
    pub fn y_to_key(&self, y: f64) -> i32 {
        127 - ((y - self.grid_top() + self.scroll_y) / self.row_h).floor() as i32
    }

    pub fn content_width(&self, len_ticks: u32) -> f64 {
        len_ticks as f64 * self.px_per_tick
    }

    pub fn content_height(&self) -> f64 {
        128.0 * self.row_h
    }

    /// Visible tick range `[start, end)`.
    pub fn visible_ticks(&self) -> (f64, f64) {
        (self.x_to_tick(self.key_w), self.x_to_tick(self.width))
    }

    /// Visible key range, `(lowest, highest)`, clamped to 0..=127.
    pub fn visible_keys(&self) -> (i32, i32) {
        let hi = self.y_to_key(self.grid_top()).clamp(0, 127);
        let lo = self.y_to_key(self.grid_bottom() - 0.001).clamp(0, 127);
        (lo, hi)
    }

    /// Keeps scrolling inside the content. `len_ticks` is the pattern length.
    pub fn clamp_scroll(&mut self, len_ticks: u32) {
        let max_x = (self.content_width(len_ticks) - self.grid_width()).max(0.0);
        let max_y = (self.content_height() - self.grid_height()).max(0.0);
        self.scroll_x = self.scroll_x.clamp(0.0, max_x);
        self.scroll_y = self.scroll_y.clamp(0.0, max_y);
    }

    /// Zooms horizontally by `factor`, keeping the tick under `anchor_x`
    /// where it is.
    pub fn zoom_x(&mut self, factor: f64, anchor_x: f64, len_ticks: u32) {
        let t = self.x_to_tick(anchor_x);
        self.px_per_tick = (self.px_per_tick * factor).clamp(MIN_PX_PER_TICK, MAX_PX_PER_TICK);
        self.scroll_x += self.tick_to_x(t) - anchor_x;
        self.clamp_scroll(len_ticks);
    }

    /// Zooms vertically (row height), keeping the key under `anchor_y`.
    pub fn zoom_y(&mut self, factor: f64, anchor_y: f64, len_ticks: u32) {
        let k = (anchor_y - self.grid_top() + self.scroll_y) / self.row_h;
        self.row_h = (self.row_h * factor).clamp(MIN_ROW_H, MAX_ROW_H);
        self.scroll_y = k * self.row_h - (anchor_y - self.grid_top());
        self.clamp_scroll(len_ticks);
    }

    /// Scrolls so `key` is inside the grid, if it is not.
    pub fn reveal_key(&mut self, key: i32, len_ticks: u32) {
        let top = self.key_to_y(key);
        if top < self.grid_top() {
            self.scroll_y -= self.grid_top() - top;
        } else if top + self.row_h > self.grid_bottom() {
            self.scroll_y += top + self.row_h - self.grid_bottom();
        }
        self.clamp_scroll(len_ticks);
    }

    /// Scrolls so the notes of a channel are in view: the key span
    /// `lo..=hi` is centered (or its top key is at the top when the span is
    /// taller than the grid), and time starts at 0 unless the first note is
    /// further right than the grid is wide.
    pub fn scroll_to_notes(&mut self, first_tick: u32, lo: i32, hi: i32, len_ticks: u32) {
        let (lo, hi) = (lo.clamp(0, 127), hi.clamp(0, 127));
        let (lo, hi) = (lo.min(hi), lo.max(hi));
        let span_h = (hi - lo + 1) as f64 * self.row_h;
        let top = (127 - hi) as f64 * self.row_h;
        self.scroll_y = if span_h >= self.grid_height() {
            top - self.row_h
        } else {
            top - (self.grid_height() - span_h) / 2.0
        };
        let x = first_tick as f64 * self.px_per_tick;
        self.scroll_x = if x < self.grid_width() * 0.8 {
            0.0
        } else {
            x - 16.0
        };
        self.clamp_scroll(len_ticks);
    }

    /// Scrolls so `tick` is inside the grid, if it is not.
    pub fn reveal_tick(&mut self, tick: f64, len_ticks: u32) {
        let x = self.tick_to_x(tick);
        if x < self.key_w {
            self.scroll_x -= self.key_w - x;
        } else if x > self.width - 20.0 {
            self.scroll_x += x - (self.width - 20.0);
        }
        self.clamp_scroll(len_ticks);
    }
}

/// Snaps a tick position down to a multiple of `unit` (at least 1).
pub fn snap_floor(tick: f64, unit: u32) -> u32 {
    let unit = unit.max(1) as f64;
    ((tick / unit).floor().max(0.0) * unit) as u32
}

/// Snaps to the nearest multiple of `unit`.
pub fn snap_round(tick: f64, unit: u32) -> i64 {
    let unit = unit.max(1) as f64;
    ((tick / unit).round() * unit) as i64
}

/// Snap choices of the roll: label and ticks. `None` ticks means the
/// pattern's step length.
pub const SNAPS: [(&str, Option<u32>); 7] = [
    ("Step", None),
    ("1/4", Some(PPQ)),
    ("1/8", Some(PPQ / 2)),
    ("1/16", Some(PPQ / 4)),
    ("1/32", Some(PPQ / 8)),
    ("1/3 beat", Some(PPQ / 3)),
    ("Off", Some(1)),
];

const NAMES: [&str; 12] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];

/// Scientific pitch names with middle C (key 60) as C4.
pub fn note_name(key: u8) -> String {
    format!("{}{}", NAMES[(key % 12) as usize], key as i32 / 12 - 1)
}

pub fn is_black_key(key: u8) -> bool {
    matches!(key % 12, 1 | 3 | 6 | 8 | 10)
}

/// Lines of the time grid inside `[from, to)` ticks: `(tick, level)` where
/// level 2 is a bar line, 1 a beat line, 0 a subdivision. `sub` is the
/// subdivision in ticks (the snap unit); lines closer than `min_px` pixels
/// are left out so a zoomed-out roll stays readable.
pub fn grid_lines(
    from: f64,
    to: f64,
    px_per_tick: f64,
    bar_ticks: u32,
    sub: u32,
    min_px: f64,
) -> Vec<(u32, u8)> {
    let mut out = Vec::new();
    let beat = PPQ;
    let show_sub = sub > 1 && sub < beat && sub as f64 * px_per_tick >= min_px;
    let show_beat = beat as f64 * px_per_tick >= min_px;
    let step = if show_sub {
        sub
    } else if show_beat {
        beat
    } else {
        bar_ticks.max(1)
    };
    let first = (from.max(0.0) / step as f64).floor() as u32;
    let mut i = first;
    loop {
        let t = i.saturating_mul(step);
        if t as f64 >= to {
            break;
        }
        if t as f64 >= from - 1.0 {
            let level = if bar_ticks > 0 && t % bar_ticks == 0 {
                2
            } else if t % beat == 0 {
                1
            } else {
                0
            };
            out.push((t, level));
        }
        if i == u32::MAX {
            break;
        }
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vp() -> Viewport {
        Viewport {
            px_per_tick: 0.1,
            row_h: 10.0,
            scroll_x: 0.0,
            scroll_y: 0.0,
            key_w: 50.0,
            ruler_h: 20.0,
            vel_h: 60.0,
            width: 450.0,
            height: 380.0,
        }
    }

    #[test]
    fn tick_and_x_are_inverse() {
        let mut v = vp();
        v.scroll_x = 33.0;
        for t in [0.0, 1.0, 960.0, 12345.5] {
            assert!((v.x_to_tick(v.tick_to_x(t)) - t).abs() < 1e-9);
        }
        // Tick 0 sits at the left edge of the grid when not scrolled.
        assert_eq!(vp().tick_to_x(0.0), 50.0);
    }

    #[test]
    fn key_and_y_are_inverse_and_127_is_on_top() {
        let mut v = vp();
        v.scroll_y = 17.0;
        for k in [0, 1, 60, 127] {
            let y = v.key_to_y(k) + v.row_h / 2.0;
            assert_eq!(v.y_to_key(y), k);
        }
        let v = vp();
        assert_eq!(v.key_to_y(127), 20.0);
        assert_eq!(v.y_to_key(20.0), 127);
        assert_eq!(v.y_to_key(29.9), 127);
        assert_eq!(v.y_to_key(30.0), 126);
    }

    #[test]
    fn regions() {
        let v = vp();
        assert_eq!(v.grid_top(), 20.0);
        assert_eq!(v.grid_bottom(), 320.0);
        assert!(v.in_grid(60.0, 100.0));
        assert!(!v.in_grid(10.0, 100.0), "keyboard column");
        assert!(!v.in_grid(60.0, 10.0), "ruler");
        assert!(!v.in_grid(60.0, 330.0), "velocity lane");
        assert!(v.in_vel_lane(60.0, 330.0));
    }

    #[test]
    fn visible_ranges() {
        let mut v = vp();
        v.scroll_y = 127.0 * 10.0 - 100.0; // show the top 10 keys... of the lane
        let (lo, hi) = v.visible_keys();
        assert!(hi <= 127 && lo >= 0 && lo < hi);
        let (a, b) = v.visible_ticks();
        assert_eq!(a, 0.0);
        assert!((b - 4000.0).abs() < 1e-9);
    }

    #[test]
    fn scroll_is_clamped_to_content() {
        let mut v = vp();
        v.scroll_x = -50.0;
        v.scroll_y = -50.0;
        v.clamp_scroll(3840);
        assert_eq!((v.scroll_x, v.scroll_y), (0.0, 0.0));
        v.scroll_x = 1e9;
        v.scroll_y = 1e9;
        v.clamp_scroll(3840);
        assert_eq!(v.scroll_x, (3840.0 * 0.1 - v.grid_width()).max(0.0));
        assert_eq!(v.scroll_y, 128.0 * 10.0 - v.grid_height());
    }

    #[test]
    fn zoom_keeps_the_anchor_tick_in_place() {
        let mut v = vp();
        v.scroll_x = 100.0;
        let anchor = 200.0;
        let t = v.x_to_tick(anchor);
        v.zoom_x(2.0, anchor, 100_000);
        assert!((v.x_to_tick(anchor) - t).abs() < 1e-6);
        assert!((v.px_per_tick - 0.2).abs() < 1e-12);
        // Limits.
        for _ in 0..40 {
            v.zoom_x(2.0, anchor, 100_000);
        }
        assert_eq!(v.px_per_tick, MAX_PX_PER_TICK);
        for _ in 0..40 {
            v.zoom_x(0.5, anchor, 100_000);
        }
        assert_eq!(v.px_per_tick, MIN_PX_PER_TICK);
    }

    #[test]
    fn vertical_zoom_keeps_the_anchor_key() {
        let mut v = vp();
        v.scroll_y = 300.0;
        let anchor = 150.0;
        let key_pos = (anchor - v.grid_top() + v.scroll_y) / v.row_h;
        v.zoom_y(1.5, anchor, 3840);
        let after = (anchor - v.grid_top() + v.scroll_y) / v.row_h;
        assert!((key_pos - after).abs() < 1e-6);
        for _ in 0..40 {
            v.zoom_y(2.0, anchor, 3840);
        }
        assert_eq!(v.row_h, MAX_ROW_H);
        for _ in 0..40 {
            v.zoom_y(0.5, anchor, 3840);
        }
        assert_eq!(v.row_h, MIN_ROW_H);
    }

    #[test]
    fn scrolling_to_notes_centers_the_key_span() {
        let mut vp = Viewport::default();
        // A kick around C2 (keys 36..=38) is far below the default C4 view.
        assert!(vp.y_to_key(vp.grid_top()) > 60);
        vp.scroll_to_notes(0, 36, 38, 3840);
        let (lo, hi) = vp.visible_keys();
        assert!(lo <= 36 && hi >= 38, "{lo}..{hi}");
        // The span sits near the middle of the grid.
        let mid_y = vp.key_to_y(37) + vp.row_h / 2.0;
        let center = vp.grid_top() + vp.grid_height() / 2.0;
        assert!((mid_y - center).abs() <= vp.row_h * 2.0, "{mid_y} {center}");
        assert_eq!(vp.scroll_x, 0.0);
    }

    #[test]
    fn a_span_taller_than_the_grid_shows_its_top() {
        let mut vp = Viewport::default();
        vp.scroll_to_notes(0, 0, 127, 3840);
        let (_, hi) = vp.visible_keys();
        assert_eq!(hi, 127);
    }

    #[test]
    fn late_first_notes_scroll_sideways() {
        let mut vp = Viewport::default();
        vp.scroll_to_notes(20_000, 60, 60, 40_000);
        assert!(vp.scroll_x > 0.0);
        let x = vp.tick_to_x(20_000.0);
        assert!(x >= vp.key_w && x < vp.width, "{x}");
    }

    #[test]
    fn reveal_scrolls_just_enough() {
        let mut v = vp();
        v.scroll_y = 300.0;
        v.reveal_key(127, 3840);
        assert_eq!(v.key_to_y(127), v.grid_top());
        v.reveal_key(0, 3840);
        assert!((v.key_to_y(0) + v.row_h - v.grid_bottom()).abs() < 1e-9);
        let mut v = vp();
        v.reveal_tick(3000.0, 6000);
        assert!(v.tick_to_x(3000.0) <= v.width);
        v.reveal_tick(0.0, 6000);
        assert_eq!(v.scroll_x, 0.0);
    }

    #[test]
    fn snapping() {
        assert_eq!(snap_floor(250.0, 240), 240);
        assert_eq!(snap_floor(239.9, 240), 0);
        assert_eq!(snap_floor(-5.0, 240), 0);
        assert_eq!(snap_floor(100.0, 0), 100, "unit 0 acts as 1");
        assert_eq!(snap_round(359.0, 240), 240);
        assert_eq!(snap_round(361.0, 240), 480);
        assert_eq!(snap_round(-130.0, 240), -240);
    }

    #[test]
    fn note_names_use_c4_for_middle_c() {
        assert_eq!(note_name(60), "C4");
        assert_eq!(note_name(61), "C#4");
        assert_eq!(note_name(0), "C-1");
        assert_eq!(note_name(127), "G9");
        assert_eq!(note_name(48), "C3");
        assert!(is_black_key(61));
        assert!(!is_black_key(60));
    }

    #[test]
    fn grid_lines_levels_and_density() {
        // Four beats per bar, sixteenth subdivisions, readable zoom.
        let lines = grid_lines(0.0, 1920.0, 0.1, 3840, 240, 8.0);
        assert_eq!(lines[0], (0, 2));
        assert!(lines.contains(&(960, 1)));
        assert!(lines.contains(&(240, 0)));
        assert_eq!(lines.len(), 8);
        // Zoomed out: subdivisions vanish, beats stay.
        let lines = grid_lines(0.0, 7680.0, 0.02, 3840, 240, 8.0);
        assert!(lines.iter().all(|(_, l)| *l >= 1));
        assert!(lines.contains(&(960, 1)));
        assert!(lines.contains(&(3840, 2)));
        // Zoomed far out: only bars.
        let lines = grid_lines(0.0, 76800.0, 0.011, 3840, 240, 20.0);
        assert!(lines.iter().all(|(_, l)| *l == 2));
        // Starts mid-way.
        let lines = grid_lines(1000.0, 2000.0, 0.1, 3840, 240, 8.0);
        assert!(lines.iter().all(|(t, _)| *t >= 999 && *t < 2000));
    }
}
