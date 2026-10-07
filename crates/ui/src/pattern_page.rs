// SPDX-License-Identifier: GPL-3.0-or-later
//! The Pattern page (docs/ui-design.md 3.2): steps over notes in one
//! vertical paned.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::gio;

use protocol::consts::{MAX_STEPS, MIN_STEPS};
use protocol::edit::Edit;

use crate::app::App;
use crate::dialogs;
use crate::view_math::SNAPS;
use crate::widgets::piano_roll::PianoRoll;
use crate::widgets::step_grid::StepGrid;

pub struct PatternPage {
    pub widget: gtk::Widget,
    pub roll: PianoRoll,
    pub snap: gtk::DropDown,
    pub paned: gtk::Paned,
}

pub fn build(window: &adw::ApplicationWindow, app: &Rc<App>) -> PatternPage {
    let rack = build_rack(window, app);
    let (roll, roll_widget, snap) = build_roll(app);
    let paned = gtk::Paned::new(gtk::Orientation::Vertical);
    paned.set_start_child(Some(&rack));
    paned.set_end_child(Some(&roll_widget));
    paned.set_resize_start_child(false);
    paned.set_shrink_start_child(false);
    paned.set_position(250);
    PatternPage {
        widget: paned.clone().upcast(),
        roll,
        snap,
        paned,
    }
}

fn icon_button(icon: &str, tip: &str, action: Option<&str>) -> gtk::Button {
    let b = gtk::Button::from_icon_name(icon);
    b.set_tooltip_text(Some(tip));
    b.update_property(&[gtk::accessible::Property::Label(tip)]);
    if let Some(a) = action {
        b.set_action_name(Some(a));
    }
    b
}

