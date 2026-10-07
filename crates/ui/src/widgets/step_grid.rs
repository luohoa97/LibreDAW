// SPDX-License-Identifier: GPL-3.0-or-later
//! The step grid (docs/ui-design.md 3.3): the cells of every channel row in
//! one widget, drawn with `snapshot()`. The channel names, mute buttons, and
//! menus are stock widgets in a header column beside it; both use the same
//! row height so rows line up.
//!
//! Drawing is layered. The static layer (ruler, bands, cells) is recorded
//! once into a render node and re-appended until the document, the
//! selection, the size, or the style changes. The playhead, the hover cell,
//! and the keyboard cursor are drawn on top each frame as a few rectangles.
//!
//! Mouse: press a cell to turn it on or off (the first cell of a stroke
//! sets the mode, dragging paints it; one undo step per stroke), and the
//! step sounds when it turns on. Keyboard: arrows move the cursor, Return
//! toggles it, Delete clears it, Home and End jump to the row ends, Page Up
//! and Page Down jump a bar. The widget is one tab stop with role `grid`.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, graphene};

use protocol::edit::Edit;
use protocol::ids::{ChannelId, PatternId};

use crate::app::App;
use crate::draw::{self, Palette, mix};
use crate::palette::{self, Role};
use crate::perf;
use crate::render_cache::{LayerCache, LayoutCache};
use crate::step_logic::{self as logic, Cell as StepCell, Hit, StepLayout};

/// What the static layer depends on.
#[derive(Clone, Copy, PartialEq)]
pub struct StaticKey {
    revision: u64,
    pattern: Option<PatternId>,
    selected: Option<ChannelId>,
    palette_gen: u64,
    width: i32,
    height: i32,
    touch: bool,
}

mod imp {
    use super::*;

    pub struct StepGrid {
        pub app: RefCell<Option<Rc<App>>>,
        pub layout: Cell<StepLayout>,
        pub touch: Cell<bool>,
        pub cursor: Cell<(usize, u32)>,
        pub hover: Cell<Option<(usize, u32)>>,
        /// Open paint stroke: the state being painted and the last step
        /// painted.
        pub stroke: RefCell<Option<Stroke>>,
        pub drag_start: Cell<(f64, f64)>,
        pub last_playhead: Cell<u64>,
        pub cache: RefCell<LayerCache<StaticKey>>,
        pub text: RefCell<LayoutCache>,
    }

    impl Default for StepGrid {
        fn default() -> StepGrid {
            StepGrid {
                app: RefCell::new(None),
                layout: Cell::new(StepLayout::cells_only(40.0, false)),
                touch: Cell::new(false),
                cursor: Cell::new((0, 0)),
                hover: Cell::new(None),
                stroke: RefCell::new(None),
                drag_start: Cell::new((0.0, 0.0)),
                last_playhead: Cell::new(0),
                cache: RefCell::new(LayerCache::new("step-grid static")),
                text: RefCell::new(LayoutCache::default()),
            }
        }
    }

    pub struct Stroke {
        pub on: bool,
        pub row: usize,
        pub last: Option<u32>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for StepGrid {
        const NAME: &'static str = "LibreDawStepGrid";
        type Type = super::StepGrid;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_accessible_role(gtk::AccessibleRole::Grid);
            klass.set_css_name("stepgrid");
        }
    }

    impl ObjectImpl for StepGrid {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            obj.set_focusable(true);
            obj.set_can_focus(true);
            obj.set_halign(gtk::Align::Start);
            obj.update_property(&[gtk::accessible::Property::Label("Steps")]);

