// SPDX-License-Identifier: GPL-3.0-or-later
//! The per-step lane editor (docs/ui-design.md 3.3, SPEC 15.4): one row
//! under the step grid, aligned to its columns, that edits the velocity,
//! the pitch offset, or the ratchet count of each active step of the
//! selected channel.
//!
//! Drawing is layered like the grid: bars, baseline, and chips are recorded
//! into a cached render node; the hover column, the value label, and the
//! keyboard cursor are drawn on top each frame.
//!
//! Mouse: press and drag across steps to paint velocity or pitch (one undo
//! step per drag); click a ratchet chip to count up, Shift+click to count
//! down; scroll over a step to nudge it. Keyboard: Left and Right move the
//! cursor, Up and Down change the value (velocity by 8, pitch by 1,
//! ratchet by one choice), Return advances a ratchet, Home and End jump.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::gdk;
use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;

use protocol::edit::Edit;
use protocol::ids::{ChannelId, PatternId};

use crate::app::App;
use crate::draw::{self, Palette, mix};
use crate::lane_logic::{self as lanes, LANE_H, Lane};
use crate::palette::{self, Role};
use crate::render_cache::{LayerCache, LayoutCache};
use crate::step_logic::{self as logic, Cell as StepCell, StepLayout};

#[derive(Clone, Copy, PartialEq)]
pub struct StaticKey {
    revision: u64,
    pattern: Option<PatternId>,
    channel: Option<ChannelId>,
    lane: Option<Lane>,
    palette_gen: u64,
    width: i32,
    height: i32,
    touch: bool,
}

/// What the lane shows for the selected channel.
struct Data {
    pattern: PatternId,
    channel: ChannelId,
    name: String,
    root: u8,
    step_ticks: u32,
    cells: Vec<StepCell>,
}

mod imp {
    use super::*;

    pub struct LaneEditor {
        pub app: RefCell<Option<Rc<App>>>,
        pub lane: Cell<Option<Lane>>,
        pub touch: Cell<bool>,
        pub row_h: Cell<f64>,
        pub cursor: Cell<u32>,
        pub hover: Cell<Option<u32>>,
        /// Last step painted in the open drag.
        pub drag_last: Cell<Option<u32>>,
        pub dragging: Cell<bool>,
        pub drag_start: Cell<(f64, f64)>,
        pub cache: RefCell<LayerCache<StaticKey>>,
        pub text: RefCell<LayoutCache>,
    }

