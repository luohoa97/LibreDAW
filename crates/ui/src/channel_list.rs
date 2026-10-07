// SPDX-License-Identifier: GPL-3.0-or-later
//! The channel header column beside the step grid (docs/ui-design.md 3.3):
//! per channel a color bar, a mute button, the name, and (on wide windows)
//! an Edit Notes button. Stock widgets only. Rows have the grid's row
//! height so both columns line up when they scroll together.
//!
//! Interaction contract (crates/ui/INTERACTIONS.md): a click selects the
//! channel and plays its sound; a double-click or Return opens its notes;
//! the context menu (right click, long press, Menu, Shift+F10) is
//! `menus::channel_menu`; F2 or the menu's Rename edits the name in place.
//! Nothing renames on a single click.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::gio;

use protocol::edit::{Edit, MixValue};
use protocol::ids::ChannelId;

use crate::app::App;
use crate::menus::{self, ROW_ACTIONS};
use crate::timeline_logic as tl;
use crate::widgets::color_bar::ColorBar;
use crate::widgets::rename_label::RenameLabel;

pub struct ChannelList {
    pub widget: gtk::Box,
    app: Rc<App>,
    rows: gtk::ListBox,
    signature: RefCell<String>,
    editing: Cell<u32>,
    labels: RefCell<Vec<(ChannelId, Rc<RenameLabel>)>>,
    row_widgets: RefCell<Vec<(ChannelId, gtk::ListBoxRow)>>,
    selecting: Cell<bool>,
}

/// What decides whether the rows must be rebuilt.
pub fn signature(app: &App, row_h: u32) -> String {
    let s = app.session.borrow();
    let p = &s.document().project;
    let mut out = format!("{row_h}|");
    for (i, c) in p.channels.iter().enumerate() {
        out.push_str(&format!(
            "{}:{}:{}:{}:{}:{};",
            c.id,
            i,
            c.name,
            c.mix.mute as u8,
            c.choke_group,
            s.sample_missing(c) as u8
        ));
    }
    out
}

