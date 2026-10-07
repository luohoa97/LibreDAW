// SPDX-License-Identifier: GPL-3.0-or-later
//! The step grid (SPEC 5.2, 11): one row per channel, one cell per step,
//! drawn with `snapshot()`. Mouse: click or drag to paint steps on or off
//! (one undo step per stroke), click a name to select the channel. Keyboard:
//! arrows move the cursor, Space or Enter toggles, Home and End jump.
//! Accessible role `grid`; the label of the widget names the cursor cell.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, graphene};

use protocol::edit::Edit;
use protocol::ids::{ChannelId, PatternId};

use crate::app::App;
use crate::draw::{self, Palette};
use crate::step_logic::{self as logic, Cell as StepCell, Hit, StepLayout};

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct StepGrid {
        pub app: RefCell<Option<Rc<App>>>,
        pub layout: Cell<StepLayout>,
        pub cursor: Cell<(usize, u32)>,
        /// Open paint stroke: the state being painted and the last step
        /// painted per row.
        pub stroke: RefCell<Option<Stroke>>,
        pub drag_start: Cell<(f64, f64)>,
        pub last_playhead: Cell<u64>,
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
            obj.set_hexpand(true);
            obj.update_property(&[gtk::accessible::Property::Label("Step sequencer")]);

            let drag = gtk::GestureDrag::new();
            drag.set_button(gdk::BUTTON_PRIMARY);
            let w = obj.downgrade();
            drag.connect_drag_begin(move |_, x, y| {
                if let Some(o) = w.upgrade() {
                    o.press(x, y);
                }
            });
            let w = obj.downgrade();
            drag.connect_drag_update(move |g, dx, dy| {
                if let Some(o) = w.upgrade() {
                    let (sx, sy) = o.imp().drag_start.get();
                    o.paint_to(sx + dx, sy + dy);
                    let _ = g;
                }
            });
            let w = obj.downgrade();
            drag.connect_drag_end(move |_, _, _| {
                if let Some(o) = w.upgrade() {
                    o.release();
                }
            });
            obj.add_controller(drag);

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
        *o.imp().app.borrow_mut() = Some(app);
        o
    }

    fn app(&self) -> Rc<App> {
        self.imp().app.borrow().clone().expect("app set")
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

    fn press(&self, x: f64, y: f64) {
        self.grab_focus();
        self.imp().drag_start.set((x, y));
        let (rows, steps) = self.dims();
        let layout = self.imp().layout.get();
        match layout.hit(x, y, rows, steps) {
            Hit::Name(row) => {
                if let Some(c) = self.channel_at(row) {
                    self.imp().cursor.set((row, self.imp().cursor.get().1));
                    self.app().select_channel(c);
                    self.preview_row(row);
                }
            }
            Hit::Cell { row, step } => {
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
            Hit::None => {}
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
        let (dr, ds) = match key {
            gdk::Key::Left => (0, -1),
            gdk::Key::Right => (0, 1),
            gdk::Key::Up => (-1, 0),
            gdk::Key::Down => (1, 0),
            gdk::Key::Home => (0, -(steps as i32)),
            gdk::Key::End => (0, steps as i32),
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

    /// Updates the accessible label with the cursor cell (SPEC 11).
    fn update_label(&self) {
        let (rows, steps) = self.dims();
        if rows == 0 || steps == 0 {
            self.update_property(&[gtk::accessible::Property::Label(
                "Step sequencer, no channels",
            )]);
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
        self.update_property(&[gtk::accessible::Property::Label(&label)]);
    }

    /// Redraws when the playhead moves.
    fn tick(&self) {
        let t = self.app().playhead_tick();
        let imp = self.imp();
        if imp.last_playhead.get() != t {
            imp.last_playhead.set(t);
            self.queue_draw();
        }
    }

    fn draw(&self, s: &gtk::Snapshot) {
        let pal = Palette::of(self);
        let layout = self.imp().layout.get();
        let (w, h) = (self.width() as f64, self.height() as f64);
        draw::fill(s, &pal.bg, 0.0, 0.0, w, h);
        let app = self.app();
        let sess = app.session.borrow();
        let proj = &sess.document().project;
        let pat = app.current_pattern().and_then(|id| proj.pattern(id));
        let Some(pat) = pat else {
            draw::text(
                self,
                s,
                &pal.text_dim,
                12.0,
                8.0,
                "No pattern. Add one to start.",
                false,
            );
            return;
        };
        let steps = pat.length_steps as u32;
        let selected = app.current_channel();
        let cursor = self.imp().cursor.get();
        let focused = self.has_focus();

        // Header: step numbers on the first step of each group.
        for st in 0..steps {
            if st % layout.group == 0 {
                draw::text(
                    self,
                    s,
                    &pal.text_dim,
                    layout.cell_x(st),
                    2.0,
                    &format!("{}", st + 1),
                    false,
                );
            }
        }

        for (row, ch) in proj.channels.iter().enumerate() {
            let y = layout.row_y(row);
            let rv = logic::row_view(pat, ch);
            let is_sel = selected == Some(ch.id);
            let band = if row % 2 == 0 {
                pal.row_even
            } else {
                pal.row_odd
            };
            draw::fill(s, &band, 0.0, y, w, layout.row_h);
            if is_sel {
                draw::fill(
                    s,
                    &draw::mix(&pal.accent, &pal.bg, 0.18),
                    0.0,
                    y,
                    w,
                    layout.row_h,
                );
                draw::fill(s, &pal.accent, 0.0, y, 3.0, layout.row_h);
            }
            draw::text_in(
                self,
                s,
                &pal.text,
                12.0,
                y,
                layout.name_w - 16.0,
                layout.row_h,
                &ch.name,
            );
            for st in 0..steps {
                let x = layout.cell_x(st);
                let cy = y + 3.0;
                let ch_h = layout.row_h - 6.0;
                let base = if (st / layout.group).is_multiple_of(2) {
                    pal.cell_off
                } else {
                    pal.cell_off_alt
                };
                draw::rounded(s, &base, x, cy, layout.cell_w, ch_h, 4.0);
                if let Some(StepCell::On { vel }) = rv.cells.get(st as usize).copied() {
                    let t = 0.45 + 0.55 * (vel as f32 / 127.0);
                    draw::rounded(
                        s,
                        &draw::mix(&pal.cell_on, &base, t),
                        x,
                        cy,
                        layout.cell_w,
                        ch_h,
                        4.0,
                    );
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
                if focused && cursor == (row, st) {
                    for (bx, by, bw, bh) in [
                        (x, cy, layout.cell_w, 2.0),
                        (x, cy + ch_h - 2.0, layout.cell_w, 2.0),
                        (x, cy, 2.0, ch_h),
                        (x + layout.cell_w - 2.0, cy, 2.0, ch_h),
                    ] {
                        draw::fill(s, &pal.cursor, bx, by, bw, bh);
                    }
                }
            }
            if rv.piano_roll_data {
                let label_x = layout.cell_x(steps.saturating_sub(1)) + layout.cell_w + 6.0;
                let _ = label_x;
                draw::text(self, s, &pal.note, layout.name_w - 14.0, y + 7.0, "~", true);
            }
        }

        // Playhead inside the pattern.
        if self.app().ui.borrow().playing {
            let tick = self.imp().last_playhead.get();
            let len = pat.length_ticks() as u64;
            if len > 0 && pat.step_ticks > 0 {
                let t = (tick % len) as f64 / pat.step_ticks as f64;
                let x = layout.step_pos_x(t);
                draw::fill(
                    s,
                    &pal.playhead,
                    x,
                    layout.header_h,
                    2.0,
                    layout.row_y(proj.channels.len()) - layout.header_h,
                );
            }
        }
        if proj.channels.is_empty() {
            draw::text(
                self,
                s,
                &pal.text_dim,
                12.0,
                layout.header_h + 8.0,
                "No channels. Use Add channel.",
                false,
            );
        }
    }
}
