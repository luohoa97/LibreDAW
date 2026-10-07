// SPDX-License-Identifier: GPL-3.0-or-later
//! A name that can be renamed in place. `GtkEditableLabel` brings its own
//! right-click menu (text editing items that have nothing to do with the
//! object), so this is a label and an entry in a stack instead: nothing
//! opens a second menu, and renaming starts only from `start_editing`
//! (the object's menu item or F2). Return or leaving the entry commits,
//! Escape cancels.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::prelude::*;

type Commit = Rc<dyn Fn(&str)>;
type Editing = Rc<dyn Fn(bool)>;

pub struct RenameLabel {
    pub widget: gtk::Stack,
    label: gtk::Label,
    entry: gtk::Entry,
    editing: Cell<bool>,
    on_commit: RefCell<Option<Commit>>,
    on_editing: RefCell<Option<Editing>>,
}

impl RenameLabel {
    pub fn new(text: &str) -> Rc<RenameLabel> {
        let label = gtk::Label::new(Some(text));
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        label.set_xalign(0.0);
        let entry = gtk::Entry::new();
        entry.set_has_frame(false);
        entry.set_max_length(128);
        let stack = gtk::Stack::new();
        stack.add_named(&label, Some("label"));
        stack.add_named(&entry, Some("entry"));
        stack.set_visible_child_name("label");
        let me = Rc::new(RenameLabel {
            widget: stack,
            label,
            entry,
            editing: Cell::new(false),
            on_commit: RefCell::new(None),
            on_editing: RefCell::new(None),
        });
        let m = me.clone();
        me.entry.connect_activate(move |_| m.finish(true));
        let keys = gtk::EventControllerKey::new();
        let m = me.clone();
        keys.connect_key_pressed(move |_, key, _, _| {
            if key == gtk::gdk::Key::Escape {
                m.finish(false);
                gtk::glib::Propagation::Stop
            } else {
                gtk::glib::Propagation::Proceed
            }
        });
        me.entry.add_controller(keys);
        let focus = gtk::EventControllerFocus::new();
        let m = me.clone();
        focus.connect_leave(move |_| m.finish(true));
        me.entry.add_controller(focus);
        me
    }

    /// Called with the new name when the user commits a change.
    pub fn connect_commit(&self, f: impl Fn(&str) + 'static) {
        *self.on_commit.borrow_mut() = Some(Rc::new(f));
    }

    /// Called with `true` when editing starts and `false` when it ends.
    pub fn connect_editing(&self, f: impl Fn(bool) + 'static) {
        *self.on_editing.borrow_mut() = Some(Rc::new(f));
    }

    pub fn text(&self) -> String {
        self.label.text().to_string()
    }

    pub fn set_text(&self, t: &str) {
        self.label.set_text(t);
    }

    pub fn label(&self) -> &gtk::Label {
        &self.label
    }

    pub fn is_editing(&self) -> bool {
        self.editing.get()
    }

    pub fn start_editing(&self) {
        if self.editing.replace(true) {
            return;
        }
        self.entry.set_text(&self.label.text());
        self.widget.set_visible_child_name("entry");
        self.entry.grab_focus();
        self.entry.select_region(0, -1);
        if let Some(f) = self.on_editing.borrow().clone() {
            f(true);
        }
    }

    fn finish(&self, commit: bool) {
        if !self.editing.replace(false) {
            return;
        }
        let new = self.entry.text().trim().to_string();
        self.widget.set_visible_child_name("label");
        if let Some(f) = self.on_editing.borrow().clone() {
            f(false);
        }
        if commit
            && !new.is_empty()
            && new != self.label.text()
            && let Some(f) = self.on_commit.borrow().clone()
        {
            f(&new);
        }
    }
}
