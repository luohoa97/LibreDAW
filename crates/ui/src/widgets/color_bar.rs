// SPDX-License-Identifier: GPL-3.0-or-later
//! A thin bar in a channel's identity color (docs/ui-design.md 3.3, 3.5).
//! Identity only: the name is always next to it, so color carries nothing
//! alone.

use std::cell::Cell;

use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;

use crate::draw;
use crate::palette;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct ColorBar {
        pub id: Cell<u32>,
        pub horizontal: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ColorBar {
        const NAME: &'static str = "LibreDawColorBar";
        type Type = super::ColorBar;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_accessible_role(gtk::AccessibleRole::Presentation);
            klass.set_css_name("colorbar");
        }
    }

    impl ObjectImpl for ColorBar {}

    impl WidgetImpl for ColorBar {
        fn measure(&self, _o: gtk::Orientation, _: i32) -> (i32, i32, i32, i32) {
            (4, 4, -1, -1)
        }

        fn snapshot(&self, s: &gtk::Snapshot) {
            let o = self.obj();
            let c = palette::colors().channel_color(self.id.get());
            draw::rounded(s, &c, 0.0, 0.0, o.width() as f64, o.height() as f64, 2.0);
        }
    }
}

glib::wrapper! {
    pub struct ColorBar(ObjectSubclass<imp::ColorBar>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl ColorBar {
    /// A bar colored for channel `id`.
    pub fn new(id: u32) -> ColorBar {
        let o: ColorBar = glib::Object::new();
        o.imp().id.set(id);
        o
    }

    /// The bar runs along the top edge of a strip instead of the side.
    pub fn set_horizontal(&self, h: bool) {
        self.imp().horizontal.set(h);
    }

    pub fn set_id(&self, id: u32) {
        self.imp().id.set(id);
        self.queue_draw();
    }
}
