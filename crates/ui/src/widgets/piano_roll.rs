// SPDX-License-Identifier: GPL-3.0-or-later
//! The piano roll (SPEC 11, 13.1 item 4): a keyboard column with C labels, a
//! bar ruler, the note grid with bar, beat and snap lines, note names on
//! notes, a velocity lane with draggable stems, snap, and zoom and scroll on
//! both axes. Drawn with `snapshot()`; the logic is in `roll_logic` and
//! `view_math`.
//!
//! Mouse: press on empty space adds a note (drag to set its length);
//! Shift-drag on empty space selects with a box; press on a note selects and
//! drags it; the right edge resizes; right click deletes; velocity stems are
//! dragged in the lane under the grid. Wheel scrolls, Shift-wheel scrolls
//! sideways, Ctrl-wheel zooms time, Ctrl-Shift-wheel zooms pitch.
//!
//! Keyboard: arrows move the cursor cell, Space adds or removes a note,
//! Shift-arrows resize the selection, Ctrl-arrows move it, Delete removes
//! it, Ctrl-A selects all, Escape clears the selection.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, graphene};

use protocol::edit::{Edit, NewNote};
use protocol::engine::EngineCommand;
use protocol::ids::{ChannelId, NoteId, PatternId};
use protocol::model::Note;

use crate::app::App;
use crate::document::DEFAULT_STEP_VEL;
use crate::draw::{self, Palette};
use crate::roll_logic::{
    self as logic, Drag, DragKind, Part, click_selection, hit_note, hit_stem, note_at_cursor,
    note_rect, notes_in_box, prune_selection, vel_to_y, with_pattern,
};
use crate::view_math::{self as vm, SNAPS, Viewport, note_name};

/// Corner points of a box selection.
type Marquee = ((f64, f64), (f64, f64));

mod imp {
    use super::*;

    pub struct PianoRoll {
        pub app: RefCell<Option<Rc<App>>>,
        pub vp: Cell<Viewport>,
        pub selection: RefCell<Vec<NoteId>>,
        pub cursor: Cell<(u32, u8)>,
        pub drag: RefCell<Option<Drag>>,
        pub marquee: Cell<Option<Marquee>>,
        pub snap_idx: Cell<usize>,
        pub hadj: gtk::Adjustment,
        pub vadj: gtk::Adjustment,
        pub updating_adj: Cell<bool>,
        pub drag_start: Cell<(f64, f64)>,
        pub pointer: Cell<(f64, f64)>,
        pub last_playhead: Cell<u64>,
        pub in_gesture: Cell<bool>,
        /// Key held down on the keyboard column (sounds while held).
        pub held_key: Cell<Option<u8>>,
        /// Last pitch previewed during a note drag.
        pub last_preview: Cell<Option<u8>>,
    }

