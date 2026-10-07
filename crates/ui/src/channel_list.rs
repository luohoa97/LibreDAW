// SPDX-License-Identifier: GPL-3.0-or-later
//! The channel header column beside the step grid (docs/ui-design.md 3.3):
//! per channel a color bar, a mute button, the name (double-click or F2 to
//! rename), and a menu. Stock widgets only. Rows have the grid's row height
//! so both columns line up when they scroll together.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::gio;

use protocol::edit::{Edit, MixValue};
use protocol::ids::ChannelId;

use crate::app::{App, UiCommand};
use crate::channels;
use crate::step_logic::RULER_H;
use crate::widgets::color_bar::ColorBar;

pub struct ChannelList {
    pub widget: gtk::Box,
    app: Rc<App>,
    rows: gtk::Box,
    signature: RefCell<String>,
    editing: Cell<u32>,
    labels: RefCell<Vec<(ChannelId, gtk::EditableLabel)>>,
    row_widgets: RefCell<Vec<(ChannelId, gtk::Box)>>,
}

/// What decides whether the rows must be rebuilt.
pub fn signature(app: &App, row_h: u32) -> String {
    let s = app.session.borrow();
    let p = &s.document().project;
    let mut out = format!("{row_h}|");
    for c in &p.channels {
        out.push_str(&format!("{}:{}:{};", c.id, c.name, c.mix.mute as u8));
    }
    out
}

