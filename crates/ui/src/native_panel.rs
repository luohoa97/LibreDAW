// SPDX-License-Identifier: GPL-3.0-or-later
//! A grid of captioned knobs for the native parameters of the sampler, the
//! 808 and the built-in effects (docs/ui-design.md 3.5, 3.7). The owner
//! gives it a list of `NativeSpec` and a callback; the panel keeps the
//! knobs, captions and value texts in step with the document.

use std::rc::Rc;

use gtk::prelude::*;

use crate::native_logic::NativeSpec;
use crate::widgets::knob::Knob;

pub struct ParamPanel {
    pub widget: gtk::Grid,
    knobs: Vec<(Knob, gtk::Label)>,
    specs: Vec<NativeSpec>,
    /// Index of the first knob in the owner's parameter table.
    base: usize,
}

impl ParamPanel {
    /// Knobs for `specs`, `cols` per row. `on_edit(index, value)` is called
    /// when the user turns a knob; `label` names the owner for assistive
    /// technology ("Kick", "Reverb").
    pub fn new(
        specs: Vec<NativeSpec>,
        base: usize,
        cols: i32,
        label: &str,
        on_edit: impl Fn(usize, f64) + 'static,
    ) -> Rc<ParamPanel> {
        let grid = gtk::Grid::new();
        grid.set_row_spacing(12);
        grid.set_column_spacing(6);
        grid.set_halign(gtk::Align::Center);
        let on_edit = Rc::new(on_edit);
        let mut knobs = Vec::new();
        for (i, s) in specs.iter().enumerate() {
            let cell = gtk::Box::new(gtk::Orientation::Vertical, 2);
            cell.set_size_request(72, -1);
            let knob = Knob::new(&format!("{} of {label}", s.name));
            knob.set_tooltip_text(Some(s.tip));
            knob.set_default_unit(s.to_unit(s.default));
            let cap = gtk::Label::new(Some(s.name));
            cap.add_css_class("caption");
            cap.set_ellipsize(gtk::pango::EllipsizeMode::End);
            let read = gtk::Label::new(Some(" "));
            read.add_css_class("caption");
            read.add_css_class("numeric");
            read.add_css_class("dim-label");
            read.set_ellipsize(gtk::pango::EllipsizeMode::End);
            cell.append(&knob);
            cell.append(&cap);
            cell.append(&read);
            grid.attach(&cell, i as i32 % cols, i as i32 / cols, 1, 1);
            let (s, on_edit) = (*s, on_edit.clone());
            knob.connect_edited(move |u| on_edit(base + i, s.from_unit(u)));
            knobs.push((knob, read));
        }
        Rc::new(ParamPanel {
            widget: grid,
            knobs,
            specs,
            base,
        })
    }

    /// Shows the document's values without calling the owner back.
    pub fn set_values(&self, value: impl Fn(usize) -> f64) {
        for (i, ((knob, read), s)) in self.knobs.iter().zip(&self.specs).enumerate() {
            let v = value(self.base + i);
            knob.set_unit(s.to_unit(v));
            let t = s.format(v);
            read.set_text(&t);
            knob.set_value_text(&t);
        }
    }
}
