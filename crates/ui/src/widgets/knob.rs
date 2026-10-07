// SPDX-License-Identifier: GPL-3.0-or-later
//! A flat rotary knob (docs/ui-design.md 3.7) drawn with `snapshot()`: a
//! 270 degree track, the value in the accent color, a pointer, no gradients
//! or shadows. It works in unit values 0 to 1; the owner maps those to a
//! parameter and sets the text that goes with the value.
//!
//! Mouse: vertical drag (200 px for the whole range, 1000 px with Shift),
//! double-click resets, the wheel acts only while the knob has focus.
//! Keyboard: arrows 1 percent, Page Up and Page Down 10 percent, Home and
//! End the ends, Backspace resets. Role `slider` with value and value text.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene};

use crate::draw;
use crate::knob_logic::{drag_unit, step_unit};
use crate::palette::{self, Role};
use crate::render_cache::LayerCache;

const SIZE: i32 = 48;
const SEGMENTS: i32 = 27;

type Edited = Rc<dyn Fn(f64)>;

#[derive(Clone, Copy, PartialEq)]
pub struct KnobKey {
    unit: u64,
    default: u64,
    width: i32,
    height: i32,
    palette_gen: u64,
}

mod imp {
    use super::*;

    pub struct Knob {
        pub unit: Cell<f64>,
        pub default: Cell<f64>,
        pub drag_from: Cell<f64>,
        pub on_edit: RefCell<Option<Edited>>,
        pub cache: RefCell<LayerCache<KnobKey>>,
    }

