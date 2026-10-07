// SPDX-License-Identifier: GPL-3.0-or-later
//! What presence looks like (SPEC 18, 18.1): a soft orange glow around the
//! window, an outline on the objects the agent works on, and a pill at the
//! top center with what it is doing and a Stop button. Escape does what
//! Stop does. The logic is in `presence.rs`.
//!
//! The window glow is a widget of its own in an overlay: it takes no
//! input and no space. Objects made of widgets (instrument rows, mixer
//! strips) get a style class that CSS fades in and out; the timeline asks
//! `presence::clip_glow` while it draws.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use adw::prelude::*;
use gtk::glib;
use gtk::subclass::prelude::*;
use gtk::{gdk, gsk};

use crate::app::App;
use crate::presence;
use protocol::control::Focus;

/// How long the glow takes to come up, in seconds.
const EASE_IN: f32 = 0.25;
/// One pulse, in seconds.
const PULSE_S: f64 = 2.0;
const PULSE_LOW: f32 = 0.55;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct Glow {
        /// Eased 0 to 1: how much of the glow shows.
        pub level: Cell<f32>,
        /// 0 to 1 within the pulse.
        pub pulse: Cell<f32>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Glow {
        const NAME: &'static str = "LibreDawAgentGlow";
        type Type = super::Glow;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_accessible_role(gtk::AccessibleRole::Presentation);
            klass.set_css_name("agentglow");
        }
    }

    impl ObjectImpl for Glow {}

    impl WidgetImpl for Glow {
        fn measure(&self, _o: gtk::Orientation, _: i32) -> (i32, i32, i32, i32) {
            (0, 0, -1, -1)
        }

        fn snapshot(&self, s: &gtk::Snapshot) {
            let k = self.level.get() * self.pulse.get();
            if k <= 0.01 {
                return;
            }
            let o = self.obj();
            let (w, h) = (o.width() as f32, o.height() as f32);
            let mut c = o.color();
            let rr = gsk::RoundedRect::from_rect(gtk::graphene::Rect::new(0.0, 0.0, w, h), 0.0);
            // A soft glow that comes in from the edge, and a thin line.
            c.set_alpha(0.55 * k);
            s.append_inset_shadow(&rr, &c, 0.0, 0.0, 0.0, 26.0);
            c.set_alpha(0.85 * k);
            s.append_border(&rr, &[2.0; 4], &[c; 4]);
        }
    }
}