impl ChannelList {
    pub fn new(app: &Rc<App>, vadj: &gtk::Adjustment) -> Rc<ChannelList> {
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 0);
        widget.set_vexpand(true);
        widget.set_hexpand(false);
        widget.set_halign(gtk::Align::Start);
        let spacer = gtk::Box::new(gtk::Orientation::Vertical, 0);
        spacer.set_height_request(tl::RULER_H as i32);
        widget.append(&spacer);
        // A stock list: hover and selection come from libadwaita.
        let rows = gtk::ListBox::new();
        rows.add_css_class("navigation-sidebar");
        rows.add_css_class("ldaw-channel-list");
        rows.set_selection_mode(gtk::SelectionMode::Single);
        rows.set_activate_on_single_click(false);
        rows.update_property(&[gtk::accessible::Property::Label("Instruments")]);
        rows.set_hexpand(false);
        // Scrolled by the timeline: the names stay level with the lanes.
        let scroller = gtk::ScrolledWindow::builder()
            .child(&rows)
            .vadjustment(vadj)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::External)
            .vexpand(true)
            .build();
        widget.append(&scroller);
        let l = Rc::new(ChannelList {
            widget,
            app: app.clone(),
            rows,
            signature: RefCell::new(String::new()),
            editing: Cell::new(0),
            labels: RefCell::new(Vec::new()),
            row_widgets: RefCell::new(Vec::new()),
            selecting: Cell::new(false),
        });
        l.apply_width();
        // Selecting a row selects the channel everywhere; Return or a
        // double-click on the row opens its notes.
        {
            let l2 = l.clone();
            l.rows.connect_row_selected(move |_, row| {
                if l2.selecting.get() {
                    return;
                }
                if let Some(id) = row.and_then(|r| l2.id_of(r))
                    && l2.app.current_channel() != Some(id)
                {
                    l2.app.select_channel(id);
                }
            });
            let l2 = l.clone();
            l.rows.connect_row_activated(move |_, row| {
                if let Some(id) = l2.id_of(row) {
                    l2.app.edit_notes(Some(id));
                }
            });
        }
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

    fn id_of(&self, row: &gtk::ListBoxRow) -> Option<ChannelId> {
        self.row_widgets
            .borrow()
            .iter()
            .find(|(_, w)| w == row)
            .map(|(id, _)| *id)
    }

    fn apply_width(&self) {
        self.widget
            .set_width_request(self.app.size_class().step_name_col() as i32);
    }

    /// Starts renaming a channel in place (F2, or the menu's Rename).
    pub fn rename(&self, id: ChannelId) {
        let label = self
            .labels
            .borrow()
            .iter()
            .find(|(c, _)| *c == id)
            .map(|(_, l)| l.clone());
        if let Some(l) = label {
            l.start_editing();
        }
    }

    /// Starts renaming the selected channel.
    pub fn rename_selected(&self) {
        if let Some(id) = self.app.current_channel() {
            self.rename(id);
        }
    }

    /// Whether a name is being edited.
    pub fn is_editing(&self) -> bool {
        self.editing.get() > 0
    }

    pub fn sync(self: &Rc<ChannelList>) {
        let row_h = if self.app.size_class().touch() {
            tl::ROW_H_TOUCH
        } else {
            tl::ROW_H
        } as u32;
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
        self.selecting.set(true);
        let row = self
            .row_widgets
            .borrow()
            .iter()
            .find(|(id, _)| Some(*id) == sel)
            .map(|(_, r)| r.clone());
        match row {
            Some(r) if !r.is_selected() => self.rows.select_row(Some(&r)),
            Some(_) => {}
            None => self.rows.unselect_all(),
        }
        self.selecting.set(false);
    }

    fn rebuild(self: &Rc<ChannelList>, row_h: u32) {
        // Keep the keyboard where it was: a rebuilt row takes the focus
        // back when the old one had it.
        let had_focus = self
            .row_widgets
            .borrow()
            .iter()
            .find(|(_, r)| r.has_focus())
            .map(|(id, _)| *id);
        while let Some(c) = self.rows.first_child() {
            self.rows.remove(&c);
        }
        self.labels.borrow_mut().clear();
        self.row_widgets.borrow_mut().clear();
        let (chans, missing) = {
            let s = self.app.session.borrow();
            let chans = s.document().project.channels.clone();
            let missing: Vec<bool> = chans.iter().map(|c| s.sample_missing(c)).collect();
            (chans, missing)
        };
        for (i, (ch, miss)) in chans.into_iter().zip(missing).enumerate() {
            let row = gtk::ListBoxRow::new();
            row.set_activatable(true);
            row.set_height_request(row_h as i32);
            row.update_property(&[gtk::accessible::Property::Label(&ch.name)]);
            row.set_tooltip_text(Some(
                "Double-click or press Return to edit notes; F2 renames",
            ));
            let content = self.build_row(
                &row,
                ch.id,
                i,
                &ch.name,
                ch.mix.mute,
                ch.choke_group,
                miss,
                row_h,
            );
            row.set_child(Some(&content));
            self.rows.append(&row);
            if had_focus == Some(ch.id) {
                row.grab_focus();
            }
            self.row_widgets.borrow_mut().push((ch.id, row));
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn build_row(
        self: &Rc<ChannelList>,
        row: &gtk::ListBoxRow,
        id: ChannelId,
        index: usize,
        name: &str,
        muted: bool,
        choke: u8,
        sample_missing: bool,
        row_h: u32,
    ) -> gtk::Box {
        let content = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        content.add_css_class("ldaw-channel-row");
        crate::presence_ui::tag(&content, protocol::control::Focus::Channel(id));
        content.set_hexpand(false);
        if muted {
            content.add_css_class("muted");
        }
        content.set_height_request(row_h as i32);

        let bar = ColorBar::new(index as u32);
        bar.set_margin_top(8);
        bar.set_margin_bottom(8);
        content.append(&bar);

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
        mute.set_tooltip_text(Some(if muted {
            "Unmute Instrument"
        } else {
            "Mute Instrument"
        }));
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
        content.append(&mute);

        // The name: a plain label until F2 or the menu's Rename. Clicks go
        // to the row (select, double-click opens the notes).
        let label = RenameLabel::new(name);
        label.widget.set_hexpand(true);
        label.widget.set_valign(gtk::Align::Center);
        label.label().set_width_chars(4);
        label.label().set_max_width_chars(14);
        label.label().add_css_class("ldaw-channel-name");
        {
            let l = self.clone();
            label.connect_editing(move |on| {
                if on {
                    l.editing.set(l.editing.get() + 1);
                } else {
                    l.editing.set(l.editing.get().saturating_sub(1));
                    let l2 = l.clone();
                    gtk::glib::idle_add_local_once(move || l2.sync());
                }
            });
            let a = self.app.clone();
            label.connect_commit(move |new| {
                a.edit(vec![Edit::RenameChannel {
                    channel: id,
                    name: new.to_string(),
                }]);
            });
            // Leaving the entry with Return or Escape gives the keyboard
            // back to the row.
            let r = row.clone();
            label.connect_finished(move || {
                r.grab_focus();
            });
        }
        self.labels.borrow_mut().push((id, label.clone()));
        content.append(&label.widget);
        if sample_missing {
            // A placeholder: the channel stays, silent, until the file is found.
            let w = gtk::Image::from_icon_name("dialog-warning-symbolic");
            w.set_tooltip_text(Some("The sound file cannot be found"));
            w.update_property(&[gtk::accessible::Property::Label(
                "The sound file cannot be found",
            )]);
            content.append(&w);
        }
        // A click anywhere on the row plays the channel's sound (the list
        // selects it).
        let click = gtk::GestureClick::new();
        click.set_propagation_phase(gtk::PropagationPhase::Capture);
        {
            let a = self.app.clone();
            click.connect_pressed(move |_, n, _, _| {
                if n == 1 {
                    let key = {
                        let s = a.session.borrow();
                        s.document().project.channel(id).map(|c| c.root_key)
                    };
                    if let Some(k) = key {
                        a.preview_pulse(id, k, doc::document::DEFAULT_STEP_VEL, 250);
                    }
                }
            });
        }
        content.add_controller(click);

        // The one menu of a channel.
        let group = gio::SimpleActionGroup::new();
        for name in ROW_ACTIONS {
            let action = if *name == "choke" {
                gio::SimpleAction::new_stateful(
                    name,
                    Some(gtk::glib::VariantTy::INT32),
                    &(choke as i32).to_variant(),
                )
            } else if matches!(*name, "drive" | "duck") {
                gio::SimpleAction::new(name, Some(gtk::glib::VariantTy::INT32))
            } else {
                gio::SimpleAction::new(name, None)
            };
            let (a, n) = (self.app.clone(), name.to_string());
            action.connect_activate(move |act, v| {
                let target = v.and_then(|v| v.get::<i32>());
                if let (Some(t), true) = (target, n == "choke") {
                    act.set_state(&t.to_variant());
                }
                menus::perform_row_action(&a, id, &n, target);
            });
            group.add_action(&action);
        }
        row.insert_action_group("row", Some(&group));
        let a = self.app.clone();
        crate::context_menu::attach(row, &menus::channel_menu(), move |_| {
            if a.current_channel() != Some(id) {
                a.select_channel(id);
            }
            true
        });
        content
    }
}