impl ChannelList {
    pub fn new(app: &Rc<App>) -> Rc<ChannelList> {
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 0);
        widget.set_valign(gtk::Align::Start);
        widget.set_hexpand(false);
        widget.set_halign(gtk::Align::Start);
        widget.set_accessible_role(gtk::AccessibleRole::List);
        widget.update_property(&[gtk::accessible::Property::Label("Channels")]);
        let spacer = gtk::Box::new(gtk::Orientation::Vertical, 0);
        spacer.set_height_request(RULER_H as i32);
        widget.append(&spacer);
        let rows = gtk::Box::new(gtk::Orientation::Vertical, 0);
        rows.set_hexpand(false);
        widget.append(&rows);
        let l = Rc::new(ChannelList {
            widget,
            app: app.clone(),
            rows,
            signature: RefCell::new(String::new()),
            editing: Cell::new(0),
            labels: RefCell::new(Vec::new()),
            row_widgets: RefCell::new(Vec::new()),
        });
        l.apply_width();
        let l2 = l.clone();
        app.on_change(move || l2.sync());
        let l2 = l.clone();
        app.on_view_change(move || {
            l2.apply_width();
            l2.sync();
        });
        l.sync();
        l
    }

    fn apply_width(&self) {
        self.widget
            .set_width_request(self.app.size_class().step_name_col() as i32);
    }

    /// Starts renaming the selected channel.
    pub fn rename_selected(&self) {
        let Some(id) = self.app.current_channel() else {
            return;
        };
        if let Some((_, l)) = self.labels.borrow().iter().find(|(c, _)| *c == id) {
            l.start_editing();
        }
    }

    pub fn sync(self: &Rc<ChannelList>) {
        let row_h = self.app.size_class().step_row_h();
        let sig = signature(&self.app, row_h);
        if *self.signature.borrow() != sig && self.editing.get() == 0 {
            *self.signature.borrow_mut() = sig;
            // Not inside the signal handler of a widget that is about to
            // be replaced (the mute button that caused this).
            let me = self.clone();
            gtk::glib::idle_add_local_once(move || {
                me.rebuild(row_h);
                me.update_selection();
            });
        }
        self.update_selection();
    }

    /// Moves the selected style without rebuilding (a click on a row must
    /// not destroy the row that is handling it).
    fn update_selection(&self) {
        let sel = self.app.current_channel();
        for (id, row) in self.row_widgets.borrow().iter() {
            if sel == Some(*id) {
                row.add_css_class("selected");
            } else {
                row.remove_css_class("selected");
            }
        }
    }

    fn rebuild(self: &Rc<ChannelList>, row_h: u32) {
        while let Some(c) = self.rows.first_child() {
            self.rows.remove(&c);
        }
        self.labels.borrow_mut().clear();
        self.row_widgets.borrow_mut().clear();
        let chans = self
            .app
            .session
            .borrow()
            .document()
            .project
            .channels
            .clone();
        for ch in chans {
            let row = self.build_row(ch.id, &ch.name, ch.mix.mute, false, row_h);
            self.rows.append(&row);
            self.row_widgets.borrow_mut().push((ch.id, row));
        }
    }

    fn build_row(
        self: &Rc<ChannelList>,
        id: ChannelId,
        name: &str,
        muted: bool,
        selected: bool,
        row_h: u32,
    ) -> gtk::Box {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        row.add_css_class("ldaw-channel-row");
        row.set_hexpand(false);
        if selected {
            row.add_css_class("selected");
        }
        if muted {
            row.add_css_class("muted");
        }
        row.set_height_request(row_h as i32);
        row.set_margin_start(2);
        row.set_margin_end(2);
        row.set_accessible_role(gtk::AccessibleRole::ListItem);

        let bar = ColorBar::new(id.0);
        bar.set_margin_top(8);
        bar.set_margin_bottom(8);
        row.append(&bar);

        let mute = gtk::ToggleButton::new();
        mute.set_icon_name(if muted {
            "audio-volume-muted-symbolic"
        } else {
            "audio-volume-high-symbolic"
        });
        mute.add_css_class("flat");
        mute.add_css_class("circular");
        mute.set_valign(gtk::Align::Center);
        mute.set_active(muted);
        mute.set_tooltip_text(Some("Mute Channel"));
        mute.update_property(&[gtk::accessible::Property::Label(&format!("Mute {name}"))]);
        {
            let a = self.app.clone();
            mute.connect_toggled(move |b| {
                a.edit(vec![Edit::SetChannelMix {
                    channel: id,
                    value: MixValue::Mute(b.is_active()),
                }]);
            });
        }
        row.append(&mute);

        let label = gtk::EditableLabel::new(name);
        label.set_hexpand(true);
        label.set_width_chars(4);
        label.set_max_width_chars(14);
        label.set_valign(gtk::Align::Center);
        label.add_css_class("ldaw-channel-name");
        label.set_tooltip_text(Some("Double-click to rename"));
        label.update_property(&[gtk::accessible::Property::Label(&format!("Name of {name}"))]);
        {
            let (l, a, orig) = (self.clone(), self.app.clone(), name.to_string());
            label.connect_editing_notify(move |w| {
                if w.is_editing() {
                    l.editing.set(l.editing.get() + 1);
                } else {
                    l.editing.set(l.editing.get().saturating_sub(1));
                    let new = w.text().trim().to_string();
                    if !new.is_empty() && new != orig {
                        a.edit(vec![Edit::RenameChannel {
                            channel: id,
                            name: new,
                        }]);
                    }
                    let l2 = l.clone();
                    gtk::glib::idle_add_local_once(move || l2.sync());
                }
            });
        }
        self.labels.borrow_mut().push((id, label.clone()));
        row.append(&label);

        // A click anywhere on the row selects the channel and plays it.
        let click = gtk::GestureClick::new();
        click.set_propagation_phase(gtk::PropagationPhase::Capture);
        {
            let a = self.app.clone();
            click.connect_pressed(move |_, n, _, _| {
                if n == 1 {
                    a.select_channel(id);
                    let key = {
                        let s = a.session.borrow();
                        s.document().project.channel(id).map(|c| c.root_key)
                    };
                    if let Some(k) = key {
                        a.preview_pulse(id, k, crate::document::DEFAULT_STEP_VEL, 250);
                    }
                }
            });
        }
        row.add_controller(click);

        // The channel menu.
        let menu = gio::Menu::new();
        let first = gio::Menu::new();
        first.append(Some("Edit _Sound"), Some("row.sound"));
        first.append(Some("Edit _Notes"), Some("row.notes"));
        menu.append_section(None, &first);
        let second = gio::Menu::new();
        second.append(Some("_Rename"), Some("row.rename"));
        second.append(Some("Remove _Channel"), Some("row.remove"));
        menu.append_section(None, &second);
        let mb = gtk::MenuButton::new();
        mb.set_icon_name("view-more-symbolic");
        mb.set_menu_model(Some(&menu));
        mb.add_css_class("flat");
        mb.add_css_class("circular");
        mb.set_valign(gtk::Align::Center);
        mb.set_tooltip_text(Some("Channel Menu"));
        mb.update_property(&[gtk::accessible::Property::Label(&format!(
            "Channel menu for {name}"
        ))]);
        row.append(&mb);

        let group = gio::SimpleActionGroup::new();
        let add = |n: &str, f: Box<dyn Fn()>| {
            let a = gio::SimpleAction::new(n, None);
            a.connect_activate(move |_, _| f());
            group.add_action(&a);
        };
        let a = self.app.clone();
        add(
            "sound",
            Box::new(move || {
                a.select_channel(id);
                a.command(UiCommand::ShowSound);
            }),
        );
        let a = self.app.clone();
        add(
            "notes",
            Box::new(move || {
                a.select_channel(id);
                a.command(UiCommand::EditNotes);
            }),
        );
        let lab = label.clone();
        add("rename", Box::new(move || lab.start_editing()));
        let a = self.app.clone();
        add("remove", Box::new(move || channels::remove(&a, id)));
        row.insert_action_group("row", Some(&group));
        row
    }
}
