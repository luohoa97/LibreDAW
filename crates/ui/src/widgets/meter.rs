// SPDX-License-Identifier: GPL-3.0-or-later
//! A stereo level meter (docs/ui-design.md 3.5), drawn with `snapshot()`.
//! The owner feeds it peak values in dBFS from the engine's atomics; the
//! meter smooths the fall, holds the peak, and latches a clip lamp. The
//! level is shown as segments, so it reads as a bar length and not only as
//! a color, and the accessible value text is the peak in dB (updated at 4 Hz
//! so a screen reader is not flooded).

use std::cell::Cell;
use std::time::{Duration, Instant};

use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;

use crate::draw::{self, mix};
use crate::palette::{self, Role};

/// Bottom of the scale in dB.
const FLOOR_DB: f32 = -60.0;
const TOP_DB: f32 = 6.0;

/// Maps dBFS to 0..1 on the meter.
pub fn db_to_frac(db: f32) -> f32 {
    ((db - FLOOR_DB) / (TOP_DB - FLOOR_DB)).clamp(0.0, 1.0)
}

/// Linear peak to dBFS (silence is the floor).
pub fn peak_to_db(p: f32) -> f32 {
    if p <= 1e-6 {
        FLOOR_DB - 20.0
    } else {
        20.0 * p.log10()
    }
}

/// Which color band a level (dBFS) is drawn in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Band {
    Good,
    Loud,
    Hot,
    Clip,
}

/// Up to -18 dBFS good, to -6 loud, to -1 hot, above that clipping.
/// How strongly unlit segments and the trough show (mix of the
/// foreground into the background): about 0.22 and 0.08, stronger in
/// high contrast.
pub fn unlit_mix(high_contrast: bool) -> (f32, f32) {
    if high_contrast {
        (0.5, 0.2)
    } else {
        (0.22, 0.08)
    }
}

pub fn band(db: f32) -> Band {
    if db <= -18.0 {
        Band::Good
    } else if db <= -6.0 {
        Band::Loud
    } else if db <= -1.0 {
        Band::Hot
    } else {
        Band::Clip
    }
}

/// The text for a peak: one decimal, "-inf" at the floor.
pub fn db_text(db: f32) -> String {
    if db <= FLOOR_DB {
        "-inf".to_string()
    } else {
        format!("{db:.1}")
    }
}

/// Fall of the displayed level per update, in fraction of the scale.
const FALL: f32 = 0.03;
/// Segment length and gap along the bar, in px.
const SEG: f64 = 3.0;
const GAP: f64 = 1.0;
/// Peak hold time before it falls.
const HOLD: Duration = Duration::from_millis(1500);

mod imp {
    use super::*;

    pub struct Meter {
        pub level: Cell<[f32; 2]>,
        pub hold: Cell<[f32; 2]>,
        pub hold_at: Cell<Option<Instant>>,
        pub clip: Cell<bool>,
        pub horizontal: Cell<bool>,
        pub last_text: Cell<Option<Instant>>,
    }