glib::wrapper! {
    pub struct Glow(ObjectSubclass<imp::Glow>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Glow {
    fn new() -> Glow {
        let g: Glow = glib::Object::new();
        g.set_can_target(false);
        g.set_can_focus(false);
        g.set_focusable(false);
        g.add_css_class("ldaw-agent-edge");
        g.imp().pulse.set(1.0);
        g
    }
}

struct Ui {
    glow: Glow,
    pill: gtk::Revealer,
    name: gtk::Label,
    text: gtk::Label,
    /// The label the pill shows now, to avoid resetting it.
    shown: RefCell<String>,
    tick: RefCell<Option<gtk::TickCallbackId>>,
    last_frame: Cell<i64>,
}

thread_local! {
    static UI: RefCell<Option<Rc<Ui>>> = const { RefCell::new(None) };
    static TAGGED: RefCell<Vec<(glib::WeakRef<gtk::Widget>, Focus)>> =
        const { RefCell::new(Vec::new()) };
    static REDRAW: RefCell<Vec<glib::WeakRef<gtk::Widget>>> = const { RefCell::new(Vec::new()) };
}

fn animations_on() -> bool {
    gtk::Settings::default().is_some_and(|s| s.is_gtk_enable_animations())
}

/// The pulse at `t` seconds: a cosine between `PULSE_LOW` and 1.
pub fn pulse_at(t: f64, animated: bool) -> f32 {
    if !animated {
        return 0.85;
    }
    let x = 0.5 - 0.5 * (std::f64::consts::TAU * t / PULSE_S).cos();
    PULSE_LOW + (1.0 - PULSE_LOW) * x as f32
}

/// One step of the glow level toward `target` after `dt` seconds.
pub fn ease(level: f32, target: f32, dt: f32) -> f32 {
    let step = (dt / EASE_IN).min(1.0);
    let v = level + (target - level) * step;
    if (v - target).abs() < 0.01 { target } else { v }
}

/// Adds the glow, the pill, and the Escape key to the window.
/// `edge` is the overlay over the whole window; the pill goes in `content`,
/// the overlay over the pages, below the header.
pub fn install(
    app: &Rc<App>,
    window: &adw::ApplicationWindow,
    edge: &gtk::Overlay,
    content: &gtk::Overlay,
) {
    let glow = Glow::new();
    edge.add_overlay(&glow);
    edge.set_measure_overlay(&glow, false);

    let pill_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    pill_box.add_css_class("osd");
    pill_box.add_css_class("ldaw-agent-pill");
    let spinner = gtk::Spinner::new();
    spinner.set_spinning(true);
    let name = gtk::Label::new(None);
    name.add_css_class("heading");
    let text = gtk::Label::new(None);
    text.set_ellipsize(gtk::pango::EllipsizeMode::End);
    text.set_max_width_chars(48);
    let stop = gtk::Button::with_label("Stop");
    stop.add_css_class("flat");
    stop.add_css_class("ldaw-agent-stop");
    stop.set_tooltip_text(Some("Stop the agent and cancel what it was doing (Escape)"));
    pill_box.append(&spinner);
    pill_box.append(&name);
    pill_box.append(&text);
    pill_box.append(&stop);
    pill_box.set_accessible_role(gtk::AccessibleRole::Status);
    let pill = gtk::Revealer::new();
    pill.set_child(Some(&pill_box));
    pill.set_transition_type(gtk::RevealerTransitionType::Crossfade);
    pill.set_halign(gtk::Align::Center);
    pill.set_valign(gtk::Align::Start);
    pill.set_margin_top(10);
    content.add_overlay(&pill);
    content.set_measure_overlay(&pill, false);
    {
        let a = app.clone();
        stop.connect_clicked(move |_| {
            presence::stop(&a);
        });
    }

    let ui = Rc::new(Ui {
        glow,
        pill,
        name,
        text,
        shown: RefCell::new(String::new()),
        tick: RefCell::new(None),
        last_frame: Cell::new(0),
    });
    UI.with(|u| *u.borrow_mut() = Some(ui.clone()));

    install_escape(app, window);

    let u = ui.clone();
    glib::timeout_add_local(Duration::from_millis(100), move || {
        refresh(&u);
        glib::ControlFlow::Continue
    });
}

/// Escape stops the agent while one is working, unless a popover or dialog
/// is open (they keep Escape). With no agent it keeps its other meanings.
fn install_escape(app: &Rc<App>, window: &adw::ApplicationWindow) {
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let (a, w) = (app.clone(), window.clone());
    keys.connect_key_pressed(move |_, key, _, state| {
        if key != gdk::Key::Escape || !crate::keys::plain(state) || !presence::active() {
            return glib::Propagation::Proceed;
        }
        if overlay_open(&w) {
            return glib::Propagation::Proceed;
        }
        if presence::stop(&a) {
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
    });
    window.add_controller(keys);
}

/// A popover, a dialog or another modal window is open.
fn overlay_open(w: &adw::ApplicationWindow) -> bool {
    if w.visible_dialog().is_some() {
        return true;
    }
    let in_popover = GtkWindowExt::focus(w)
        .and_then(|f| f.ancestor(gtk::Popover::static_type()))
        .is_some();
    in_popover
        || gtk::Window::list_toplevels().iter().any(|t| {
            t.downcast_ref::<gtk::Window>().is_some_and(|t| {
                t != w.upcast_ref::<gtk::Window>() && t.is_visible() && t.is_modal()
            })
        })
}

/// The glow disappears now, not after the ease (the hard stop).
pub fn snap_off() {
    if let Some(ui) = UI.with(|u| u.borrow().clone()) {
        ui.glow.imp().level.set(0.0);
        ui.pill.set_transition_duration(0);
        ui.pill.set_reveal_child(false);
        ui.glow.queue_draw();
        refresh(&ui);
    }
}

/// Makes `w` glow while the agent works on `focus` (an instrument row, a
/// mixer strip). Call it when the widget is built.
pub fn tag(w: &impl IsA<gtk::Widget>, focus: Focus) {
    let w = w.upcast_ref::<gtk::Widget>();
    sync_class(w, focus, Instant::now());
    TAGGED.with(|t| t.borrow_mut().push((w.downgrade(), focus)));
}

/// Redraws `w` (a custom-drawn widget) while anything glows.
pub fn watch_redraw(w: &impl IsA<gtk::Widget>) {
    REDRAW.with(|r| {
        r.borrow_mut()
            .push(w.upcast_ref::<gtk::Widget>().downgrade())
    });
}

/// The pointer is over an activity entry that changed `foci`.
pub fn hover(foci: Vec<Focus>) {
    presence::with(|p| p.set_hover(foci));
    if let Some(ui) = UI.with(|u| u.borrow().clone()) {
        refresh(&ui);
    }
}

fn sync_class(w: &gtk::Widget, focus: Focus, now: Instant) {
    let lit = presence::with(|p| p.intensity(focus, now)) > 0.0;
    if lit {
        if adw::StyleManager::default().is_dark() {
            w.add_css_class("dark");
        } else {
            w.remove_css_class("dark");
        }
        w.add_css_class("ldaw-agent-glow");
    } else {
        w.remove_css_class("ldaw-agent-glow");
    }
}

fn redraw_watched() {
    REDRAW.with(|r| {
        let mut r = r.borrow_mut();
        r.retain(|w| w.upgrade().is_some());
        for w in r.iter().filter_map(|w| w.upgrade()) {
            w.queue_draw();
        }
    });
}

/// Ten times a second, and on every frame while something moves.
fn refresh(ui: &Rc<Ui>) {
    let now = Instant::now();
    let (active, animating, who, label) = presence::with(|p| {
        p.tick(now);
        (
            p.active(now),
            p.animating(now),
            p.who().to_string(),
            p.label(),
        )
    });
    if ui.glow.has_css_class("dark") != adw::StyleManager::default().is_dark() {
        if adw::StyleManager::default().is_dark() {
            ui.glow.add_css_class("dark");
        } else {
            ui.glow.remove_css_class("dark");
        }
    }
    presence::set_glow_color(ui.glow.color());

    if active {
        let changed = *ui.shown.borrow() != format!("{who}\n{label}");
        if changed {
            *ui.shown.borrow_mut() = format!("{who}\n{label}");
            ui.name.set_label(&who);
            ui.text.set_label(&label);
            ui.pill
                .update_property(&[gtk::accessible::Property::Label(&format!("{who}: {label}"))]);
        }
        ui.pill.set_transition_duration(200);
    }
    ui.pill.set_reveal_child(active);

    TAGGED.with(|t| {
        let mut t = t.borrow_mut();
        t.retain(|(w, _)| w.upgrade().is_some());
        for (w, f) in t.iter() {
            if let Some(w) = w.upgrade() {
                sync_class(&w, *f, now);
            }
        }
    });

    let imp = ui.glow.imp();
    let target = if active { 1.0 } else { 0.0 };
    if !animations_on() {
        imp.level.set(target);
        imp.pulse.set(pulse_at(0.0, false));
        ui.glow.queue_draw();
        if animating {
            redraw_watched();
        }
        stop_tick(ui);
    } else if animating || imp.level.get() != target {
        start_tick(ui);
    } else {
        stop_tick(ui);
    }
}

fn start_tick(ui: &Rc<Ui>) {
    if ui.tick.borrow().is_some() {
        return;
    }
    let u = ui.clone();
    let id = ui.glow.add_tick_callback(move |w, clock| {
        let now_us = clock.frame_time();
        let prev = u.last_frame.replace(now_us);
        let dt = if prev == 0 {
            0.016
        } else {
            ((now_us - prev) as f32 / 1e6).clamp(0.0, 0.1)
        };
        let (active, animating) = presence::with(|p| {
            let n = Instant::now();
            (p.active(n), p.animating(n))
        });
        let imp = w.imp();
        imp.level
            .set(ease(imp.level.get(), if active { 1.0 } else { 0.0 }, dt));
        imp.pulse.set(pulse_at(now_us as f64 / 1e6, true));
        w.queue_draw();
        if animating {
            redraw_watched();
        }
        if !animating && imp.level.get() == 0.0 {
            u.last_frame.set(0);
            // Stopped from `refresh`, which sees the same state.
            *u.tick.borrow_mut() = None;
            return glib::ControlFlow::Break;
        }
        glib::ControlFlow::Continue
    });
    *ui.tick.borrow_mut() = Some(id);
}

fn stop_tick(ui: &Rc<Ui>) {
    if let Some(id) = ui.tick.borrow_mut().take() {
        id.remove();
        ui.last_frame.set(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pulse_stays_between_its_two_opacities() {
        for i in 0..40 {
            let v = pulse_at(i as f64 * 0.05, true);
            assert!((PULSE_LOW..=1.0).contains(&v), "{v}");
        }
        assert!((pulse_at(0.0, true) - PULSE_LOW).abs() < 1e-5);
        assert!((pulse_at(1.0, true) - 1.0).abs() < 1e-5);
        assert_eq!(
            pulse_at(1.0, false),
            pulse_at(0.3, false),
            "still when animations are off"
        );
    }

    #[test]
    fn the_level_eases_and_lands() {
        let mut v = 0.0;
        for _ in 0..120 {
            v = ease(v, 1.0, 0.016);
        }
        assert_eq!(v, 1.0);
        assert_eq!(ease(0.5, 0.0, 1.0), 0.0);
    }
}