            let drag = gtk::GestureDrag::new();
            drag.set_button(gdk::BUTTON_PRIMARY);
            let w = obj.downgrade();
            drag.connect_drag_begin(move |_, x, y| {
                if let Some(o) = w.upgrade() {
                    o.press(x, y);
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
            motion.connect_motion(move |_, x, y| {
                if let Some(o) = w.upgrade() {
                    o.hover_at(Some((x, y)));
                }
            });
            let w = obj.downgrade();
            motion.connect_leave(move |_| {
                if let Some(o) = w.upgrade() {
                    o.hover_at(None);
                }
            });
            obj.add_controller(motion);

            let keys = gtk::EventControllerKey::new();
            let w = obj.downgrade();
            keys.connect_key_pressed(move |_, key, _, _| match w.upgrade() {
                Some(o) if o.key(key) => glib::Propagation::Stop,
                _ => glib::Propagation::Proceed,
            });
            obj.add_controller(keys);

            let w = obj.downgrade();
            obj.add_tick_callback(move |_, _| {
                if let Some(o) = w.upgrade() {
                    o.tick();
                    glib::ControlFlow::Continue
                } else {
                    glib::ControlFlow::Break
                }
            });
        }
    }

    impl WidgetImpl for StepGrid {
        fn measure(&self, orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            let (w, h) = self.obj().content_size();
            let v = if orientation == gtk::Orientation::Horizontal {
                w
            } else {
                h
            };
            (v.ceil() as i32, v.ceil() as i32, -1, -1)
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            self.obj().draw(snapshot);
        }
    }
}

glib::wrapper! {
    pub struct StepGrid(ObjectSubclass<imp::StepGrid>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl StepGrid {
    pub fn new(app: Rc<App>) -> StepGrid {
        let o: StepGrid = glib::Object::new();
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
        o
    }

    fn app(&self) -> Rc<App> {
        self.imp().app.borrow().clone().expect("app set")
    }

    /// Row height and cell size follow the size class.
    fn apply_size_class(&self) {
        let c = self.app().size_class();
        let touch = c.touch();
        self.imp().touch.set(touch);
        self.imp()
            .layout
            .set(StepLayout::cells_only(c.step_row_h() as f64, touch));
        self.queue_resize();
        self.queue_draw();
    }

    /// Row height (the header column uses the same value).
    pub fn row_height(&self) -> f64 {
        self.imp().layout.get().row_h
    }

    /// Rows and steps of what is shown now.
    fn dims(&self) -> (usize, u32) {
        let app = self.app();
        let s = app.session.borrow();
        let p = &s.document().project;
        let steps = app
            .current_pattern()
            .and_then(|id| p.pattern(id))
            .map(|pt| pt.length_steps as u32)
            .unwrap_or(0);
        (p.channels.len(), steps)
    }

    fn content_size(&self) -> (f64, f64) {
        let (rows, steps) = self.dims();
        self.imp().layout.get().content_size(rows, steps)
    }

    fn channel_at(&self, row: usize) -> Option<ChannelId> {
        let app = self.app();
        let s = app.session.borrow();
        s.document().project.channels.get(row).map(|c| c.id)
    }

    fn pattern_id(&self) -> Option<PatternId> {
        self.app().current_pattern()
    }

    fn is_read_only(&self, row: usize) -> bool {
        let app = self.app();
        let s = app.session.borrow();
        let p = &s.document().project;
        let (Some(pid), Some(ch)) = (app.current_pattern(), p.channels.get(row)) else {
            return false;
        };
        p.pattern(pid)
            .map(|pat| logic::row_view(pat, ch).piano_roll_data)
            .unwrap_or(false)
    }

    fn cell_state(&self, row: usize, step: u32) -> StepCell {
        let app = self.app();
        let s = app.session.borrow();
        let p = &s.document().project;
        let (Some(pid), Some(ch)) = (app.current_pattern(), p.channels.get(row)) else {
            return StepCell::Off;
        };
        p.pattern(pid)
            .and_then(|pat| logic::row_view(pat, ch).cells.get(step as usize).copied())
            .unwrap_or(StepCell::Off)
    }

    fn set_step(&self, row: usize, step: u32, on: bool, in_gesture: bool) {
        let (Some(pattern), Some(channel)) = (self.pattern_id(), self.channel_at(row)) else {
            return;
        };
        if matches!(self.cell_state(row, step), StepCell::On { .. }) == on {
            return;
        }
        let e = vec![Edit::SetStep {
            pattern,
            channel,
            step: step as u8,
            on,
            vel: None,
        }];
        if in_gesture {
            self.app().gesture_edit(e);
        } else {
            self.app().edit_quiet(e);
        }
        if on {
            self.preview_row(row);
        }
    }

    /// Plays the channel's root key (a step plays that key, 5.2).
    fn preview_row(&self, row: usize) {
        let Some(channel) = self.channel_at(row) else {
            return;
        };
        let app = self.app();
        let key = {
            let s = app.session.borrow();
            s.document().project.channel(channel).map(|c| c.root_key)
        };
        if let Some(key) = key {
            app.preview_pulse(channel, key, crate::document::DEFAULT_STEP_VEL, 250);
        }
    }

    fn hover_at(&self, p: Option<(f64, f64)>) {
        let (rows, steps) = self.dims();
        let layout = self.imp().layout.get();
        let h = p.and_then(|(x, y)| match layout.hit(x, y, rows, steps) {
            Hit::Cell { row, step } => Some((row, step)),
            _ => None,
        });
        if self.imp().hover.replace(h) != h {
            self.queue_draw();
        }
    }

    fn press(&self, x: f64, y: f64) {
        self.grab_focus();
        self.imp().drag_start.set((x, y));
        let (rows, steps) = self.dims();
        let layout = self.imp().layout.get();
        if let Hit::Cell { row, step } = layout.hit(x, y, rows, steps) {
            self.imp().cursor.set((row, step));
            if let Some(c) = self.channel_at(row) {
                self.app().select_channel(c);
            }
            if self.is_read_only(row) {
                self.app()
                    .toast("This row has piano roll notes. Edit it in the piano roll.");
                return;
            }
            let on = !matches!(self.cell_state(row, step), StepCell::On { .. });
            if self.app().gesture_begin("Steps") {
                *self.imp().stroke.borrow_mut() = Some(imp::Stroke {
                    on,
                    row,
                    last: Some(step),
                });
                self.set_step(row, step, on, true);
            }
        }
        self.update_label();
        self.queue_draw();
    }

    fn paint_to(&self, x: f64, _y: f64) {
        let (_, steps) = self.dims();
        let (row, on, last) = match self.imp().stroke.borrow().as_ref() {
            Some(s) => (s.row, s.on, s.last),
            None => return,
        };
        let Some(step) = self.imp().layout.get().nearest_step(x, steps) else {
            return;
        };
        if last == Some(step) {
            return;
        }
        // Fill every step between the last painted one and this one, so a
        // fast stroke leaves no holes.
        let from = last.unwrap_or(step);
        let (a, b) = (from.min(step), from.max(step));
        for s in a..=b {
            self.set_step(row, s, on, true);
        }
        if let Some(st) = self.imp().stroke.borrow_mut().as_mut() {
            st.last = Some(step);
        }
        self.imp().cursor.set((row, step));
    }

    fn release(&self) {
        if self.imp().stroke.borrow_mut().take().is_some() {
            self.app().gesture_end();
        }
        self.update_label();
    }

    fn key(&self, key: gdk::Key) -> bool {
        let (rows, steps) = self.dims();
        if rows == 0 || steps == 0 {
            return false;
        }
        let cur = self.imp().cursor.get();
        let bar = self.steps_per_bar() as i32;
        let (dr, ds) = match key {
            gdk::Key::Left => (0, -1),
            gdk::Key::Right => (0, 1),
            gdk::Key::Up => (-1, 0),
            gdk::Key::Down => (1, 0),
            gdk::Key::Home => (0, -(steps as i32)),
            gdk::Key::End => (0, steps as i32),
            gdk::Key::Page_Up => (0, -bar),
            gdk::Key::Page_Down => (0, bar),
            gdk::Key::Return | gdk::Key::KP_Enter => {
                let (row, step) = logic::move_cursor(cur, 0, 0, rows, steps);
                if self.is_read_only(row) {
                    self.app()
                        .toast("This row has piano roll notes. Edit it in the piano roll.");
                } else {
                    let on = !matches!(self.cell_state(row, step), StepCell::On { .. });
                    self.set_step(row, step, on, false);
                }
                self.update_label();
                return true;
            }
            gdk::Key::Delete | gdk::Key::BackSpace => {
                let (row, step) = logic::move_cursor(cur, 0, 0, rows, steps);
                if !self.is_read_only(row) {
                    self.set_step(row, step, false, false);
                }
                self.update_label();
                return true;
            }
            _ => return false,
        };
        let next = logic::move_cursor(cur, dr, ds, rows, steps);
        self.imp().cursor.set(next);
        if let Some(c) = self.channel_at(next.0)
            && self.app().current_channel() != Some(c)
        {
            self.app().select_channel(c);
        }
        self.update_label();
        self.queue_draw();
        true
    }

    fn steps_per_bar(&self) -> u32 {
        let beats = self.app().session.borrow().document().project.time_sig_num as u32;
        beats * 4
    }

    /// Updates the accessible label with the cursor cell (SPEC 11).
    fn update_label(&self) {
        let (rows, steps) = self.dims();
        if rows == 0 || steps == 0 {
            self.update_property(&[gtk::accessible::Property::Label("Steps, no channels")]);
            return;
        }
        let (row, step) = logic::move_cursor(self.imp().cursor.get(), 0, 0, rows, steps);
        let name = {
            let app = self.app();
            let s = app.session.borrow();
            s.document()
                .project
                .channels
                .get(row)
                .map(|c| c.name.clone())
                .unwrap_or_default()
        };
        let label = logic::cell_label(
            &name,
            step,
            self.cell_state(row, step),
            self.is_read_only(row),
        );
        self.update_property(&[
            gtk::accessible::Property::Label(&label),
            gtk::accessible::Property::ValueText(&label),
        ]);
    }

    /// Redraws when the playhead moves.
    fn tick(&self) {
        let t = self.app().playhead_tick();
        let imp = self.imp();
        if imp.last_playhead.get() != t {
            imp.last_playhead.set(t);
            if self.app().ui.borrow().playing {
                self.queue_draw();
            }
        }
    }

    fn static_key(&self) -> StaticKey {
        let app = self.app();
        let s = app.session.borrow();
        StaticKey {
            revision: s.document().revision,
            pattern: app.current_pattern(),
            selected: app.current_channel(),
            palette_gen: palette::generation(),
            width: self.width(),
            height: self.height(),
            touch: self.imp().touch.get(),
        }
    }

    fn draw(&self, s: &gtk::Snapshot) {
        let _frame = perf::frame("step-grid");
        let imp = self.imp();
        let key = self.static_key();
        {
            let mut cache = imp.cache.borrow_mut();
            cache.append(s, key, |rec| self.draw_static(rec));
        }
        self.draw_dynamic(s);
    }

    /// The layer that only changes with data, selection, size, and style.
    fn draw_static(&self, s: &gtk::Snapshot) {
        let pal = Palette::current();
        let colors = palette::colors();
        let layout = self.imp().layout.get();
        let (w, h) = (self.width() as f64, self.height() as f64);
        draw::fill(s, &pal.bg, 0.0, 0.0, w, h);
        let app = self.app();
        let sess = app.session.borrow();
        let proj = &sess.document().project;
        let Some(pat) = app.current_pattern().and_then(|id| proj.pattern(id)) else {
            return;
        };
        let steps = pat.length_steps as u32;
        let selected = app.current_channel();
        let mut text = self.imp().text.borrow_mut();
        let ch_h = layout.cell_h();
        let pad_y = (layout.row_h - ch_h) / 2.0;
        let hc = colors.high_contrast;
        let off_edge = mix(&pal.fg, &pal.bg, if hc { 0.6 } else { 0.22 });

        // Ruler: bar numbers at the first step of each bar, beat ticks.
        let bar_steps = self.steps_per_bar();
        for st in 0..steps {
            let x = layout.cell_x(st);
            if st % bar_steps == 0 {
                let l = text.get(self, &format!("{}", st / bar_steps + 1), true);
                draw::layout_at(s, &l, &pal.text, x, 3.0);
            } else if st % layout.group == 0 {
                draw::fill(s, &pal.line_beat, x, layout.header_h - 7.0, 1.0, 5.0);
            }
        }
        draw::hline(s, &pal.line_beat, layout.header_h - 1.0, 0.0, w);

        // Beat groups: every second group is a little darker.
        let rows_h = layout.row_y(proj.channels.len()) - layout.header_h;
        for g in (0..steps.div_ceil(layout.group)).filter(|g| g % 2 == 1) {
            let x0 = layout.cell_x(g * layout.group) - layout.cell_gap;
            let last = ((g + 1) * layout.group).min(steps) - 1;
            let x1 = layout.cell_x(last) + layout.cell_w + layout.cell_gap;
            let mut c = pal.fg;
            c.set_alpha(0.045);
            draw::fill(s, &c, x0, layout.header_h, x1 - x0, rows_h);
        }

        for (row, ch) in proj.channels.iter().enumerate() {
            let y = layout.row_y(row);
            let rv = logic::row_view(pat, ch);
            let muted = ch.mix.mute;
            if selected == Some(ch.id) {
                let mut c = pal.accent;
                c.set_alpha(0.14);
                draw::fill(s, &c, 0.0, y, w, layout.row_h);
            }
            let body = colors.channel_color(ch.id.0);
            if row > 0 {
                draw::hline(s, &pal.line_sub, y, 0.0, w);
            }
            for st in 0..steps {
                let x = layout.cell_x(st);
                let cy = y + pad_y;
                let on = rv.cells.get(st as usize).copied();
                match on {
                    Some(StepCell::On { vel }) => {
                        // Louder steps are more opaque; the bar under the
                        // cell carries the same level for anyone who cannot
                        // tell opacity apart.
                        let t = 0.45 + 0.55 * (vel as f32 / 127.0);
                        let mut c = mix(&body, &pal.bg, t);
                        if muted {
                            c = mix(&c, &pal.bg, 0.4);
                        }
                        draw::rounded(s, &c, x, cy, layout.cell_w, ch_h, 4.0);
                        if layout.cell_w >= 20.0 {
                            let bw = (layout.cell_w - 8.0) * (vel as f64 / 127.0);
                            let mut bc = colors.get(Role::ViewBg);
                            bc.set_alpha(0.55);
                            draw::fill(s, &bc, x + 4.0, cy + ch_h - 5.0, bw, 2.0);
                        }
                    }
                    _ => {
                        draw::rounded(s, &off_edge, x, cy, layout.cell_w, ch_h, 4.0);
                        draw::rounded(
                            s,
                            &pal.bg,
                            x + 1.0,
                            cy + 1.0,
                            layout.cell_w - 2.0,
                            ch_h - 2.0,
                            3.0,
                        );
                    }
                }
                if rv.piano_roll_data {
                    // Diagonal hatch marks the row as read-only here.
                    let mut hx = x - ch_h;
                    s.push_clip(&draw::rect(x, cy, layout.cell_w, ch_h));
                    while hx < x + layout.cell_w {
                        s.save();
                        s.translate(&graphene::Point::new(hx as f32, (cy + ch_h) as f32));
                        s.rotate(-45.0);
                        s.append_color(&pal.hatch, &draw::rect(0.0, 0.0, 1.5, ch_h * 1.5));
                        s.restore();
                        hx += 7.0;
                    }
                    s.pop();
                }
            }
        }
    }

    /// Playhead, hover, and keyboard cursor: a few rectangles per frame.
    fn draw_dynamic(&self, s: &gtk::Snapshot) {
        let imp = self.imp();
        let layout = imp.layout.get();
        let app = self.app();
        let pal = Palette::current();
        let sess = app.session.borrow();
        let proj = &sess.document().project;
        let Some(pat) = app.current_pattern().and_then(|id| proj.pattern(id)) else {
            return;
        };
        let rows = proj.channels.len();
        let ch_h = layout.cell_h();
        let pad_y = (layout.row_h - ch_h) / 2.0;

        if let Some((row, step)) = imp.hover.get()
            && row < rows
        {
            let mut c = pal.fg;
            c.set_alpha(0.12);
            draw::rounded(
                s,
                &c,
                layout.cell_x(step),
                layout.row_y(row) + pad_y,
                layout.cell_w,
                ch_h,
                4.0,
            );
        }
        if self.has_focus() && self.is_focus_visible_now() && rows > 0 {
            let (row, step) =
                logic::move_cursor(imp.cursor.get(), 0, 0, rows, pat.length_steps as u32);
            let (x, y) = (layout.cell_x(step) - 1.0, layout.row_y(row) + pad_y - 1.0);
            let colors = palette::colors();
            let ring = if colors.high_contrast {
                colors.get(Role::WindowFg)
            } else {
                colors.get(Role::Accent)
            };
            draw::outline(s, &ring, x, y, layout.cell_w + 2.0, ch_h + 2.0, 2.0);
            if colors.high_contrast {
                draw::outline(
                    s,
                    &colors.get(Role::WindowBg),
                    x + 2.0,
                    y + 2.0,
                    layout.cell_w - 2.0,
                    ch_h - 2.0,
                    1.0,
                );
            }
        }
        if app.ui.borrow().playing {
            let tick = imp.last_playhead.get();
            let len = pat.length_ticks() as u64;
            if len > 0 && pat.step_ticks > 0 {
                let t = (tick % len) as f64 / pat.step_ticks as f64;
                let x = layout.step_pos_x(t);
                let colors = palette::colors();
                draw::fill(
                    s,
                    &colors.get(Role::Accent),
                    x,
                    layout.header_h,
                    2.0,
                    layout.row_y(rows) - layout.header_h,
                );
                // A small triangle in the ruler: a 5 px stem and a cap.
                draw::fill(
                    s,
                    &colors.get(Role::Accent),
                    x - 3.0,
                    layout.header_h - 6.0,
                    8.0,
                    3.0,
                );
                draw::fill(
                    s,
                    &colors.get(Role::Accent),
                    x - 1.0,
                    layout.header_h - 3.0,
                    4.0,
                    2.0,
                );
            }
        }
    }

    /// `focus-visible` of this widget (keyboard focus, not mouse focus).
    fn is_focus_visible_now(&self) -> bool {
        self.root()
            .and_then(|r| r.downcast::<gtk::Window>().ok())
            .map(|w| w.property::<bool>("focus-visible"))
            .unwrap_or(true)
    }
}
