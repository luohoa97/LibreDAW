// SPDX-License-Identifier: GPL-3.0-or-later
//! The Patterns lane's header (SPEC 20.7): a menu button that lists the
//! patterns of the project, each with Place at Playhead and Rename.

use std::rc::Rc;

use adw::prelude::*;

use protocol::edit::Edit;

use crate::app::App;
use crate::pattern_logic;

/// The button beside the lane. Its list is made each time it opens.
pub fn header(app: &Rc<App>) -> gtk::MenuButton {
    let btn = gtk::MenuButton::new();
    btn.set_label("Patterns");
    btn.add_css_class("flat");
    btn.add_css_class("caption");
    btn.set_tooltip_text(Some(pattern_logic::TOOLTIP));
    btn.update_property(&[gtk::accessible::Property::Label(
        "Patterns: the beats and sections of this song",
    )]);
    let a = app.clone();
    btn.set_create_popup_func(move |b| b.set_popover(Some(&popover(&a, b))));
    btn
}

fn popover(app: &Rc<App>, button: &gtk::MenuButton) -> gtk::Popover {
    let pop = gtk::Popover::new();
    let col = gtk::Box::new(gtk::Orientation::Vertical, 6);
    col.set_margin_top(8);
    col.set_margin_bottom(8);
    col.set_margin_start(8);
    col.set_margin_end(8);
    let list = {
        let s = app.session.borrow();
        pattern_logic::list(&s.document().project)
    };
    if list.is_empty() {
        let hint = gtk::Label::new(Some(
            "No patterns yet. Select clips on different rows, then press Ctrl+G to make one.",
        ));
        hint.add_css_class("dim-label");
        hint.set_wrap(true);
        hint.set_max_width_chars(32);
        col.append(&hint);
    }
    for (group, name, placed) in list {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let label = gtk::Label::new(Some(&format!("{name} ({placed} placed)")));
        label.set_xalign(0.0);
        label.set_hexpand(true);
        row.append(&label);
        let place = gtk::Button::with_label("Place at Playhead");
        place.set_tooltip_text(Some(
            "Adds another copy of this pattern where the playhead is",
        ));
        let (a, p) = (app.clone(), pop.clone());
        place.connect_clicked(move |_| {
            let at = a.playhead_tick().min(u32::MAX as u64) as u32;
            a.edit(vec![pattern_logic::place_edit(group, at)]);
            p.popdown();
        });
        row.append(&place);
        let rename = gtk::Button::with_label("Rename");
        rename.set_tooltip_text(Some("Changes this pattern's name"));
        let (a, p, b, old) = (app.clone(), pop.clone(), button.clone(), name.clone());
        rename.connect_clicked(move |_| {
            p.popdown();
            let a = a.clone();
            crate::dialogs::ask_name(&b, "Rename Pattern", &old, move |n| {
                a.edit(vec![Edit::RenameGroup { group, name: n }]);
            });
        });
        row.append(&rename);
        col.append(&row);
    }
    pop.set_child(Some(&col));
    pop
}
