// SPDX-License-Identifier: GPL-3.0-or-later
//! A stereo level meter (SPEC 11), drawn with `snapshot()`. The owner feeds
//! it peak values in dBFS from the engine's atomics; the meter smooths the
//! fall and holds the peak briefly.

use std::cell::Cell;

use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;

use crate::draw::{self, Palette};

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

/// Fall of the displayed level per update, in fraction of the scale.
const FALL: f32 = 0.03;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct Meter {
        pub level: Cell<[f32; 2]>,
        pub hold: Cell<[f32; 2]>,
        pub clip: Cell<bool>,
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
            self.obj().set_size_request(18, 80);
            self.obj()
                .update_property(&[gtk::accessible::Property::Label("Level meter")]);
        }
    }

    impl WidgetImpl for Meter {
        fn measure(&self, o: gtk::Orientation, _: i32) -> (i32, i32, i32, i32) {
            let v = if o == gtk::Orientation::Horizontal {
                18
            } else {
                80
            };
            (v, v, -1, -1)
        }

        fn snapshot(&self, s: &gtk::Snapshot) {
            let obj = self.obj();
            let pal = Palette::of(obj.as_ref());
            let (w, h) = (obj.width() as f64, obj.height() as f64);
            let bar_w = ((w - 3.0) / 2.0).floor().max(2.0);
            let level = self.level.get();
            let hold = self.hold.get();
            for ch in 0..2 {
                let x = ch as f64 * (bar_w + 3.0);
                draw::fill(s, &pal.cell_off, x, 0.0, bar_w, h);
                let fh = level[ch] as f64 * h;
                // Green, then amber above -12 dB, red above -1 dB.
                let amber_at = db_to_frac(-12.0) as f64 * h;
                let red_at = db_to_frac(-1.0) as f64 * h;
                let green = pal.ok;
                let amber = pal.warn;
                let red = pal.error;
                for (lo, hi, col) in [
                    (0.0, amber_at, green),
                    (amber_at, red_at, amber),
                    (red_at, h, red),
                ] {
                    let top = fh.min(hi);
                    if top > lo {
                        draw::fill(s, &col, x, h - top, bar_w, top - lo);
                    }
                }
                let hy = hold[ch] as f64 * h;
                if hy > 1.0 {
                    draw::fill(s, &pal.text, x, h - hy, bar_w, 1.0);
                }
            }
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

    /// Feeds the newest peaks in dBFS (left, right).
    pub fn update(&self, db: [f32; 2]) {
        let imp = self.imp();
        let mut level = imp.level.get();
        let mut hold = imp.hold.get();
        for i in 0..2 {
            let f = db_to_frac(db[i]);
            level[i] = if f >= level[i] {
                f
            } else {
                (level[i] - FALL).max(f)
            };
            hold[i] = if f >= hold[i] {
                f
            } else {
                (hold[i] - FALL / 4.0).max(level[i])
            };
        }
        if level != imp.level.get() || hold != imp.hold.get() {
            imp.level.set(level);
            imp.hold.set(hold);
            self.queue_draw();
        }
        if db[0].max(db[1]) > 0.0 {
            imp.clip.set(true);
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
}