    impl Default for Knob {
        fn default() -> Knob {
            Knob {
                unit: Cell::new(0.0),
                default: Cell::new(0.5),
                drag_from: Cell::new(0.0),
                on_edit: RefCell::new(None),
                cache: RefCell::new(LayerCache::new("knob")),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Knob {
        const NAME: &'static str = "LibreDawKnob";
        type Type = super::Knob;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_accessible_role(gtk::AccessibleRole::Slider);
            klass.set_css_name("knob");
        }
    }

    impl ObjectImpl for Knob {
        fn constructed(&self) {
            self.parent_constructed();
            let o = self.obj();
            o.set_focusable(true);
            o.set_can_focus(true);
            o.set_halign(gtk::Align::Center);
            o.set_cursor_from_name(Some("ns-resize"));
            palette::watch(&*o);

            let drag = gtk::GestureDrag::new();
            drag.set_button(gdk::BUTTON_PRIMARY);
            let w = o.downgrade();
            drag.connect_drag_begin(move |_, _, _| {
                if let Some(k) = w.upgrade() {
                    k.grab_focus();
                    k.imp().drag_from.set(k.imp().unit.get());
                }
            });
            let w = o.downgrade();
            drag.connect_drag_update(move |g, _, dy| {
                if let Some(k) = w.upgrade() {
                    let fine = g
                        .current_event_state()
                        .contains(gdk::ModifierType::SHIFT_MASK);
                    k.edit(drag_unit(k.imp().drag_from.get(), dy, fine));
                }
            });
            o.add_controller(drag);

            let click = gtk::GestureClick::new();
            let w = o.downgrade();
            click.connect_pressed(move |_, n, _, _| {
                if n == 2
                    && let Some(k) = w.upgrade()
                {
                    k.edit(k.imp().default.get());
                }
            });
            o.add_controller(click);

            let scroll = gtk::EventControllerScroll::new(
                gtk::EventControllerScrollFlags::VERTICAL
                    | gtk::EventControllerScrollFlags::DISCRETE,
            );
            let w = o.downgrade();
            scroll.connect_scroll(move |c, _, dy| match w.upgrade() {
                Some(k) if k.has_focus() => {
                    let fine = c
                        .current_event_state()
                        .contains(gdk::ModifierType::SHIFT_MASK);
                    k.edit(step_unit(k.imp().unit.get(), -dy, fine));
                    glib::Propagation::Stop
                }
                _ => glib::Propagation::Proceed,
            });
            o.add_controller(scroll);

            let keys = gtk::EventControllerKey::new();
            let w = o.downgrade();
            keys.connect_key_pressed(move |_, key, _, st| {
                let Some(k) = w.upgrade() else {
                    return glib::Propagation::Proceed;
                };
                // Plain keys, and Shift for fine steps.
                let fine = crate::keys::only(st, gdk::ModifierType::SHIFT_MASK);
                if !(crate::keys::plain(st) || fine) {
                    return glib::Propagation::Proceed;
                }
                let u = k.imp().unit.get();
                let new = match key {
                    gdk::Key::Up | gdk::Key::Right => {
                        (u + if fine { 0.002 } else { 0.01 }).min(1.0)
                    }
                    gdk::Key::Down | gdk::Key::Left => {
                        (u - if fine { 0.002 } else { 0.01 }).max(0.0)
                    }
                    gdk::Key::Page_Up => (u + 0.1).min(1.0),
                    gdk::Key::Page_Down => (u - 0.1).max(0.0),
                    gdk::Key::Home => 0.0,
                    gdk::Key::End => 1.0,
                    gdk::Key::BackSpace | gdk::Key::Delete => k.imp().default.get(),
                    _ => return glib::Propagation::Proceed,
                };
                k.edit(new);
                glib::Propagation::Stop
            });
            o.add_controller(keys);
        }
    }

    impl WidgetImpl for Knob {
        fn measure(&self, _o: gtk::Orientation, _: i32) -> (i32, i32, i32, i32) {
            (SIZE, SIZE, -1, -1)
        }

        fn snapshot(&self, s: &gtk::Snapshot) {
            let o = self.obj();
            let key = KnobKey {
                unit: self.unit.get().to_bits(),
                default: self.default.get().to_bits(),
                width: o.width(),
                height: o.height(),
                palette_gen: palette::generation(),
            };
            let unit = self.unit.get();
            self.cache.borrow_mut().append(s, key, |rec| {
                draw_knob(rec, o.width() as f64, o.height() as f64, unit)
            });
            if o.has_focus()
                && o.root().is_some_and(|r| {
                    r.downcast::<gtk::Window>()
                        .map(|w| w.property::<bool>("focus-visible"))
                        .unwrap_or(true)
                })
            {
                let colors = palette::colors();
                let ring = if colors.high_contrast {
                    colors.get(Role::WindowFg)
                } else {
                    colors.get(Role::Accent)
                };
                let (w, h) = (o.width() as f64, o.height() as f64);
                draw::outline(s, &ring, 1.0, 1.0, w - 2.0, h - 2.0, 2.0);
            }
        }
    }
}

fn draw_knob(s: &gtk::Snapshot, w: f64, h: f64, unit: f64) {
    let colors = palette::colors();
    let fg = colors.get(Role::ViewFg);
    let mut track = fg;
    track.set_alpha(0.18);
    let accent = colors.get(Role::AccentBg);
    let pointer = colors.get(Role::WindowFg);
    let (cx, cy) = (w / 2.0, h / 2.0);
    let r = (w.min(h) / 2.0 - 3.0).max(8.0);
    // A disc behind the pointer.
    let mut face = fg;
    face.set_alpha(0.07);
    draw::rounded(
        s,
        &face,
        cx - (r - 7.0),
        cy - (r - 7.0),
        2.0 * (r - 7.0),
        2.0 * (r - 7.0),
        r - 7.0,
    );
    for i in 0..=SEGMENTS {
        let f = i as f64 / SEGMENTS as f64;
        let angle = -135.0 + 270.0 * f;
        let lit = unit > 0.0 && f <= unit + 1e-9;
        let c = if lit { accent } else { track };
        s.save();
        s.translate(&graphene::Point::new(cx as f32, cy as f32));
        s.rotate(angle as f32);
        s.append_color(&c, &draw::rect(-1.5, -r, 3.0, 5.0));
        s.restore();
    }
    let angle = -135.0 + 270.0 * unit;
    s.save();
    s.translate(&graphene::Point::new(cx as f32, cy as f32));
    s.rotate(angle as f32);
    s.append_color(&pointer, &draw::rect(-1.0, -(r - 8.0), 2.0, r - 14.0));
    s.restore();
}

glib::wrapper! {
    pub struct Knob(ObjectSubclass<imp::Knob>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Knob {
    pub fn new(label: &str) -> Knob {
        let k: Knob = glib::Object::new();
        k.update_property(&[
            gtk::accessible::Property::Label(label),
            gtk::accessible::Property::ValueMin(0.0),
            gtk::accessible::Property::ValueMax(1.0),
            gtk::accessible::Property::ValueNow(0.0),
        ]);
        k
    }

    /// Called with the new unit value whenever the user changes the knob.
    pub fn connect_edited(&self, f: impl Fn(f64) + 'static) {
        *self.imp().on_edit.borrow_mut() = Some(Rc::new(f));
    }

    pub fn unit(&self) -> f64 {
        self.imp().unit.get()
    }

    pub fn set_default_unit(&self, u: f64) {
        self.imp().default.set(u.clamp(0.0, 1.0));
    }

    /// Sets the position without calling the owner back.
    pub fn set_unit(&self, u: f64) {
        let u = u.clamp(0.0, 1.0);
        if (self.imp().unit.replace(u) - u).abs() > 1e-12 {
            self.update_property(&[gtk::accessible::Property::ValueNow(u)]);
            self.queue_draw();
        }
    }

    /// The words that go with the value ("2.0 kHz").
    pub fn set_value_text(&self, t: &str) {
        self.update_property(&[gtk::accessible::Property::ValueText(t)]);
    }

    fn edit(&self, u: f64) {
        let before = self.imp().unit.get();
        self.set_unit(u);
        if (self.imp().unit.get() - before).abs() > 1e-12 {
            let cb = self.imp().on_edit.borrow().clone();
            if let Some(cb) = cb {
                cb(self.imp().unit.get());
            }
        }
    }
}