    impl Default for Meter {
        fn default() -> Meter {
            Meter {
                level: Cell::new([0.0; 2]),
                hold: Cell::new([0.0; 2]),
                hold_at: Cell::new(None),
                clip: Cell::new(false),
                horizontal: Cell::new(false),
                last_text: Cell::new(None),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Meter {
        const NAME: &'static str = "LibreDawMeter";
        type Type = super::Meter;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_accessible_role(gtk::AccessibleRole::Meter);
            klass.set_css_name("meter");
        }
    }

    impl ObjectImpl for Meter {
        fn constructed(&self) {
            self.parent_constructed();
            let o = self.obj();
            o.update_property(&[gtk::accessible::Property::Label("Level")]);
            palette::watch(&*o);
            // Click the clip lamp to reset it.
            let click = gtk::GestureClick::new();
            let w = o.downgrade();
            click.connect_pressed(move |_, _, _, _| {
                if let Some(m) = w.upgrade() {
                    m.reset_clip();
                }
            });
            o.add_controller(click);
        }
    }

    impl WidgetImpl for Meter {
        fn measure(&self, o: gtk::Orientation, _: i32) -> (i32, i32, i32, i32) {
            let horizontal = self.horizontal.get();
            let along = (o == gtk::Orientation::Horizontal) == horizontal;
            let v = if along { 60 } else { 18 };
            (v, v, -1, -1)
        }

        fn snapshot(&self, s: &gtk::Snapshot) {
            let obj = self.obj();
            let colors = palette::colors();
            let fg = colors.get(Role::ViewFg);
            let bg = colors.get(Role::ViewBg);
            let (off_t, trough_t) = unlit_mix(colors.high_contrast);
            let off = mix(&fg, &bg, off_t);
            let trough = mix(&fg, &bg, trough_t);
            let (w, h) = (obj.width() as f64, obj.height() as f64);
            let horizontal = self.horizontal.get();
            // Work in (along, across); `rect` maps back to widget space.
            let (len, thick) = if horizontal { (w, h) } else { (h, w) };
            let rect = |a: f64, t: f64, l: f64, th: f64| {
                if horizontal {
                    (a, t, l, th)
                } else {
                    (t, len - a - l, th, l)
                }
            };
            let lamp = 6.0;
            let bar_len = (len - lamp - 2.0).max(4.0);
            let bar_w = ((thick - 3.0) / 2.0).floor().max(2.0);
            let level = self.level.get();
            let hold = self.hold.get();
            let band_color = |db: f32| match band(db) {
                Band::Good => colors.get(Role::Success),
                Band::Loud => mix(&colors.get(Role::Warning), &colors.get(Role::Success), 0.5),
                Band::Hot => colors.get(Role::Warning),
                Band::Clip => colors.get(Role::Error),
            };
            for ch in 0..2 {
                let t = ch as f64 * (bar_w + 3.0);
                // A trough behind the segments shows the full scale.
                let (x, y, rw, rh) = rect(0.0, t, bar_len, bar_w);
                draw::rounded(s, &trough, x - 1.0, y - 1.0, rw + 2.0, rh + 2.0, 2.0);
                let n = (bar_len / (SEG + GAP)).floor() as usize;
                for i in 0..n {
                    let a = i as f64 * (SEG + GAP);
                    let frac_lo = a / bar_len;
                    let lit = (level[ch] as f64) > frac_lo;
                    let db = FLOOR_DB + (frac_lo as f32) * (TOP_DB - FLOOR_DB);
                    let c = if lit { band_color(db) } else { off };
                    let (x, y, rw, rh) = rect(a, t, SEG, bar_w);
                    draw::fill(s, &c, x, y, rw, rh);
                }
                let ha = hold[ch] as f64 * bar_len;
                if ha > 2.0 {
                    let (x, y, rw, rh) = rect(ha.min(bar_len - 2.0), t, 2.0, bar_w);
                    draw::fill(s, &fg, x, y, rw, rh);
                }
            }
            // Clip lamp at the far end: solid, never blinking.
            let lamp_color = if self.clip.get() {
                colors.get(Role::ErrorBg)
            } else {
                off
            };
            let (x, y, rw, rh) = rect(len - lamp, 0.0, lamp, thick);
            draw::fill(s, &lamp_color, x, y, rw, rh);
        }
    }
}

glib::wrapper! {
    pub struct Meter(ObjectSubclass<imp::Meter>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Meter {
    pub fn new() -> Meter {
        glib::Object::new()
    }

    /// A meter that runs left to right (the transport bar).
    pub fn new_horizontal() -> Meter {
        let m = Meter::new();
        m.imp().horizontal.set(true);
        m
    }

    pub fn set_label(&self, label: &str) {
        self.update_property(&[gtk::accessible::Property::Label(label)]);
        self.set_tooltip_text(Some("Clipped - Click to Reset"));
    }

    pub fn reset_clip(&self) {
        self.imp().clip.set(false);
        self.queue_draw();
    }

    pub fn clipped(&self) -> bool {
        self.imp().clip.get()
    }

    /// Highest held peak of the two channels in dBFS.
    pub fn peak_db(&self) -> f32 {
        let h = self.imp().hold.get();
        FLOOR_DB + h[0].max(h[1]) * (TOP_DB - FLOOR_DB)
    }

    /// Feeds the newest peaks in dBFS (left, right).
    pub fn update(&self, db: [f32; 2]) {
        let imp = self.imp();
        let now = Instant::now();
        let mut level = imp.level.get();
        let mut hold = imp.hold.get();
        let mut raised_hold = false;
        for i in 0..2 {
            let f = db_to_frac(db[i]);
            level[i] = if f >= level[i] {
                f
            } else {
                (level[i] - FALL).max(f)
            };
            if f >= hold[i] {
                hold[i] = f;
                raised_hold = true;
            }
        }
        if raised_hold {
            imp.hold_at.set(Some(now));
        } else if imp.hold_at.get().is_none_or(|t| now - t > HOLD) {
            // The hold has run out: fall toward the level.
            for i in 0..2 {
                hold[i] = (hold[i] - FALL / 3.0).max(level[i]);
            }
        }
        if level != imp.level.get() || hold != imp.hold.get() {
            imp.level.set(level);
            imp.hold.set(hold);
            self.queue_draw();
        }
        if db[0].max(db[1]) >= 0.0 && !imp.clip.replace(true) {
            self.queue_draw();
        }
        // Screen reader text at 4 Hz.
        if imp
            .last_text
            .get()
            .is_none_or(|t| now - t > Duration::from_millis(250))
        {
            imp.last_text.set(Some(now));
            let text = format!("{} decibels", db_text(self.peak_db()));
            self.update_property(&[gtk::accessible::Property::ValueText(&text)]);
        }
    }
}

impl Default for Meter {
    fn default() -> Meter {
        Meter::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scale_mapping() {
        assert_eq!(db_to_frac(-60.0), 0.0);
        assert_eq!(db_to_frac(-100.0), 0.0);
        assert_eq!(db_to_frac(6.0), 1.0);
        assert_eq!(db_to_frac(20.0), 1.0);
        assert!((db_to_frac(-27.0) - 0.5).abs() < 1e-6);
        assert!(db_to_frac(-12.0) < db_to_frac(-1.0));
    }

    #[test]
    fn peak_conversion() {
        assert_eq!(peak_to_db(1.0), 0.0);
        assert!((peak_to_db(0.5) + 6.0206).abs() < 1e-3);
        assert!(peak_to_db(0.0) < -60.0);
    }

    #[test]
    fn color_bands() {
        assert_eq!(band(-40.0), Band::Good);
        assert_eq!(band(-18.0), Band::Good);
        assert_eq!(band(-12.0), Band::Loud);
        assert_eq!(band(-3.0), Band::Hot);
        assert_eq!(band(-0.5), Band::Clip);
        assert_eq!(band(3.0), Band::Clip);
    }

    #[test]
    fn peak_text() {
        assert_eq!(db_text(-3.24), "-3.2");
        assert_eq!(db_text(0.0), "0.0");
        assert_eq!(db_text(-60.0), "-inf");
        assert_eq!(db_text(-80.0), "-inf");
    }
}