/// Pattern bar, step grid, and the add channel button.
fn build_rack(window: &adw::ApplicationWindow, app: &Rc<App>) -> gtk::Widget {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);

    // Pattern bar.
    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    bar.add_css_class("toolbar");
    let plabel = gtk::Label::new(Some("Pattern"));
    plabel.add_css_class("heading");
    let patterns = gtk::DropDown::from_strings(&[]);
    patterns.set_tooltip_text(Some("Pattern shown in the rack and the piano roll"));
    let add_pattern = icon_button("list-add-symbolic", "New pattern", None);
    let del_pattern = icon_button("user-trash-symbolic", "Remove this pattern", None);
    let steps_label = gtk::Label::new(Some("Steps"));
    steps_label.add_css_class("dim-label");
    let steps = gtk::SpinButton::with_range(MIN_STEPS as f64, MAX_STEPS as f64, 1.0);
    steps.set_tooltip_text(Some("Pattern length in steps"));
    steps.update_property(&[gtk::accessible::Property::Label("Pattern length in steps")]);
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    let add_channel = gtk::MenuButton::new();
    add_channel.set_child(Some(
        &adw::ButtonContent::builder()
            .icon_name("list-add-symbolic")
            .label("Add channel")
            .build(),
    ));
    let cm = gio::Menu::new();
    cm.append(Some("Synth"), Some("win.add-synth"));
    cm.append(Some("Plugin instrument…"), Some("win.add-instrument"));
    add_channel.set_menu_model(Some(&cm));
    let rename = icon_button(
        "document-edit-symbolic",
        "Rename the selected channel",
        None,
    );
    bar.append(&plabel);
    bar.append(&patterns);
    bar.append(&add_pattern);
    bar.append(&del_pattern);
    bar.append(&steps_label);
    bar.append(&steps);
    bar.append(&spacer);
    bar.append(&rename);
    bar.append(&add_channel);
    root.append(&bar);
    root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

    let grid = StepGrid::new(app.clone());
    let sw = gtk::ScrolledWindow::builder()
        .child(&grid)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .build();
    root.append(&sw);

    let updating = Rc::new(Cell::new(false));
    let ids: Rc<RefCell<Vec<protocol::ids::PatternId>>> = Rc::new(RefCell::new(Vec::new()));

    {
        let a = app.clone();
        add_pattern.connect_clicked(move |_| {
            let n = a.session.borrow().document().project.patterns.len() + 1;
            if let Some(r) = a.edit(vec![Edit::AddPattern {
                name: format!("Pattern {n}"),
                length_steps: 16,
            }]) {
                a.select_pattern(protocol::ids::PatternId(r.created[0]));
            }
        });
        let a = app.clone();
        del_pattern.connect_clicked(move |_| {
            if let Some(p) = a.current_pattern() {
                a.edit(vec![Edit::RemovePattern { pattern: p }]);
            }
        });
        let (a, ids2, up) = (app.clone(), ids.clone(), updating.clone());
        patterns.connect_selected_notify(move |d| {
            if up.get() {
                return;
            }
            if let Some(id) = ids2.borrow().get(d.selected() as usize).copied() {
                a.select_pattern(id);
            }
        });
        let (a, up) = (app.clone(), updating.clone());
        steps.connect_value_changed(move |s| {
            if up.get() {
                return;
            }
            if let Some(p) = a.current_pattern() {
                let v = s.value() as u8;
                let cur = a
                    .session
                    .borrow()
                    .document()
                    .project
                    .pattern(p)
                    .map(|x| x.length_steps);
                if cur != Some(v) {
                    a.edit(vec![Edit::SetPatternLength {
                        pattern: p,
                        length_steps: v,
                    }]);
                }
            }
        });
        let (a, w) = (app.clone(), window.clone());
        rename.connect_clicked(move |_| {
            let Some(c) = a.current_channel() else { return };
            let cur = a
                .session
                .borrow()
                .document()
                .project
                .channel(c)
                .map(|x| x.name.clone())
                .unwrap_or_default();
            let a2 = a.clone();
            dialogs::ask_name(&w, "Rename channel", &cur, move |n| {
                a2.edit(vec![Edit::RenameChannel {
                    channel: c,
                    name: n,
                }]);
            });
        });
    }

    let sync = {
        let (a, ids, up, patterns, steps) = (
            app.clone(),
            ids.clone(),
            updating.clone(),
            patterns.clone(),
            steps.clone(),
        );
        move || {
            up.set(true);
            let (names, new_ids, sel_len) = {
                let s = a.session.borrow();
                let p = &s.document().project;
                let names: Vec<String> = p.patterns.iter().map(|x| x.name.clone()).collect();
                let new_ids: Vec<_> = p.patterns.iter().map(|x| x.id).collect();
                let len = a
                    .current_pattern()
                    .and_then(|id| p.pattern(id))
                    .map(|x| x.length_steps as f64);
                (names, new_ids, len)
            };
            let cur_names: Vec<String> = patterns
                .model()
                .and_then(|m| m.downcast::<gtk::StringList>().ok())
                .map(|l| {
                    (0..l.n_items())
                        .filter_map(|i| l.string(i).map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();
            if cur_names != names {
                let refs: Vec<&str> = names.iter().map(String::as_str).collect();
                patterns.set_model(Some(&gtk::StringList::new(&refs)));
            }
            *ids.borrow_mut() = new_ids.clone();
            if let Some(i) = a
                .current_pattern()
                .and_then(|id| new_ids.iter().position(|x| *x == id))
                && patterns.selected() as usize != i
            {
                patterns.set_selected(i as u32);
            }
            if let Some(l) = sel_len
                && (steps.value() - l).abs() > 0.5
            {
                steps.set_value(l);
            }
            up.set(false);
        }
    };
    sync();
    app.on_change(sync);
    root.upcast()
}

/// The piano roll with its toolbar and scrollbars.
fn build_roll(app: &Rc<App>) -> (PianoRoll, gtk::Widget, gtk::DropDown) {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let tools = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    tools.add_css_class("toolbar");
    let title = gtk::Label::new(Some("Piano roll"));
    title.add_css_class("heading");
    let chan = gtk::Label::new(None);
    chan.add_css_class("dim-label");
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    let snap_label = gtk::Label::new(Some("Snap"));
    snap_label.add_css_class("dim-label");
    let labels: Vec<&str> = SNAPS.iter().map(|s| s.0).collect();
    let snap = gtk::DropDown::from_strings(&labels);
    snap.set_tooltip_text(Some("Grid that notes snap to"));
    let zx_out = icon_button("zoom-out-symbolic", "Zoom out in time", None);
    let zx_in = icon_button("zoom-in-symbolic", "Zoom in in time", None);
    let zy_out = icon_button("go-down-symbolic", "Make rows smaller", None);
    let zy_in = icon_button("go-up-symbolic", "Make rows taller", None);
    let tl = gtk::Label::new(Some("Time"));
    tl.add_css_class("dim-label");
    let pl = gtk::Label::new(Some("Pitch"));
    pl.add_css_class("dim-label");
    for w in [
        title.upcast_ref::<gtk::Widget>(),
        chan.upcast_ref(),
        spacer.upcast_ref(),
        snap_label.upcast_ref(),
        snap.upcast_ref(),
        tl.upcast_ref(),
        zx_out.upcast_ref(),
        zx_in.upcast_ref(),
        pl.upcast_ref(),
        zy_out.upcast_ref(),
        zy_in.upcast_ref(),
    ] {
        tools.append(w);
    }
    root.append(&tools);
    root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

    let roll = PianoRoll::new(app.clone());
    let grid = gtk::Grid::new();
    let vs = gtk::Scrollbar::new(gtk::Orientation::Vertical, Some(&roll.vadj()));
    let hs = gtk::Scrollbar::new(gtk::Orientation::Horizontal, Some(&roll.hadj()));
    grid.attach(&roll, 0, 0, 1, 1);
    grid.attach(&vs, 1, 0, 1, 1);
    grid.attach(&hs, 0, 1, 1, 1);
    grid.set_vexpand(true);
    root.append(&grid);

    {
        let r = roll.clone();
        snap.connect_selected_notify(move |d| r.set_snap_index(d.selected() as usize));
        let r = roll.clone();
        zx_in.connect_clicked(move |_| r.zoom_x(1.3));
        let r = roll.clone();
        zx_out.connect_clicked(move |_| r.zoom_x(1.0 / 1.3));
        let r = roll.clone();
        zy_in.connect_clicked(move |_| r.zoom_y(1.2));
        let r = roll.clone();
        zy_out.connect_clicked(move |_| r.zoom_y(1.0 / 1.2));
    }
    let a = app.clone();
    let sync = move || {
        let s = a.session.borrow();
        let p = &s.document().project;
        let name = a
            .current_channel()
            .and_then(|c| p.channel(c))
            .map(|c| c.name.clone());
        chan.set_text(&name.map(|n| format!("— {n}")).unwrap_or_default());
    };
    sync();
    app.on_change(sync);
    (roll, root.upcast(), snap)
}
