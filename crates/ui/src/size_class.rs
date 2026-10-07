// SPDX-License-Identifier: GPL-3.0-or-later
//! Size classes and the pattern focus (docs/ui-design.md 2.5, 2.6), without
//! GTK. The `AdwBreakpoint`s in `window.rs` use the same thresholds; this
//! module decides what each class means for layout numbers, so the rules
//! are unit tested and `tokens` stay in one place.

/// Widths are in `sp` (scaled with the user's text size).
pub const REGULAR_MAX_SP: u32 = 1399;
pub const COMPACT_MAX_SP: u32 = 900;
pub const NARROW_MAX_SP: u32 = 600;
/// Height below which the short class applies, in pixels.
pub const SHORT_MAX_PX: u32 = 760;
/// Landscape phone: narrow and this short.
pub const LANDSCAPE_MAX_PX: u32 = 500;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Width {
    /// 1400 sp and more: both side panes can sit beside the content.
    Wide,
    /// Up to 1399 sp: side panes overlay the content.
    Regular,
    /// Up to 900 sp: transport extras fold into a menu.
    Compact,
    /// Up to 600 sp: one section at a time, switcher at the bottom.
    Narrow,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SizeClass {
    pub width: Width,
    pub short: bool,
    /// Narrow and short: a landscape phone.
    pub landscape: bool,
}

/// What the Pattern page shows (docs/ui-design.md 2.6).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum PatternFocus {
    #[default]
    Both,
    Steps,
    Notes,
}

impl PatternFocus {
    pub fn name(self) -> &'static str {
        match self {
            PatternFocus::Both => "both",
            PatternFocus::Steps => "steps",
            PatternFocus::Notes => "notes",
        }
    }

    pub fn from_name(s: &str) -> Option<PatternFocus> {
        match s {
            "both" => Some(PatternFocus::Both),
            "steps" => Some(PatternFocus::Steps),
            "notes" => Some(PatternFocus::Notes),
            _ => None,
        }
    }

    /// The next value of the "Focus Steps or Notes" toggle.
    pub fn cycle(self) -> PatternFocus {
        match self {
            PatternFocus::Both => PatternFocus::Steps,
            PatternFocus::Steps => PatternFocus::Notes,
            PatternFocus::Notes => PatternFocus::Both,
        }
    }

    pub fn shows_steps(self) -> bool {
        self != PatternFocus::Notes
    }

    pub fn shows_notes(self) -> bool {
        self != PatternFocus::Steps
    }
}

impl SizeClass {
    pub fn from_size(width_sp: f64, height_px: f64) -> SizeClass {
        let w = width_sp;
        let width = if w <= NARROW_MAX_SP as f64 {
            Width::Narrow
        } else if w <= COMPACT_MAX_SP as f64 {
            Width::Compact
        } else if w <= REGULAR_MAX_SP as f64 {
            Width::Regular
        } else {
            Width::Wide
        };
        SizeClass {
            width,
            short: height_px <= SHORT_MAX_PX as f64,
            landscape: width == Width::Narrow && height_px <= LANDSCAPE_MAX_PX as f64,
        }
    }

    /// Side panes overlay the content instead of sitting beside it.
    pub fn sidebars_overlay(self) -> bool {
        self.width != Width::Wide
    }

    /// Key, time signature, and mode fold into one popover.
    pub fn transport_folds(self) -> bool {
        self.width >= Width::Compact
    }

    /// The header shows the switcher bar at the bottom instead of the
    /// switcher in the header. Not on a landscape phone.
    pub fn bottom_switcher(self) -> bool {
        self.width == Width::Narrow && !self.landscape
    }

    pub fn touch(self) -> bool {
        self.width == Width::Narrow
    }

    /// Channel name column of the step grid, in px.
    pub fn step_name_col(self) -> u32 {
        match self.width {
            Width::Wide | Width::Regular => 200,
            Width::Compact => 140,
            Width::Narrow => 120,
        }
    }

    /// Mixer strip width in px.
    pub fn strip_width(self) -> u32 {
        match self.width {
            Width::Wide | Width::Regular => 96,
            Width::Compact => 80,
            Width::Narrow => 88,
        }
    }

    /// Step row height in px.
    pub fn step_row_h(self) -> u32 {
        if self.touch() { 44 } else { 40 }
    }

    /// Upper bound for the piano roll row height in px.
    pub fn roll_row_h_max(self) -> f64 {
        if self.short { 16.0 } else { 32.0 }
    }

    /// Mixer meter height in px.
    pub fn meter_height(self) -> u32 {
        if self.short { 120 } else { 180 }
    }

    /// Whether the pattern page can show both sections.
    pub fn can_show_both(self) -> bool {
        self.width != Width::Narrow
    }

    /// The focus to use now. `user` is what the user chose (stored in the
    /// view file). Narrow windows show one section at a time, so `Both` is
    /// not available there and falls back to steps.
    pub fn effective_focus(self, user: PatternFocus) -> PatternFocus {
        if !self.can_show_both() && user == PatternFocus::Both {
            PatternFocus::Steps
        } else {
            user
        }
    }