    impl Default for PianoRoll {
        fn default() -> PianoRoll {
            PianoRoll {
                app: RefCell::new(None),
                vp: Cell::new(Viewport::default()),
                selection: RefCell::new(Vec::new()),
                cursor: Cell::new((0, 60)),
                drag: RefCell::new(None),
                marquee: Cell::new(None),
                snap_idx: Cell::new(0),
                hadj: gtk::Adjustment::new(0.0, 0.0, 1.0, 10.0, 100.0, 1.0),
                vadj: gtk::Adjustment::new(0.0, 0.0, 1.0, 10.0, 100.0, 1.0),
                updating_adj: Cell::new(false),
                drag_start: Cell::new((0.0, 0.0)),
                pointer: Cell::new((100.0, 100.0)),
                last_playhead: Cell::new(0),
                in_gesture: Cell::new(false),
                held_key: Cell::new(None),
                last_preview: Cell::new(None),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PianoRoll {
        const NAME: &'static str = "LibreDawPianoRoll";
        type Type = super::PianoRoll;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_accessible_role(gtk::AccessibleRole::Generic);
            klass.set_css_name("pianoroll");
        }
    }

    impl ObjectImpl for PianoRoll {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            obj.set_focusable(true);
            obj.set_can_focus(true);
            obj.set_hexpand(true);
            obj.set_vexpand(true);
            obj.update_property(&[gtk::accessible::Property::Label("Piano roll")]);

            let drag = gtk::GestureDrag::new();
            drag.set_button(gdk::BUTTON_PRIMARY);
            let w = obj.downgrade();
            drag.connect_drag_begin(move |g, x, y| {
                if let Some(o) = w.upgrade() {
                    let st = g.current_event_state();
                    o.press(x, y, st);
                }
            });
            let w = obj.downgrade();
            drag.connect_drag_update(move |_, dx, dy| {
                if let Some(o) = w.upgrade() {
                    let (sx, sy) = o.imp().drag_start.get();
                    o.drag_to(sx + dx, sy + dy);
                }
            });
            let w = obj.downgrade();
            drag.connect_drag_end(move |_, _, _| {
                if let Some(o) = w.upgrade() {
                    o.release();
                }
            });
            obj.add_controller(drag);

            let click = gtk::GestureClick::new();
            click.set_button(gdk::BUTTON_SECONDARY);
            let w = obj.downgrade();
            click.connect_pressed(move |_, _, x, y| {
                if let Some(o) = w.upgrade() {
                    o.right_click(x, y);
                }
            });
            obj.add_controller(click);

            let motion = gtk::EventControllerMotion::new();
            let w = obj.downgrade();
            motion.connect_motion(move |_, x, y| {
                if let Some(o) = w.upgrade() {
                    o.imp().pointer.set((x, y));
                }
            });
            obj.add_controller(motion);

            let scroll = gtk::EventControllerScroll::new(
                gtk::EventControllerScrollFlags::BOTH_AXES
                    | gtk::EventControllerScrollFlags::DISCRETE,
            );
            let w = obj.downgrade();
            scroll.connect_scroll(move |c, dx, dy| match w.upgrade() {
                Some(o) => {
                    o.scroll(dx, dy, c.current_event_state());
                    glib::Propagation::Stop
                }
                None => glib::Propagation::Proceed,
            });
            obj.add_controller(scroll);

            let keys = gtk::EventControllerKey::new();
            let w = obj.downgrade();
            keys.connect_key_pressed(move |_, key, _, st| match w.upgrade() {
                Some(o) if o.key(key, st) => glib::Propagation::Stop,
                _ => glib::Propagation::Proceed,
            });
            obj.add_controller(keys);

            let w = obj.downgrade();
            self.hadj.connect_value_changed(move |a| {
                if let Some(o) = w.upgrade()
                    && !o.imp().updating_adj.get()
                {
                    let mut vp = o.imp().vp.get();
                    vp.scroll_x = a.value();
                    o.imp().vp.set(vp);
                    o.queue_draw();
                }
            });
            let w = obj.downgrade();
            self.vadj.connect_value_changed(move |a| {
                if let Some(o) = w.upgrade()
                    && !o.imp().updating_adj.get()
                {
                    let mut vp = o.imp().vp.get();
                    vp.scroll_y = a.value();
                    o.imp().vp.set(vp);
                    o.queue_draw();
                }
            });

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

    impl WidgetImpl for PianoRoll {
        fn measure(&self, o: gtk::Orientation, _: i32) -> (i32, i32, i32, i32) {
            let (min, nat) = if o == gtk::Orientation::Horizontal {
                (240, 600)
            } else {
                (200, 360)
            };
            (min, nat, -1, -1)
        }

        fn size_allocate(&self, w: i32, h: i32, _baseline: i32) {
            let mut vp = self.vp.get();
            vp.width = w as f64;
            vp.height = h as f64;
            self.vp.set(vp);
            self.obj().sync_adjustments();
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            self.obj().draw(snapshot);
        }
    }
}

glib::wrapper! {
    pub struct PianoRoll(ObjectSubclass<imp::PianoRoll>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

/// Everything the roll needs about the pattern and channel it shows.
struct View {
    pattern: PatternId,
    channel: ChannelId,
    notes: Vec<Note>,
    len_ticks: u32,
    step_ticks: u32,
    bar_ticks: u32,
}

impl PianoRoll {
    pub fn new(app: Rc<App>) -> PianoRoll {
        let o: PianoRoll = glib::Object::new();
        let w = o.downgrade();
        app.on_change(move || {
            if let Some(o) = w.upgrade() {
                o.refresh();
            }
        });
        *o.imp().app.borrow_mut() = Some(app);
        o.refresh();
        o
    }

    pub fn hadj(&self) -> gtk::Adjustment {
        self.imp().hadj.clone()
    }

    pub fn vadj(&self) -> gtk::Adjustment {
        self.imp().vadj.clone()
    }

    pub fn snap_index(&self) -> usize {
        self.imp().snap_idx.get()
    }

    pub fn set_snap_index(&self, i: usize) {
        self.imp().snap_idx.set(i.min(SNAPS.len() - 1));
        self.queue_draw();
    }

    /// `(px per tick, row height, scroll x, scroll y, snap index)` for the
    /// project's `.view.toml`.
    pub fn view_params(&self) -> (f64, f64, f64, f64, usize) {
        let vp = self.imp().vp.get();
        (
            vp.px_per_tick,
            vp.row_h,
            vp.scroll_x,
            vp.scroll_y,
            self.snap_index(),
        )
    }

    pub fn set_view_params(&self, px_per_tick: f64, row_h: f64, sx: f64, sy: f64, snap: usize) {
        let len = self.view().map(|v| v.len_ticks).unwrap_or(u32::MAX / 2);
        let mut vp = self.imp().vp.get();
        vp.px_per_tick = px_per_tick.clamp(vm::MIN_PX_PER_TICK, vm::MAX_PX_PER_TICK);
        vp.row_h = row_h.clamp(vm::MIN_ROW_H, vm::MAX_ROW_H);
        vp.scroll_x = sx;
        vp.scroll_y = sy;
        vp.clamp_scroll(len);
        self.imp().vp.set(vp);
        self.imp().snap_idx.set(snap.min(SNAPS.len() - 1));
        self.sync_adjustments();
        self.queue_draw();
    }

    pub fn zoom_x(&self, factor: f64) {
        let len = self.view().map(|v| v.len_ticks).unwrap_or(0);
        let mut vp = self.imp().vp.get();
        let anchor = vp.key_w + vp.grid_width() / 2.0;
        vp.zoom_x(factor, anchor, len);
        self.imp().vp.set(vp);
        self.sync_adjustments();
        self.queue_draw();
    }

    pub fn zoom_y(&self, factor: f64) {
        let len = self.view().map(|v| v.len_ticks).unwrap_or(0);
        let mut vp = self.imp().vp.get();
        let anchor = vp.grid_top() + vp.grid_height() / 2.0;
        vp.zoom_y(factor, anchor, len);
        self.imp().vp.set(vp);
        self.sync_adjustments();
        self.queue_draw();
    }

    fn app(&self) -> Rc<App> {
        self.imp().app.borrow().clone().expect("app set")
    }

    fn view(&self) -> Option<View> {
        let app = self.app();
        let s = app.session.borrow();
        let p = &s.document().project;
        let pid = app.current_pattern()?;
        let cid = app.current_channel()?;
        let pat = p.pattern(pid)?;
        p.channel(cid)?;
        Some(View {
            pattern: pid,
            channel: cid,
            notes: pat.notes_of(cid).to_vec(),
            len_ticks: pat.length_ticks(),
            step_ticks: pat.step_ticks,
            bar_ticks: protocol::model::ticks_per_bar(p.time_sig_num),
        })
    }

    fn snap_ticks(&self, v: &View) -> u32 {
        SNAPS[self.imp().snap_idx.get()]
            .1
            .unwrap_or(v.step_ticks)
            .max(1)
    }

    fn refresh(&self) {
        if let Some(v) = self.view() {
            prune_selection(&mut self.imp().selection.borrow_mut(), &v.notes);
        } else {
            self.imp().selection.borrow_mut().clear();
        }
        self.sync_adjustments();
        self.update_label();
        self.queue_draw();
    }

    /// Matches the scrollbars' adjustments to the content and view.
    fn sync_adjustments(&self) {
        let imp = self.imp();
        let len = self.view().map(|v| v.len_ticks).unwrap_or(0);
        let mut vp = imp.vp.get();
        vp.clamp_scroll(len);
        imp.vp.set(vp);
        imp.updating_adj.set(true);
        let cw = vp.content_width(len).max(vp.grid_width());
        imp.hadj.configure(
            vp.scroll_x,
            0.0,
            cw,
            vp.grid_width() / 20.0,
            vp.grid_width() * 0.9,
            vp.grid_width(),
        );
        let ch = vp.content_height();
        imp.vadj.configure(
            vp.scroll_y,
            0.0,
            ch,
            vp.row_h * 3.0,
            vp.grid_height() * 0.9,
            vp.grid_height(),
        );
        imp.updating_adj.set(false);
    }

    fn update_label(&self) {
        let Some(v) = self.view() else {
            self.update_property(&[gtk::accessible::Property::Label(
                "Piano roll, no channel or pattern selected",
            )]);
            return;
        };
        let (tick, key) = self.imp().cursor.get();
        let bar = tick / v.bar_ticks.max(1) + 1;
        let beat = (tick % v.bar_ticks.max(1)) / protocol::consts::PPQ + 1;
        let has = note_at_cursor(&v.notes, tick, key).is_some();
        let label = format!(
            "Piano roll, cursor bar {bar} beat {beat}, {}, {}, {} notes selected",
            note_name(key),
            if has { "note present" } else { "empty" },
            self.imp().selection.borrow().len()
        );
        self.update_property(&[gtk::accessible::Property::Label(&label)]);
    }

    fn tick(&self) {
        let t = self.app().playhead_tick();
        if self.imp().last_playhead.get() != t {
            self.imp().last_playhead.set(t);
            self.queue_draw();
        }
    }

    // ---- input ----

    fn press(&self, x: f64, y: f64, state: gdk::ModifierType) {
        self.grab_focus();
        let imp = self.imp();
        imp.drag_start.set((x, y));
        let Some(v) = self.view() else { return };
        let vp = imp.vp.get();
        let shift = state.contains(gdk::ModifierType::SHIFT_MASK);
        let app = self.app();

        // Ruler: move the playhead.
        if y < vp.grid_top() && x >= vp.key_w {
            let tick = vm::snap_floor(vp.x_to_tick(x), self.snap_ticks(&v)) as u64;
            let _ = app
                .session
                .borrow_mut()
                .link
                .command(EngineCommand::Seek { tick });
            return;
        }

        // Keyboard column: the key sounds while the button is held.
        if x < vp.key_w && y >= vp.grid_top() && y < vp.grid_bottom() {
            let key = vp.y_to_key(y);
            if (0..=127).contains(&key) {
                imp.held_key.set(Some(key as u8));
                imp.cursor.set((imp.cursor.get().0, key as u8));
                app.preview_on(v.channel, key as u8, DEFAULT_STEP_VEL);
            }
            self.after_input();
            return;
        }

        if vp.in_vel_lane(x, y) {
            if let Some(id) = hit_stem(&v.notes, &vp, x, y) {
                let sel = {
                    let cur = imp.selection.borrow().clone();
                    if cur.contains(&id) { cur } else { vec![id] }
                };
                *imp.selection.borrow_mut() = sel.clone();
                if app.gesture_begin("Velocity") {
                    imp.in_gesture.set(true);
                    let mut d = Drag::new(
                        DragKind::Velocity,
                        sel,
                        &v.notes,
                        vp.x_to_tick(x),
                        vp.y_to_key(y),
                        v.len_ticks,
                    );
                    self.apply_drag(&mut d, x, y, &v);
                    *imp.drag.borrow_mut() = Some(d);
                }
            }
            self.after_input();
            return;
        }

        if !vp.in_grid(x, y) {
            return;
        }

        match hit_note(&v.notes, &vp, x, y) {
            Some((id, part)) => {
                let sel = click_selection(&imp.selection.borrow(), id, shift);
                *imp.selection.borrow_mut() = sel.clone();
                if shift {
                    self.after_input();
                    return;
                }
                let kind = if part == Part::RightEdge {
                    DragKind::Resize
                } else {
                    DragKind::Move
                };
                if app.gesture_begin(if part == Part::RightEdge {
                    "Resize notes"
                } else {
                    "Move notes"
                }) {
                    imp.in_gesture.set(true);
                    *imp.drag.borrow_mut() = Some(Drag::new(
                        kind,
                        sel,
                        &v.notes,
                        vp.x_to_tick(x),
                        vp.y_to_key(y),
                        v.len_ticks,
                    ));
                }
                if let Some(n) = v.notes.iter().find(|n| n.id == id) {
                    imp.cursor.set((n.start, n.key));
                    imp.last_preview.set(Some(n.key));
                    app.preview_pulse(v.channel, n.key, n.vel, 300);
                }
            }
            None if shift => {
                imp.marquee.set(Some(((x, y), (x, y))));
            }
            None => {
                let snap = self.snap_ticks(&v);
                let tick = vm::snap_floor(vp.x_to_tick(x), snap);
                let key = vp.y_to_key(y);
                if tick >= v.len_ticks || !(0..=127).contains(&key) {
                    imp.selection.borrow_mut().clear();
                    self.after_input();
                    return;
                }
                imp.cursor.set((tick, key as u8));
                if app.gesture_begin("Add note") {
                    imp.in_gesture.set(true);
                    let r = app.gesture_edit(vec![Edit::AddNotes {
                        pattern: v.pattern,
                        channel: v.channel,
                        notes: vec![NewNote {
                            start: tick,
                            len: snap,
                            key: key as u8,
                            vel: DEFAULT_STEP_VEL,
                        }],
                    }]);
                    if let Some(a) = r {
                        let id = NoteId(a.created[0]);
                        *imp.selection.borrow_mut() = vec![id];
                        imp.last_preview.set(Some(key as u8));
                        app.preview_pulse(
                            v.channel,
                            key as u8,
                            DEFAULT_STEP_VEL,
                            self.hold_ms(snap),
                        );
                        let made = Note {
                            id,
                            start: tick,
                            len: snap,
                            key: key as u8,
                            vel: DEFAULT_STEP_VEL,
                        };
                        *imp.drag.borrow_mut() = Some(Drag::new(
                            DragKind::Resize,
                            vec![id],
                            &[made],
                            vp.x_to_tick(x),
                            key,
                            v.len_ticks,
                        ));
                    }
                }
            }
        }
        self.after_input();
    }

    fn apply_drag(&self, d: &mut Drag, x: f64, y: f64, v: &View) {
        let vp = self.imp().vp.get();
        let snap = self.snap_ticks(v);
        if let Some(e) = d.update(&vp, x, y, snap) {
            self.app().gesture_edit(vec![with_pattern(e, v.pattern)]);
        }
    }

    /// How long a preview of a note of `ticks` sounds: its length, at most
    /// 500 ms and at least 80 ms.
    fn hold_ms(&self, ticks: u32) -> u64 {
        let bpm = self.app().session.borrow().document().project.tempo_bpm;
        let ms = ticks as f64 / protocol::consts::PPQ as f64 * 60_000.0 / bpm.max(1.0);
        (ms as u64).clamp(80, 500)
    }

    fn drag_to(&self, x: f64, y: f64) {
        let imp = self.imp();
        if let Some(held) = imp.held_key.get() {
            // Sliding over the keyboard plays each key it reaches.
            let vp = imp.vp.get();
            let key = vp.y_to_key(y).clamp(0, 127) as u8;
            if key != held
                && let Some(v) = self.view()
            {
                imp.held_key.set(Some(key));
                imp.cursor.set((imp.cursor.get().0, key));
                self.app().preview_on(v.channel, key, DEFAULT_STEP_VEL);
                self.after_input();
            }
            return;
        }
        if let Some(((x0, y0), _)) = imp.marquee.get() {
            imp.marquee.set(Some(((x0, y0), (x, y))));
            if let Some(v) = self.view() {
                let vp = imp.vp.get();
                *imp.selection.borrow_mut() = notes_in_box(&v.notes, &vp, (x0, y0), (x, y));
            }
            self.after_input();
            return;
        }
        let Some(v) = self.view() else { return };
        let taken = imp.drag.borrow_mut().take();
        if let Some(mut d) = taken {
            self.apply_drag(&mut d, x, y, &v);
            if matches!(d.kind, DragKind::Move)
                && let Some(n) = self
                    .view()
                    .and_then(|v| v.notes.into_iter().find(|n| n.id == d.ids[0]))
            {
                imp.cursor.set((n.start, n.key));
                // Each new pitch of a dragged note sounds once.
                if imp.last_preview.get() != Some(n.key) {
                    imp.last_preview.set(Some(n.key));
                    self.app().preview_pulse(v.channel, n.key, n.vel, 300);
                }
            }
            *imp.drag.borrow_mut() = Some(d);
        }
        self.after_input();
    }

    fn release(&self) {
        let imp = self.imp();
        imp.marquee.set(None);
        imp.drag.borrow_mut().take();
        // Letting go ends a held key. Pulses (new notes, dragged notes)
        // end on their own timer, so a quick click is still heard.
        if imp.held_key.take().is_some() {
            self.app().preview_off();
        }
        imp.last_preview.set(None);
        if imp.in_gesture.replace(false) {
            self.app().gesture_end();
        }
        self.after_input();
    }

    fn right_click(&self, x: f64, y: f64) {
        self.grab_focus();
        let Some(v) = self.view() else { return };
        let vp = self.imp().vp.get();
        if let Some((id, _)) = hit_note(&v.notes, &vp, x, y) {
            let sel = self.imp().selection.borrow().clone();
            let ids = if sel.contains(&id) { sel } else { vec![id] };
            self.app().edit(vec![Edit::RemoveNotes {
                pattern: v.pattern,
                notes: ids,
            }]);
        }
    }

    fn scroll(&self, dx: f64, dy: f64, st: gdk::ModifierType) {
        let imp = self.imp();
        let len = self.view().map(|v| v.len_ticks).unwrap_or(0);
        let mut vp = imp.vp.get();
        let (px, py) = imp.pointer.get();
        let ctrl = st.contains(gdk::ModifierType::CONTROL_MASK);
        let shift = st.contains(gdk::ModifierType::SHIFT_MASK);
        let f = 1.15f64.powf(-dy);
        if ctrl && shift {
            vp.zoom_y(f, py, len);
        } else if ctrl {
            vp.zoom_x(f, px, len);
        } else if shift {
            vp.scroll_x += (dy + dx) * 60.0;
        } else {
            vp.scroll_y += dy * vp.row_h * 3.0;
            vp.scroll_x += dx * 60.0;
        }
        vp.clamp_scroll(len);
        imp.vp.set(vp);
        self.sync_adjustments();
        self.queue_draw();
    }

    fn key(&self, key: gdk::Key, st: gdk::ModifierType) -> bool {
        let Some(v) = self.view() else { return false };
        let imp = self.imp();
        let app = self.app();
        let snap = self.snap_ticks(&v) as i64;
        let ctrl = st.contains(gdk::ModifierType::CONTROL_MASK);
        let shift = st.contains(gdk::ModifierType::SHIFT_MASK);
        let sel = imp.selection.borrow().clone();
        let (dir_t, dir_k) = match key {
            gdk::Key::Left => (-1i64, 0i32),
            gdk::Key::Right => (1, 0),
            gdk::Key::Up => (0, 1),
            gdk::Key::Down => (0, -1),
            _ => (0, 0),
        };
        let arrow = dir_t != 0 || dir_k != 0;
        if arrow && ctrl && !sel.is_empty() {
            // Move the selection.
            app.edit_quiet(vec![Edit::MoveNotes {
                pattern: v.pattern,
                notes: sel,
                dt: dir_t * snap,
                dkey: dir_k as i16,
            }]);
            return true;
        }
        if arrow && shift && !sel.is_empty() {
            if dir_t != 0 {
                app.edit_quiet(vec![Edit::ResizeNotes {
                    pattern: v.pattern,
                    notes: sel,
                    dlen: dir_t * snap,
                }]);
            }
            return true;
        }
        if arrow {
            let (t, k) = imp.cursor.get();
            let nt = (t as i64 + dir_t * snap).clamp(0, v.len_ticks as i64 - 1) as u32;
            let nt = vm::snap_floor(nt as f64, snap as u32);
            let nk = (k as i32 + dir_k).clamp(0, 127) as u8;
            imp.cursor.set((nt, nk));
            let mut vp = imp.vp.get();
            vp.reveal_key(nk as i32, v.len_ticks);
            vp.reveal_tick(nt as f64, v.len_ticks);
            imp.vp.set(vp);
            self.sync_adjustments();
            self.update_label();
            self.queue_draw();
            return true;
        }
        match key {
            gdk::Key::space => {
                let (t, k) = imp.cursor.get();
                match note_at_cursor(&v.notes, t, k) {
                    Some(id) => {
                        app.edit_quiet(vec![Edit::RemoveNotes {
                            pattern: v.pattern,
                            notes: vec![id],
                        }]);
                    }
                    None => {
                        if let Some(a) = app.edit_quiet(vec![Edit::AddNotes {
                            pattern: v.pattern,
                            channel: v.channel,
                            notes: vec![NewNote {
                                start: t,
                                len: snap as u32,
                                key: k,
                                vel: DEFAULT_STEP_VEL,
                            }],
                        }]) {
                            *imp.selection.borrow_mut() = vec![NoteId(a.created[0])];
                            app.preview_pulse(
                                v.channel,
                                k,
                                DEFAULT_STEP_VEL,
                                self.hold_ms(snap as u32),
                            );
                        }
                    }
                }
                true
            }
            gdk::Key::Delete | gdk::Key::BackSpace => {
                if !sel.is_empty() {
                    app.edit_quiet(vec![Edit::RemoveNotes {
                        pattern: v.pattern,
                        notes: sel,
                    }]);
                }
                true
            }
            gdk::Key::a if ctrl => {
                *imp.selection.borrow_mut() = v.notes.iter().map(|n| n.id).collect();
                self.after_input();
                true
            }
            gdk::Key::Escape => {
                imp.selection.borrow_mut().clear();
                self.after_input();
                true
            }
            _ => false,
        }
    }

    fn after_input(&self) {
        self.update_label();
        self.queue_draw();
    }

    // ---- drawing ----

    fn draw(&self, s: &gtk::Snapshot) {
        let pal = Palette::of(self);
        let imp = self.imp();
        let vp = imp.vp.get();
        let (w, h) = (vp.width, vp.height);
        draw::fill(s, &pal.bg, 0.0, 0.0, w, h);
        let Some(v) = self.view() else {
            draw::text(
                self,
                s,
                &pal.text_dim,
                16.0,
                16.0,
                "Select a channel and a pattern to edit notes.",
                false,
            );
            return;
        };
        let snap = self.snap_ticks(&v);
        let sel = imp.selection.borrow().clone();
        let focused = self.has_focus();

        // -- grid area --
        s.push_clip(&draw::rect(
            vp.key_w,
            vp.grid_top(),
            vp.grid_width(),
            vp.grid_height(),
        ));
        let (lo, hi) = vp.visible_keys();
        for key in lo..=hi {
            let y = vp.key_to_y(key);
            let col = if vm::is_black_key(key as u8) {
                pal.row_black_key
            } else if key % 2 == 0 {
                pal.row_even
            } else {
                pal.row_odd
            };
            draw::fill(s, &col, vp.key_w, y, vp.grid_width(), vp.row_h);
            if key % 12 == 0 {
                draw::hline(s, &pal.line_bar, y + vp.row_h, vp.key_w, w);
            } else {
                draw::hline(s, &pal.line_sub, y + vp.row_h, vp.key_w, w);
            }
        }
        let (t0, t1) = vp.visible_ticks();
        for (t, level) in vm::grid_lines(t0, t1, vp.px_per_tick, v.bar_ticks, snap, 7.0) {
            let x = vp.tick_to_x(t as f64);
            let col = match level {
                2 => &pal.line_bar,
                1 => &pal.line_beat,
                _ => &pal.line_sub,
            };
            draw::vline(s, col, x, vp.grid_top(), vp.grid_bottom());
        }
        // Beyond the pattern end.
        let end_x = vp.tick_to_x(v.len_ticks as f64);
        if end_x < w {
            draw::fill(
                s,
                &pal.outside,
                end_x.max(vp.key_w),
                vp.grid_top(),
                w - end_x.max(vp.key_w),
                vp.grid_height(),
            );
            draw::vline(s, &pal.line_bar, end_x, vp.grid_top(), vp.grid_bottom());
        }

        // Notes.
        for n in &v.notes {
            let (x, y, nw, nh) = note_rect(&vp, n);
            if x + nw < vp.key_w || x > w || y + nh < vp.grid_top() || y > vp.grid_bottom() {
                continue;
            }
            let selected = sel.contains(&n.id);
            let t = 0.6 + 0.4 * (n.vel as f32 / 127.0);
            let body = if selected {
                pal.note_sel
            } else {
                draw::mix(&pal.note, &pal.bg, t)
            };
            draw::fill(s, &pal.note_edge, x, y + 1.0, nw, nh - 1.0);
            draw::fill(
                s,
                &body,
                x + 1.0,
                y + 2.0,
                (nw - 2.0).max(0.0),
                (nh - 3.0).max(0.0),
            );
            if nw >= 30.0 && vp.row_h >= 11.0 {
                draw::text_in(
                    self,
                    s,
                    if selected { &pal.bg } else { &pal.accent_fg },
                    x + 4.0,
                    y + 1.0,
                    nw - 6.0,
                    nh - 1.0,
                    &note_name(n.key),
                );
            }
        }

        // Cursor cell.
        if focused {
            let (ct, ck) = imp.cursor.get();
            let cx = vp.tick_to_x(ct as f64);
            let cy = vp.key_to_y(ck as i32);
            let cw = (snap as f64 * vp.px_per_tick).max(3.0);
            for (bx, by, bw, bh) in [
                (cx, cy, cw, 2.0),
                (cx, cy + vp.row_h - 2.0, cw, 2.0),
                (cx, cy, 2.0, vp.row_h),
                (cx + cw - 2.0, cy, 2.0, vp.row_h),
            ] {
                draw::fill(s, &pal.cursor, bx, by, bw, bh);
            }
        }

        // Marquee.
        if let Some(((x0, y0), (x1, y1))) = imp.marquee.get() {
            let (mx, my) = (x0.min(x1), y0.min(y1));
            let (mw, mh) = ((x1 - x0).abs(), (y1 - y0).abs());
            let mut c = pal.note_sel;
            c.set_alpha(0.25);
            draw::fill(s, &c, mx, my, mw, mh);
            draw::fill(s, &pal.note_sel, mx, my, mw, 1.0);
            draw::fill(s, &pal.note_sel, mx, my + mh, mw, 1.0);
            draw::fill(s, &pal.note_sel, mx, my, 1.0, mh);
            draw::fill(s, &pal.note_sel, mx + mw, my, 1.0, mh);
        }
        s.pop();

        // -- keyboard column --
        s.push_clip(&draw::rect(0.0, vp.grid_top(), vp.key_w, vp.grid_height()));
        draw::fill(
            s,
            &pal.key_white,
            0.0,
            vp.grid_top(),
            vp.key_w,
            vp.grid_height(),
        );
        let (_, ck) = imp.cursor.get();
        for key in lo..=hi {
            let y = vp.key_to_y(key);
            if vm::is_black_key(key as u8) {
                draw::fill(s, &pal.key_black, 0.0, y, vp.key_w * 0.62, vp.row_h);
            }
            if key as u8 == ck && focused {
                let mut c = pal.accent;
                c.set_alpha(0.6);
                draw::fill(s, &c, 0.0, y, vp.key_w, vp.row_h);
            }
            draw::hline(s, &pal.line_sub, y + vp.row_h, 0.0, vp.key_w);
            if key % 12 == 0 {
                draw::text_in(
                    self,
                    s,
                    &pal.key_text,
                    vp.key_w * 0.64,
                    y,
                    vp.key_w * 0.36 - 2.0,
                    vp.row_h,
                    &note_name(key as u8),
                );
            }
        }
        s.pop();

        // -- ruler --
        draw::fill(s, &pal.row_even, 0.0, 0.0, w, vp.ruler_h);
        draw::fill(s, &pal.bg, 0.0, 0.0, vp.key_w, vp.ruler_h);
        s.push_clip(&draw::rect(vp.key_w, 0.0, vp.grid_width(), vp.ruler_h));
        for (t, level) in vm::grid_lines(t0, t1, vp.px_per_tick, v.bar_ticks, snap, 9.0) {
            let x = vp.tick_to_x(t as f64);
            match level {
                2 => {
                    draw::vline(s, &pal.line_bar, x, 0.0, vp.ruler_h);
                    let bar = t / v.bar_ticks.max(1) + 1;
                    draw::text(self, s, &pal.text, x + 4.0, 3.0, &format!("{bar}"), true);
                }
                1 => draw::vline(s, &pal.line_beat, x, vp.ruler_h * 0.5, vp.ruler_h),
                _ => draw::vline(s, &pal.line_sub, x, vp.ruler_h * 0.75, vp.ruler_h),
            }
        }
        s.pop();
        draw::hline(s, &pal.line_bar, vp.ruler_h - 1.0, 0.0, w);

        // -- velocity lane --
        let vt = vp.vel_top();
        draw::fill(s, &pal.row_odd, vp.key_w, vt, vp.grid_width(), vp.vel_h);
        draw::hline(s, &pal.line_bar, vt, 0.0, w);
        draw::fill(s, &pal.bg, 0.0, vt, vp.key_w, vp.vel_h);
        draw::text(self, s, &pal.text_dim, 6.0, vt + 4.0, "Velocity", false);
        s.push_clip(&draw::rect(vp.key_w, vt, vp.grid_width(), vp.vel_h));
        for (t, level) in vm::grid_lines(t0, t1, vp.px_per_tick, v.bar_ticks, snap, 9.0) {
            if level >= 1 {
                let c = if level == 2 {
                    &pal.line_bar
                } else {
                    &pal.line_beat
                };
                draw::vline(s, c, vp.tick_to_x(t as f64), vt, vt + vp.vel_h);
            }
        }
        let base = vt + vp.vel_h - logic::VEL_PAD;
        for n in &v.notes {
            let x = vp.tick_to_x(n.start as f64) + 1.0;
            if x < vp.key_w - 6.0 || x > w + 6.0 {
                continue;
            }
            let y = vel_to_y(&vp, n.vel);
            let selected = sel.contains(&n.id);
            let col = if selected { pal.note_sel } else { pal.note };
            draw::fill(s, &col, x - 0.5, y, 2.0, base - y);
            draw::rounded(s, &col, x - 4.0, y - 4.0, 8.0, 8.0, 4.0);
        }
        s.pop();

        // -- playhead --
        if app_playing(&self.app()) && v.len_ticks > 0 {
            let tick = imp.last_playhead.get() % v.len_ticks as u64;
            let x = vp.tick_to_x(tick as f64);
            if x >= vp.key_w && x < w {
                draw::fill(s, &pal.playhead, x, 0.0, 2.0, h);
            }
        }
        // A separator between the keyboard and the grid.
        draw::vline(s, &pal.line_bar, vp.key_w - 1.0, vp.grid_top(), h);
        let _ = graphene::Point::new(0.0, 0.0);
    }
}

fn app_playing(app: &App) -> bool {
    app.ui.borrow().playing
}
