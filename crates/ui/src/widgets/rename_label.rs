// SPDX-License-Identifier: GPL-3.0-or-later
//! A name that can be renamed in place. `GtkEditableLabel` starts editing
//! on a click and brings its own text menu (with items that have nothing to
//! do with the object, like "Change Direction"), so this is a label and an
//! entry in a stack instead: clicks go to the object, nothing opens a
//! second menu, and renaming starts only from `start_editing` (F2 or the
//! object's Rename item). Return or leaving the entry commits, Escape
//! cancels.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::prelude::*;

type Commit = Rc<dyn Fn(&str)>;
type Editing = Rc<dyn Fn(bool)>;
type Finished = Rc<dyn Fn()>;

/// What ending an edit does with the typed text: the new name to commit,
/// or `None` (cancelled, empty, or unchanged).
pub fn committed(typed: &str, old: &str, commit: bool) -> Option<String> {
    let new = typed.trim();
    (commit && !new.is_empty() && new != old).then(|| new.to_string())
}

pub struct RenameLabel {
    pub widget: gtk::Stack,
    label: gtk::Label,
    entry: gtk::Entry,
    editing: Cell<bool>,
    on_commit: RefCell<Option<Commit>>,
    on_editing: RefCell<Option<Editing>>,
    on_finished: RefCell<Option<Finished>>,
}

impl RenameLabel {
    pub fn new(text: &str) -> Rc<RenameLabel> {
        let label = gtk::Label::new(Some(text));
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        label.set_xalign(0.0);
        let entry = gtk::Entry::new();
        entry.set_max_length(128);
        entry.update_property(&[gtk::accessible::Property::Label("New name")]);
        let stack = gtk::Stack::new();
        stack.set_hhomogeneous(false);
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
            on_finished: RefCell::new(None),
        });
        let m = Rc::downgrade(&me);
        me.entry.connect_activate(move |_| {
            if let Some(m) = m.upgrade() {
                m.finish(true, true);
            }
        });
        let keys = gtk::EventControllerKey::new();
        let m = Rc::downgrade(&me);
        keys.connect_key_pressed(move |_, key, _, _| {
            if key == gtk::gdk::Key::Escape
                && let Some(m) = m.upgrade()
            {
                m.finish(false, true);
                return gtk::glib::Propagation::Stop;
            }
            gtk::glib::Propagation::Proceed
        });
        me.entry.add_controller(keys);
        let focus = gtk::EventControllerFocus::new();
        let m = Rc::downgrade(&me);
        focus.connect_leave(move |_| {
            if let Some(m) = m.upgrade() {
                m.finish(true, false);
            }
        });
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

    /// Called when editing ends from the keyboard (Return or Escape), so
    /// the owner can take the focus back. Not called when the focus left.
    pub fn connect_finished(&self, f: impl Fn() + 'static) {
        *self.on_finished.borrow_mut() = Some(Rc::new(f));
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
        if let Some(f) = self.on_editing.borrow().clone() {
            f(true);
        }
        self.entry.grab_focus();
        self.entry.select_region(0, -1);
    }

    fn finish(&self, commit: bool, from_key: bool) {
        if !self.editing.replace(false) {
            return;
        }
        let new = committed(&self.entry.text(), &self.label.text(), commit);
        if let Some(n) = &new {
            self.label.set_text(n);
        }
        self.widget.set_visible_child_name("label");
        if from_key && let Some(f) = self.on_finished.borrow().clone() {
            f();
        }
        if let Some(f) = self.on_editing.borrow().clone() {
            f(false);
        }
        if let Some(n) = new
            && let Some(f) = self.on_commit.borrow().clone()
        {
            f(&n);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_ending_an_edit_commits() {
        assert_eq!(committed(" Kick 2 ", "Kick", true), Some("Kick 2".into()));
        assert_eq!(committed("Kick", "Kick", true), None, "unchanged");
        assert_eq!(committed("   ", "Kick", true), None, "empty");
        assert_eq!(committed("Snare", "Kick", false), None, "Escape cancels");
    }
}