    /// Initial position of the divider as a fraction of the height given to
    /// steps.
    pub fn default_split(self) -> f64 {
        if self.short { 0.4 } else { 0.42 }
    }
}

/// Divider position in pixels for a paned of `total` px with `fraction`
/// for the start child, keeping both children at least `min` px.
pub fn split_position(total: i32, fraction: f64, min: i32) -> i32 {
    if total <= 2 * min {
        return total / 2;
    }
    ((total as f64 * fraction).round() as i32).clamp(min, total - min)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classes_follow_the_thresholds() {
        let c = |w, h| SizeClass::from_size(w, h);
        assert_eq!(c(1920.0, 1080.0).width, Width::Wide);
        assert_eq!(c(1400.0, 900.0).width, Width::Wide);
        assert_eq!(c(1399.0, 900.0).width, Width::Regular);
        assert_eq!(c(901.0, 900.0).width, Width::Regular);
        assert_eq!(c(900.0, 900.0).width, Width::Compact);
        assert_eq!(c(601.0, 900.0).width, Width::Compact);
        assert_eq!(c(600.0, 900.0).width, Width::Narrow);
        assert_eq!(c(360.0, 640.0).width, Width::Narrow);
        assert!(!c(1920.0, 1080.0).short);
        assert!(c(1280.0, 720.0).short);
        assert!(c(1280.0, 760.0).short);
    }

    #[test]
    fn landscape_phone_keeps_the_header_switcher() {
        let phone = SizeClass::from_size(600.0, 360.0);
        assert!(phone.landscape);
        assert!(!phone.bottom_switcher());
        // Wider than narrow: not a landscape phone, header switcher anyway.
        let wider = SizeClass::from_size(640.0, 360.0);
        assert!(!wider.landscape && !wider.bottom_switcher());
        let tiny = SizeClass::from_size(500.0, 400.0);
        assert!(tiny.landscape);
        assert!(!tiny.bottom_switcher());
        let portrait = SizeClass::from_size(360.0, 640.0);
        assert!(portrait.bottom_switcher());
        assert!(portrait.touch());
    }

    #[test]
    fn layout_numbers_by_class() {
        let wide = SizeClass::from_size(1920.0, 1080.0);
        let compact = SizeClass::from_size(800.0, 900.0);
        let narrow = SizeClass::from_size(360.0, 640.0);
        assert_eq!(wide.step_name_col(), 200);
        assert_eq!(compact.step_name_col(), 140);
        assert_eq!(narrow.step_name_col(), 120);
        assert_eq!(wide.strip_width(), 96);
        assert_eq!(compact.strip_width(), 80);
        assert_eq!(narrow.strip_width(), 88);
        assert_eq!(wide.step_row_h(), 40);
        assert_eq!(narrow.step_row_h(), 44);
        assert!(!wide.sidebars_overlay());
        assert!(SizeClass::from_size(1280.0, 720.0).sidebars_overlay());
        assert!(!wide.transport_folds());
        assert!(compact.transport_folds());
        assert_eq!(SizeClass::from_size(1280.0, 720.0).roll_row_h_max(), 16.0);
        assert_eq!(wide.roll_row_h_max(), 32.0);
    }

    #[test]
    fn narrow_never_shows_both_sections() {
        let narrow = SizeClass::from_size(360.0, 640.0);
        assert_eq!(
            narrow.effective_focus(PatternFocus::Both),
            PatternFocus::Steps
        );
        assert_eq!(
            narrow.effective_focus(PatternFocus::Notes),
            PatternFocus::Notes
        );
        let wide = SizeClass::from_size(1920.0, 1080.0);
        assert_eq!(wide.effective_focus(PatternFocus::Both), PatternFocus::Both);
        assert_eq!(
            wide.effective_focus(PatternFocus::Steps),
            PatternFocus::Steps
        );
    }

    #[test]
    fn focus_cycles_and_round_trips() {
        let mut f = PatternFocus::Both;
        let mut seen = Vec::new();
        for _ in 0..3 {
            f = f.cycle();
            seen.push(f);
        }
        assert_eq!(
            seen,
            [PatternFocus::Steps, PatternFocus::Notes, PatternFocus::Both]
        );
        for f in seen {
            assert_eq!(PatternFocus::from_name(f.name()), Some(f));
        }
        assert_eq!(PatternFocus::from_name("nope"), None);
        assert!(PatternFocus::Both.shows_steps() && PatternFocus::Both.shows_notes());
        assert!(!PatternFocus::Notes.shows_steps());
        assert!(!PatternFocus::Steps.shows_notes());
    }

    #[test]
    fn split_position_keeps_both_children() {
        assert_eq!(split_position(800, 0.4, 120), 320);
        assert_eq!(split_position(800, 0.05, 120), 120);
        assert_eq!(split_position(800, 0.99, 120), 680);
        assert_eq!(split_position(200, 0.4, 120), 100);
    }
}