    impl Default for LaneEditor {
        fn default() -> LaneEditor {
            LaneEditor {
                app: RefCell::new(None),
                lane: Cell::new(None),
                touch: Cell::new(false),
                row_h: Cell::new(40.0),
                cursor: Cell::new(0),
                hover: Cell::new(None),
                drag_last: Cell::new(None),
                dragging: Cell::new(false),
                drag_start: Cell::new((0.0, 0.0)),
                cache: RefCell::new(LayerCache::new("lane-editor static")),
                text: RefCell::new(LayoutCache::default()),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for LaneEditor {
        const NAME: &'static str = "LibreDawLaneEditor";
        type Type = super::LaneEditor;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_accessible_role(gtk::AccessibleRole::Grid);
            klass.set_css_name("laneeditor");
        }
    }

    impl ObjectImpl for LaneEditor {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            obj.set_focusable(true);
            obj.set_can_focus(true);
            obj.set_halign(gtk::Align::Fill);
            obj.set_hexpand(true);

            let drag = gtk::GestureDrag::new();
            drag.set_button(gdk::BUTTON_PRIMARY);
            let w = obj.downgrade();
            drag.connect_drag_begin(move |g, x, y| {
                if let Some(o) = w.upgrade() {
                    let shift = g
                        .current_event_state()
                        .contains(gdk::ModifierType::SHIFT_MASK);
                    o.press(x, y, shift);
                }
            });
            let w = obj.downgrade();
            drag.connect_drag_update(move |_, dx, dy| {
                if let Some(o) = w.upgrade() {
                    let (sx, sy) = o.imp().drag_start.get();
                    o.paint_to(sx + dx, sy + dy);
                }
            });
            let w = obj.downgrade();
            drag.connect_drag_end(move |_, _, _| {
                if let Some(o) = w.upgrade() {
                    o.release();
                }
            });
            obj.add_controller(drag);

            let motion = gtk::EventControllerMotion::new();
            let w = obj.downgrade();
            motion.connect_motion(move |_, x, _| {
                if let Some(o) = w.upgrade() {
                    let h = o.step_at(x);
                    if o.imp().hover.replace(h) != h {
                        o.queue_draw();
                    }
                }
            });
            let w = obj.downgrade();
            motion.connect_leave(move |_| {
                if let Some(o) = w.upgrade()
                    && o.imp().hover.replace(None).is_some()
                {
                    o.queue_draw();
                }
            });
            obj.add_controller(motion);

            let scroll = gtk::EventControllerScroll::new(
                gtk::EventControllerScrollFlags::VERTICAL
                    | gtk::EventControllerScrollFlags::DISCRETE,
            );
            let w = obj.downgrade();
            scroll.connect_scroll(move |_, _, dy| match w.upgrade() {
                Some(o) if o.scroll(dy) => glib::Propagation::Stop,
                _ => glib::Propagation::Proceed,
            });
            obj.add_controller(scroll);

            let keys = gtk::EventControllerKey::new();
            let w = obj.downgrade();
            keys.connect_key_pressed(move |_, key, _, state| match w.upgrade() {
                Some(o) if o.key(key, state) => glib::Propagation::Stop,
                _ => glib::Propagation::Proceed,
            });
            obj.add_controller(keys);
        }
    }

