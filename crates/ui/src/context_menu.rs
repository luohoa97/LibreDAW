// SPDX-License-Identifier: GPL-3.0-or-later
//! One way to open an object's menu (docs/ui-design.md 3.3, HIG Menus): a
//! right click or a long press opens it at the pointer; the Menu key and
//! Shift+F10 open it at the focused object. The popover is made when it
//! opens and unparented when it closes, so nothing is left behind when the
//! object's widget is rebuilt.
//!
//! Custom widgets (no layout manager) call `present_popovers` from their
//! `size_allocate` and `unparent_popovers` from `dispose`.

use std::cell::RefCell;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gdk, gio, glib};

/// Whether a key press asks for the context menu: the Menu key, or
/// Shift+F10, with no other modifier.
pub fn is_menu_key(key: gdk::Key, state: gdk::ModifierType) -> bool {
    let mods = crate::keys::mods(state);
    match key {
        gdk::Key::Menu => mods.is_empty(),
        gdk::Key::F10 => mods == gdk::ModifierType::SHIFT_MASK,
        _ => false,
    }
}

/// Where the menu points.
#[derive(Clone, Copy, Debug)]
pub enum Anchor {
    /// At the pointer, in widget coordinates.
    Pointer(f64, f64),
    /// Under the whole widget.
    Widget,
    /// Under a part of the widget (the focused cell or note).
    Rect(gdk::Rectangle),
}

type Prepare = Rc<dyn Fn(Option<(f64, f64)>) -> Option<Anchor>>;

struct Inner {
    widget: glib::WeakRef<gtk::Widget>,
    model: gio::MenuModel,
    prepare: Prepare,
    open: RefCell<Option<gtk::PopoverMenu>>,
}

impl Inner {
    fn popup(self: &Rc<Inner>, at: Option<(f64, f64)>) -> bool {
        let Some(w) = self.widget.upgrade() else {
            return false;
        };
        let Some(anchor) = (self.prepare)(at) else {
            return false;
        };
        if let Some(old) = self.open.borrow_mut().take() {
            old.unparent();
        }
        let pop = gtk::PopoverMenu::from_model(Some(&self.model));
        pop.set_parent(&w);
        let rect = match anchor {
            Anchor::Pointer(x, y) => {
                pop.set_has_arrow(false);
                pop.set_halign(gtk::Align::Start);
                gdk::Rectangle::new(x as i32, y as i32, 1, 1)
            }
            Anchor::Widget => gdk::Rectangle::new(0, 0, w.width().max(1), w.height().max(1)),
            Anchor::Rect(r) => r,
        };
        pop.set_pointing_to(Some(&rect));
        pop.set_position(gtk::PositionType::Bottom);
        let me = Rc::downgrade(self);
        pop.connect_closed(move |p| {
            let (me, p) = (me.clone(), p.clone());
            // After the activated item ran: its action may need the parent.
            glib::idle_add_local_once(move || {
                if let Some(me) = me.upgrade() {
                    let mine = me.open.borrow().as_ref() == Some(&p);
                    if mine {
                        me.open.borrow_mut().take();
                    }
                }
                if p.parent().is_some() {
                    p.unparent();
                }
            });
        });
        *self.open.borrow_mut() = Some(pop.clone());
        pop.popup();
        true
    }
}

/// Gives `widget` the context menu `model`. `prepare` runs before it opens
/// with the pointer position (`None` from the keyboard); it can select the
/// object, and returns `false` to open nothing. Actions in the model
/// resolve from `widget` upwards.
pub fn attach(
    widget: &impl IsA<gtk::Widget>,
    model: &impl IsA<gio::MenuModel>,
    prepare: impl Fn(Option<(f64, f64)>) -> bool + 'static,
) {
    attach_at(widget, model, move |at| {
        prepare(at).then_some(match at {
            Some((x, y)) => Anchor::Pointer(x, y),
            None => Anchor::Widget,
        })
    });
}

/// As `attach`, with `prepare` choosing where the menu points.
pub fn attach_at(
    widget: &impl IsA<gtk::Widget>,
    model: &impl IsA<gio::MenuModel>,
    prepare: impl Fn(Option<(f64, f64)>) -> Option<Anchor> + 'static,
) {
    let widget = widget.as_ref();
    let inner = Rc::new(Inner {
        widget: widget.downgrade(),
        model: model.clone().upcast(),
        prepare: Rc::new(prepare),
        open: RefCell::new(None),
    });

    let click = gtk::GestureClick::new();
    click.set_button(gdk::BUTTON_SECONDARY);
    let i = inner.clone();
    click.connect_pressed(move |g, _, x, y| {
        if i.popup(Some((x, y))) {
            g.set_state(gtk::EventSequenceState::Claimed);
        }
    });
    widget.add_controller(click);

    let long = gtk::GestureLongPress::new();
    long.set_touch_only(true);
    let i = inner.clone();
    long.connect_pressed(move |g, x, y| {
        if i.popup(Some((x, y))) {
            g.set_state(gtk::EventSequenceState::Claimed);
        }
    });
    widget.add_controller(long);

    let keys = gtk::EventControllerKey::new();
    let i = inner.clone();
    keys.connect_key_pressed(move |_, key, _, state| {
        if is_menu_key(key, state) && i.popup(None) {
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
    });
    widget.add_controller(keys);

    widget.connect_destroy(move |_| {
        if let Some(p) = inner.open.borrow_mut().take()
            && p.parent().is_some()
        {
            p.unparent();
        }
    });
}

/// Allocates the popovers parented to a custom widget.
pub fn present_popovers(w: &gtk::Widget) {
    let mut c = w.first_child();
    while let Some(x) = c {
        if let Some(p) = x.downcast_ref::<gtk::Popover>() {
            p.present();
        }
        c = x.next_sibling();
    }
}

/// Unparents the popovers of a custom widget that is going away.
pub fn unparent_popovers(w: &gtk::Widget) {
    let mut c = w.first_child();
    while let Some(x) = c {
        c = x.next_sibling();
        if x.is::<gtk::Popover>() {
            x.unparent();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_keys() {
        let none = gdk::ModifierType::empty();
        let shift = gdk::ModifierType::SHIFT_MASK;
        let ctrl = gdk::ModifierType::CONTROL_MASK;
        assert!(is_menu_key(gdk::Key::Menu, none));
        assert!(is_menu_key(gdk::Key::F10, shift));
        assert!(
            !is_menu_key(gdk::Key::F10, none),
            "F10 alone is the primary menu"
        );
        assert!(!is_menu_key(gdk::Key::F10, shift | ctrl));
        assert!(!is_menu_key(gdk::Key::Menu, ctrl));
        assert!(!is_menu_key(gdk::Key::Return, none));
        // Lock keys do not count as modifiers.
        assert!(is_menu_key(gdk::Key::Menu, gdk::ModifierType::LOCK_MASK));
    }
}