    impl WidgetImpl for LaneEditor {
        fn measure(&self, orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            let obj = self.obj();
            if self.lane.get().is_none() {
                return (0, 0, -1, -1);
            }
            let v = if orientation == gtk::Orientation::Horizontal {
                obj.base_content_width()
            } else {
                LANE_H + 4.0
            };
            (v.ceil() as i32, v.ceil() as i32, -1, -1)
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            self.obj().draw(snapshot);
        }
    }
}

glib::wrapper! {
    pub struct LaneEditor(ObjectSubclass<imp::LaneEditor>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl LaneEditor {
    pub fn new(app: Rc<App>) -> LaneEditor {
        let o: LaneEditor = glib::Object::new();
        let w = o.downgrade();
        app.on_change(move || {
            if let Some(o) = w.upgrade() {
                o.queue_resize();
                o.queue_draw();
                o.update_label();
            }
        });
        let w = o.downgrade();
        app.on_view_change(move || {
            if let Some(o) = w.upgrade() {
                o.apply_size_class();
            }
        });
        *o.imp().app.borrow_mut() = Some(app);
        o.apply_size_class();
        o.set_visible(false);
        o
    }

    fn app(&self) -> Rc<App> {
        self.imp().app.borrow().clone().expect("app set")
    }

    fn apply_size_class(&self) {
        let c = self.app().size_class();
        self.imp().touch.set(c.touch());
        self.imp().row_h.set(c.step_row_h() as f64);
        self.queue_resize();
        self.queue_draw();
    }

    /// Opens a lane, or closes the editor with `None`.
    pub fn set_lane(&self, lane: Option<Lane>) {
        self.imp().lane.set(lane);
        self.set_visible(lane.is_some());
        self.queue_resize();
        self.queue_draw();
        self.update_label();
    }

    pub fn lane(&self) -> Option<Lane> {
        self.imp().lane.get()
    }

    fn steps(&self) -> u32 {
        self.data().map(|d| d.cells.len() as u32).unwrap_or(0)
    }

    fn base_content_width(&self) -> f64 {
        let base = StepLayout::cells_only(self.imp().row_h.get(), self.imp().touch.get());
        base.content_size(1, self.steps()).0
    }

    /// The same layout the step grid uses at this width.
    fn layout(&self) -> StepLayout {
        logic::grid_layout(
            self.imp().row_h.get(),
            self.imp().touch.get(),
            self.width() as f64,
            self.steps(),
        )
    }

    fn data(&self) -> Option<Data> {
        let app = self.app();
        let s = app.session.borrow();
        let p = &s.document().project;
        let pat = p.pattern(app.current_pattern()?)?;
        let ch = p.channel(app.current_channel()?)?;
        let rv = logic::row_view(pat, ch);
        Some(Data {
            pattern: pat.id,
            channel: ch.id,
            name: ch.name.clone(),
            root: ch.root_key,
            step_ticks: pat.step_ticks,
            cells: rv.cells,
        })
    }

    /// The step column under `x`, if the pointer is over one.
    fn step_at(&self, x: f64) -> Option<u32> {
        let steps = self.steps();
        let l = self.layout();
        let s = l.nearest_step(x, steps)?;
        let c = l.cell_x(s) + l.cell_w / 2.0;
        ((x - c).abs() <= (l.cell_w + l.cell_gap) / 2.0 + 1.0).then_some(s)
    }

    fn lane_edit(d: &Data, step: u32, lane: Lane, value: i32) -> Edit {
        let (vel, off, repeat) = match lane {
            Lane::Velocity => (Some(value as u8), None, None),
            Lane::Pitch => (None, Some(value as i8), None),
            Lane::Ratchet => (None, None, Some(value as u8)),
        };
        Edit::SetStepLanes {
            pattern: d.pattern,
            channel: d.channel,
            step: step as u8,
            vel,
            off,
            repeat,
        }
    }

    fn set_lanes(&self, d: &Data, step: u32, lane: Lane, value: i32, in_gesture: bool) -> bool {
        let e = vec![Self::lane_edit(d, step, lane, value)];
        let applied = if in_gesture {
            self.app().gesture_edit(e).is_some()
        } else {
            self.app().edit_quiet(e).is_some()
        };
        if applied && lane == Lane::Pitch {
            let key = (d.root as i32 + value).clamp(0, 127) as u8;
            self.app()
                .preview_pulse(d.channel, key, doc::document::DEFAULT_STEP_VEL, 250);
        }
        applied
    }

    fn current(d: &Data, step: u32) -> Option<(u8, i8, u8)> {
        match d.cells.get(step as usize) {
            Some(StepCell::On { vel, off, repeat }) => Some((*vel, *off, *repeat)),
            _ => None,
        }
    }

    /// Writes the value the pointer height means to `step`.
    fn paint_step(&self, d: &Data, step: u32, y: f64, in_gesture: bool) {
        let Some(lane) = self.lane() else { return };
        let Some((vel, off, _)) = Self::current(d, step) else {
            return;
        };
        match lane {
            Lane::Velocity => {
                let v = lanes::vel_from_y(y, LANE_H);
                if v != vel {
                    self.set_lanes(d, step, lane, v as i32, in_gesture);
                }
            }
            Lane::Pitch => {
                let o = lanes::nudge_off(lanes::off_from_y(y, LANE_H), 0, d.root);
                if o != off {
                    self.set_lanes(d, step, lane, o as i32, in_gesture);
                }
            }
            Lane::Ratchet => {}
        }
    }

    fn press(&self, x: f64, y: f64, shift: bool) {
        self.grab_focus();
        self.imp().drag_start.set((x, y));
        let Some(lane) = self.lane() else { return };
        let (Some(d), Some(step)) = (self.data(), self.step_at(x)) else {
            return;
        };
        self.imp().cursor.set(step);
        if let Some((_, _, repeat)) = Self::current(&d, step) {
            if lane == Lane::Ratchet {
                let choices = lanes::ratchet_choices(d.step_ticks);
                let next = lanes::cycle_ratchet(repeat, !shift, &choices);
                if next != repeat {
                    self.app().gesture_begin("Ratchet");
                    self.set_lanes(&d, step, lane, next as i32, true);
                    self.app().gesture_end();
                }
            } else if self.app().gesture_begin(lane.label()) {
                self.imp().dragging.set(true);
                self.imp().drag_last.set(Some(step));
                self.paint_step(&d, step, y, true);
            }
        }
        self.update_label();
        self.queue_draw();
    }

    fn paint_to(&self, x: f64, y: f64) {
        if !self.imp().dragging.get() {
            return;
        }
        let Some(d) = self.data() else { return };
        let steps = d.cells.len() as u32;
        let Some(step) = self.layout().nearest_step(x, steps) else {
            return;
        };
        let from = self.imp().drag_last.get().unwrap_or(step);
        let (a, b) = (from.min(step), from.max(step));
        for s in a..=b {
            self.paint_step(&d, s, y, true);
        }
        self.imp().drag_last.set(Some(step));
        self.imp().cursor.set(step);
    }

    fn release(&self) {
        if self.imp().dragging.replace(false) {
            self.app().gesture_end();
        }
        self.imp().drag_last.set(None);
        self.update_label();
    }

    /// Nudges the value of `step` by one notch in the `up` direction.
    fn nudge(&self, d: &Data, step: u32, up: bool, big: bool) {
        let Some(lane) = self.lane() else { return };
        let Some((vel, off, repeat)) = Self::current(d, step) else {
            return;
        };
        let sign = if up { 1 } else { -1 };
        let (value, cur) = match lane {
            Lane::Velocity => (
                lanes::nudge_vel(vel, sign * if big { 8 } else { 4 }) as i32,
                vel as i32,
            ),
            Lane::Pitch => (
                lanes::nudge_off(off, sign * if big { 12 } else { 1 }, d.root) as i32,
                off as i32,
            ),
            Lane::Ratchet => {
                let c = lanes::ratchet_choices(d.step_ticks);
                (lanes::step_ratchet(repeat, up, &c) as i32, repeat as i32)
            }
        };
        if value != cur {
            let e = Self::lane_edit(d, step, lane, value);
            self.app().edit_resting(lane.label(), vec![e]);
            if lane == Lane::Pitch {
                let key = (d.root as i32 + value).clamp(0, 127) as u8;
                self.app()
                    .preview_pulse(d.channel, key, doc::document::DEFAULT_STEP_VEL, 250);
            }
        }
    }

    fn scroll(&self, dy: f64) -> bool {
        let (Some(d), Some(step)) = (self.data(), self.imp().hover.get()) else {
            return false;
        };
        if Self::current(&d, step).is_none() {
            return false;
        }
        self.nudge(&d, step, dy < 0.0, false);
        true
    }

    fn key(&self, key: gdk::Key, state: gdk::ModifierType) -> bool {
        let Some(d) = self.data() else { return false };
        let steps = d.cells.len() as u32;
        if steps == 0 || self.lane().is_none() {
            return false;
        }
        let cur = self.imp().cursor.get().min(steps - 1);
        let big = state.contains(gdk::ModifierType::CONTROL_MASK);
        let go = |to: u32| {
            self.imp().cursor.set(to.min(steps - 1));
            self.update_label();
            self.queue_draw();
            true
        };
        match key {
            gdk::Key::Left => go(cur.saturating_sub(1)),
            gdk::Key::Right => go(cur + 1),
            gdk::Key::Home => go(0),
            gdk::Key::End => go(steps - 1),
            gdk::Key::Up => {
                self.nudge(&d, cur, true, big);
                self.update_label();
                true
            }
            gdk::Key::Down => {
                self.nudge(&d, cur, false, big);
                self.update_label();
                true
            }
            gdk::Key::Return | gdk::Key::KP_Enter if self.lane() == Some(Lane::Ratchet) => {
                if let Some((_, _, repeat)) = Self::current(&d, cur) {
                    let c = lanes::ratchet_choices(d.step_ticks);
                    let next = lanes::cycle_ratchet(repeat, true, &c);
                    if self.app().gesture_begin("Ratchet") {
                        self.set_lanes(&d, cur, Lane::Ratchet, next as i32, true);
                        self.app().gesture_end();
                    }
                    self.update_label();
                }
                true
            }
            _ => false,
        }
    }

    fn update_label(&self) {
        let Some(lane) = self.lane() else { return };
        let Some(d) = self.data() else {
            self.update_property(&[gtk::accessible::Property::Label(&format!(
                "{} lane",
                lane.label()
            ))]);
            return;
        };
        let name = format!("{} lane for {}", lane.label(), d.name);
        let cur = self.imp().cursor.get();
        let value = match Self::current(&d, cur) {
            Some((v, o, r)) => lanes::value_text(lane, cur, v, o, r),
            None => format!("Step {}, off", cur + 1),
        };
        self.update_property(&[
            gtk::accessible::Property::Label(&name),
            gtk::accessible::Property::ValueText(&value),
        ]);
    }

    fn static_key(&self) -> StaticKey {
        let app = self.app();
        let s = app.session.borrow();
        StaticKey {
            revision: s.document().revision,
            pattern: app.current_pattern(),
            channel: app.current_channel(),
            lane: self.lane(),
            palette_gen: palette::generation(),
            width: self.width(),
            height: self.height(),
            touch: self.imp().touch.get(),
        }
    }

    fn draw(&self, s: &gtk::Snapshot) {
        if self.lane().is_none() {
            return;
        }
        let key = self.static_key();
        {
            let mut cache = self.imp().cache.borrow_mut();
            cache.append(s, key, |rec| self.draw_static(rec));
        }
        self.draw_dynamic(s);
    }

    fn draw_static(&self, s: &gtk::Snapshot) {
        let Some(lane) = self.lane() else { return };
        let Some(d) = self.data() else { return };
        let pal = Palette::current();
        let colors = palette::colors();
        let l = self.layout();
        let (w, h) = (self.width() as f64, LANE_H);
        let steps = d.cells.len() as u32;
        let hc = colors.high_contrast;
        draw::fill(s, &pal.bg, 0.0, 0.0, w, self.height() as f64);
        let body = colors.channel_color(d.channel.0);

        // Beat groups, shaded like the grid above.
        for g in (0..steps.div_ceil(l.group)).filter(|g| g % 2 == 1) {
            let x0 = l.cell_x(g * l.group) - l.cell_gap;
            let last = ((g + 1) * l.group).min(steps) - 1;
            let x1 = l.cell_x(last) + l.cell_w + l.cell_gap;
            let mut c = pal.fg;
            c.set_alpha(0.045);
            draw::fill(s, &c, x0, 0.0, x1 - x0, h);
        }
        // Guides: faint marks at 32, 64, 96 for velocity; the center line
        // for pitch; a baseline for both.
        let faint = mix(&pal.fg, &pal.bg, if hc { 0.5 } else { 0.12 });
        match lane {
            Lane::Velocity => {
                for v in [32u8, 64, 96] {
                    let y = h - 6.0 - (lanes::vel_bar_h(v, h) - 2.0);
                    draw::hline(s, &faint, y, 0.0, w);
                }
                draw::hline(s, &pal.line_beat, h - 6.0, 0.0, w);
            }
            Lane::Pitch => {
                draw::hline(s, &pal.line_bar, h / 2.0, 0.0, w);
            }
            Lane::Ratchet => {
                draw::hline(s, &faint, h - 6.0, 0.0, w);
            }
        }

        let mut text = self.imp().text.borrow_mut();
        for (i, cell) in d.cells.iter().enumerate() {
            let st = i as u32;
            let x = l.cell_x(st);
            let StepCell::On { vel, off, repeat } = *cell else {
                // An off step shows a dot on the baseline, so the column
                // is visibly there but not editable.
                let y = match lane {
                    Lane::Pitch => h / 2.0,
                    _ => h - 6.0,
                };
                draw::fill(s, &faint, x + l.cell_w / 2.0 - 1.5, y - 1.5, 3.0, 3.0);
                continue;
            };
            let bw = (l.cell_w * 0.5).clamp(8.0, 28.0);
            let bx = x + (l.cell_w - bw) / 2.0;
            match lane {
                Lane::Velocity => {
                    let bh = lanes::vel_bar_h(vel, h);
                    let top = h - 6.0 - bh + 2.0;
                    draw::rounded(s, &body, bx, top, bw, bh, 2.0);
                    if hc {
                        draw::outline(s, &pal.fg, bx, top, bw, bh, 1.0);
                    }
                }
                Lane::Pitch => {
                    let (top, bh) = lanes::off_bar(off, h);
                    draw::rounded(s, &body, bx, top, bw, bh, 2.0);
                    if hc {
                        draw::outline(s, &pal.fg, bx, top, bw, bh.max(2.0), 1.0);
                    }
                }
                Lane::Ratchet => {
                    let (cw, ch) = (bw.max(22.0), 22.0);
                    let cx = x + (l.cell_w - cw) / 2.0;
                    let cy = (h - ch) / 2.0;
                    let on = repeat > 1;
                    let fill = if on {
                        body
                    } else {
                        mix(&pal.fg, &pal.bg, 0.12)
                    };
                    draw::rounded(s, &fill, cx, cy, cw, ch, 6.0);
                    let label = text.get(self, &repeat.to_string(), on);
                    let tc = if on {
                        palette::readable_on(&body)
                    } else {
                        pal.text_dim
                    };
                    let (lw, _) = label.pixel_size();
                    let lx = cx + (cw - lw as f64) / 2.0;
                    draw::layout_in(s, &label, &tc, lx, cy, lw as f64 + 1.0, ch);
                }
            }
        }
    }

    fn draw_dynamic(&self, s: &gtk::Snapshot) {
        let Some(lane) = self.lane() else { return };
        let Some(d) = self.data() else { return };
        let l = self.layout();
        let pal = Palette::current();
        let colors = palette::colors();
        let steps = d.cells.len() as u32;
        if steps == 0 {
            return;
        }
        if let Some(h) = self.imp().hover.get().filter(|h| *h < steps) {
            let mut c = pal.fg;
            c.set_alpha(0.1);
            draw::rounded(s, &c, l.cell_x(h), 2.0, l.cell_w, LANE_H - 4.0, 4.0);
        }
        // The value of the hovered or cursor step, as a small caption.
        let focus = self.has_focus();
        let shown = self
            .imp()
            .hover
            .get()
            .or(focus.then(|| self.imp().cursor.get().min(steps - 1)));
        if let Some(st) = shown
            && lane != Lane::Ratchet
            && let Some((vel, off, _)) = Self::current(&d, st)
        {
            let txt = match lane {
                Lane::Velocity => vel.to_string(),
                _ => format!("{off:+}"),
            };
            let layout = self.create_pango_layout(Some(&txt));
            let attrs = gtk::pango::AttrList::new();
            attrs.insert(gtk::pango::AttrFloat::new_scale(0.85));
            layout.set_attributes(Some(&attrs));
            let (tw, th) = layout.pixel_size();
            let x = l.cell_x(st) + (l.cell_w - tw as f64) / 2.0;
            let y = 1.0;
            let mut back = pal.bg;
            back.set_alpha(0.85);
            draw::rounded(s, &back, x - 2.0, y, tw as f64 + 4.0, th as f64 + 1.0, 3.0);
            draw::layout_at(s, &layout, &pal.text, x, y);
        }
        if focus && self.is_focus_visible_now() {
            let st = self.imp().cursor.get().min(steps - 1);
            let ring = if colors.high_contrast {
                colors.get(Role::WindowFg)
            } else {
                colors.get(Role::Accent)
            };
            draw::outline(
                s,
                &ring,
                l.cell_x(st) - 1.0,
                1.0,
                l.cell_w + 2.0,
                LANE_H - 2.0,
                2.0,
            );
        }
    }

    fn is_focus_visible_now(&self) -> bool {
        self.root()
            .and_then(|r| r.downcast::<gtk::Window>().ok())
            .map(|w| w.property::<bool>("focus-visible"))
            .unwrap_or(true)
    }
}
