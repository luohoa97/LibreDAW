// SPDX-License-Identifier: GPL-3.0-or-later
//! The piano roll (docs/ui-design.md 3.4): a keyboard column, a bar ruler,
//! the note grid, and a velocity lane, with snap, zoom, and scroll on both
//! axes. Drawn with `snapshot()`; the logic is in `roll_logic` and
//! `view_math`.
//!
//! Drawing is layered. The static layer (rows, grid lines, notes, keyboard,
//! ruler, velocity stems) is recorded once into a render node and
//! re-appended until the data, the view (scroll, zoom, size), the snap, or
//! the style changes. Selection outlines, the cursor, the box selection,
//! held keys, and the playhead are drawn on top each frame.
//!
//! Mouse: press on empty space adds a note (drag to set its length) and it
//! sounds; Shift-drag on empty space selects with a box; press on a note
//! selects and drags it (each new pitch sounds); the right edge resizes;
//! double-click deletes a note; right click opens a menu; a key on the
//! keyboard column sounds while held; velocity stems are dragged in the lane
//! under the grid. Wheel scrolls, Shift-wheel scrolls sideways, Ctrl-wheel
//! zooms time, Ctrl-Shift-wheel zooms pitch.
//!
//! Keyboard: arrows move the cursor cell, Return adds or removes a note,
//! Shift-arrows resize the selection, Ctrl-arrows move it, + and - change
//! velocity, Delete removes, Ctrl-A selects all, Escape clears the selection,
//! Page Up and Page Down scroll an octave.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, gio};

use protocol::edit::{Edit, NewNote};
use protocol::engine::EngineCommand;
use protocol::ids::{ChannelId, NoteId, PatternId};
use protocol::model::Note;

use crate::app::App;
use crate::draw::{self, Palette, mix};
use crate::palette::{self, Role};
use crate::perf;
use crate::render_cache::{LayerCache, LayoutCache};
use crate::roll_logic::{
    self as logic, Drag, DragKind, Part, click_selection, hit_note, hit_stem, note_at_cursor,
    note_rect, notes_in_box, prune_selection, vel_to_y, with_pattern,
};
use crate::view_math::{self as vm, SNAPS, Viewport, note_name};
use doc::document::DEFAULT_STEP_VEL;

/// Corner points of a box selection.
type Marquee = ((f64, f64), (f64, f64));

/// Height of the velocity lane when it is shown.
const VEL_LANE_H: f64 = 64.0;

/// What the static layer depends on.
#[derive(Clone, Copy, PartialEq)]
pub struct StaticKey {
    revision: u64,
    pattern: Option<PatternId>,
    channel: Option<ChannelId>,
    palette_gen: u64,
    px_per_tick: u64,
    row_h: u64,
    scroll_x: u64,
    scroll_y: u64,
    width: u64,
    height: u64,
    vel_h: u64,
    snap: usize,
}

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
        /// The pattern and channel the view was last scrolled for.
        pub last_shown: Cell<Option<(PatternId, ChannelId)>>,
        /// A saved scroll was restored: do not scroll to the notes once.
        pub restored: Cell<bool>,
        /// Last note press, for double-click.
        pub last_click: Cell<Option<(i64, NoteId)>>,
        pub view_cache: RefCell<Option<Rc<View>>>,
        pub cache: RefCell<LayerCache<StaticKey>>,
        pub text: RefCell<LayoutCache>,
        pub menu: RefCell<Option<gtk::PopoverMenu>>,
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
                last_shown: Cell::new(None),
                restored: Cell::new(false),
                last_click: Cell::new(None),
                view_cache: RefCell::new(None),
                cache: RefCell::new(LayerCache::new("piano-roll static")),
                text: RefCell::new(LayoutCache::default()),
                menu: RefCell::new(None),
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
            palette::watch(&*obj);

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

        fn dispose(&self) {
            if let Some(m) = self.menu.borrow_mut().take() {
                m.unparent();
            }
        }
    }

    impl WidgetImpl for PianoRoll {
        fn measure(&self, o: gtk::Orientation, _: i32) -> (i32, i32, i32, i32) {
            let (min, nat) = if o == gtk::Orientation::Horizontal {
                (120, 600)
            } else {
                (160, 360)
            };
            (min, nat, -1, -1)
        }

        fn size_allocate(&self, w: i32, h: i32, _baseline: i32) {
            let mut vp = self.vp.get();
            vp.width = w as f64;
            vp.height = h as f64;
            self.vp.set(vp);
            self.obj().sync_adjustments();
            if let Some(m) = self.menu.borrow().as_ref() {
                m.present();
            }
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
pub struct View {
    revision: u64,
    pattern: PatternId,
    channel: ChannelId,
    root_key: u8,
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

    /// Shows or hides the velocity lane.
    pub fn set_velocity_lane(&self, on: bool) {
        let mut vp = self.imp().vp.get();
        vp.vel_h = if on { VEL_LANE_H } else { 0.0 };
        self.imp().vp.set(vp);
        self.sync_adjustments();
        self.queue_draw();
    }

    pub fn velocity_lane(&self) -> bool {
        self.imp().vp.get().vel_h > 0.0
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
        // The saved scroll wins over scrolling to the notes, once.
        self.imp().restored.set(true);
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

    /// Back to the default zoom on both axes.
    pub fn reset_zoom(&self) {
        let d = Viewport::default();
        let len = self.view().map(|v| v.len_ticks).unwrap_or(0);
        let mut vp = self.imp().vp.get();
        vp.px_per_tick = d.px_per_tick;
        vp.row_h = d.row_h;
        vp.clamp_scroll(len);
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

    /// The shown pattern and channel, cached per document revision (the
    /// notes are cloned once per change, not once per frame).
    fn view(&self) -> Option<Rc<View>> {
        let app = self.app();
        let s = app.session.borrow();
        let revision = s.document().revision;
        let p = &s.document().project;
        let pid = app.current_pattern()?;
        let cid = app.current_channel()?;
        if let Some(v) = self.imp().view_cache.borrow().as_ref()
            && v.revision == revision
            && v.pattern == pid
            && v.channel == cid
        {
            return Some(v.clone());
        }
        let pat = p.pattern(pid)?;
        let ch = p.channel(cid)?;
        let v = Rc::new(View {
            revision,
            pattern: pid,
            channel: cid,
            root_key: ch.root_key,
            notes: pat.notes_of(cid).to_vec(),
            len_ticks: pat.length_ticks(),
            step_ticks: pat.step_ticks,
            bar_ticks: protocol::model::ticks_per_bar(p.time_sig_num),
        });
        *self.imp().view_cache.borrow_mut() = Some(v.clone());
        Some(v)
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
            self.scroll_to_notes_if_new(&v);
        } else {
            self.imp().selection.borrow_mut().clear();
            self.imp().last_shown.set(None);
        }
        self.sync_adjustments();
        self.update_label();
        self.queue_draw();
    }

    /// When the channel or pattern changes, scrolls to its notes (or to its
    /// root key when it has none) instead of leaving the view where the
    /// previous channel was.
    fn scroll_to_notes_if_new(&self, v: &View) {
        let imp = self.imp();
        let now = Some((v.pattern, v.channel));
        if imp.last_shown.get() == now {
            return;
        }
        imp.last_shown.set(now);
        if imp.restored.replace(false) {
            return;
        }
        let (first, lo, hi) = notes_extent(&v.notes, v.root_key);
        let mut vp = imp.vp.get();
        vp.scroll_to_notes(first, lo as i32, hi as i32, v.len_ticks);
        imp.vp.set(vp);
        imp.cursor.set((first, lo));
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
                "Piano roll, no channel selected",
            )]);
            return;
        };
        let name = self
            .app()
            .session
            .borrow()
            .document()
            .project
            .channel(v.channel)
            .map(|c| c.name.clone())
            .unwrap_or_default();
        let (tick, key) = self.imp().cursor.get();
        let bar = tick / v.bar_ticks.max(1) + 1;
        let beat = (tick % v.bar_ticks.max(1)) / protocol::consts::PPQ + 1;
        let note =
            note_at_cursor(&v.notes, tick, key).and_then(|id| v.notes.iter().find(|n| n.id == id));
        let at = match note {
            Some(n) => format!("note, length {} ticks, velocity {}", n.len, n.vel),
            None => "no note".to_string(),
        };
        let value = format!("{}, bar {bar} beat {beat}, {at}", note_name(key));
        self.update_property(&[
            gtk::accessible::Property::Label(&format!("Piano roll for {name}")),
            gtk::accessible::Property::ValueText(&format!(
                "{value}, {} notes selected",
                self.imp().selection.borrow().len()
            )),
        ]);
    }

    fn tick(&self) {
        let t = self.app().playhead_tick();
        if self.imp().last_playhead.get() != t {
            self.imp().last_playhead.set(t);
            if self.app().ui.borrow().playing {
                self.queue_draw();
            }
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
                // Double-click deletes (right click is for the menu).
                let now = glib::monotonic_time() / 1000;
                if let Some((t, last)) = imp.last_click.get()
                    && last == id
                    && now - t < 450
                {
                    imp.last_click.set(None);
                    if app.gesture_begin("Delete note") {
                        app.gesture_edit(vec![Edit::RemoveNotes {
                            pattern: v.pattern,
                            notes: vec![id],
                        }]);
                        app.gesture_end();
                    }
                    imp.selection.borrow_mut().clear();
                    self.after_input();
                    return;
                }
                imp.last_click.set(Some((now, id)));
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
                            off: 0,
                            repeat: 1,
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
                && let Some(v2) = self.view()
                && let Some(n) = v2.notes.iter().find(|n| n.id == d.ids[0])
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
            if !sel.contains(&id) {
                *self.imp().selection.borrow_mut() = vec![id];
            }
            self.after_input();
            self.show_menu(x, y);
        }
    }

    /// The note menu: Delete and Duplicate.
    fn show_menu(&self, x: f64, y: f64) {
        let menu = gio::Menu::new();
        menu.append(Some("_Delete"), Some("roll.delete"));
        menu.append(Some("D_uplicate"), Some("roll.duplicate"));
        let group = gio::SimpleActionGroup::new();
        let w = self.downgrade();
        let del = gio::SimpleAction::new("delete", None);
        del.connect_activate(move |_, _| {
            if let Some(o) = w.upgrade() {
                o.delete_selection();
            }
        });
        group.add_action(&del);
        let w = self.downgrade();
        let dup = gio::SimpleAction::new("duplicate", None);
        dup.connect_activate(move |_, _| {
            if let Some(o) = w.upgrade() {
                o.duplicate_selection();
            }
        });
        group.add_action(&dup);
        self.insert_action_group("roll", Some(&group));
        let pop = gtk::PopoverMenu::from_model(Some(&menu));
        pop.set_parent(self);
        pop.set_has_arrow(false);
        pop.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        let w = self.downgrade();
        pop.connect_closed(move |p| {
            let p = p.clone();
            let w = w.clone();
            glib::idle_add_local_once(move || {
                if let Some(o) = w.upgrade()
                    && let Some(m) = o.imp().menu.borrow_mut().take()
                    && m == p
                {
                    m.unparent();
                }
            });
        });
        if let Some(old) = self.imp().menu.borrow_mut().replace(pop.clone()) {
            old.unparent();
        }
        pop.popup();
    }

    fn delete_selection(&self) {
        let Some(v) = self.view() else { return };
        let sel = self.imp().selection.borrow().clone();
        if !sel.is_empty() {
            self.app().edit_quiet(vec![Edit::RemoveNotes {
                pattern: v.pattern,
                notes: sel,
            }]);
        }
    }

    /// Copies the selected notes to just after the selection.
    fn duplicate_selection(&self) {
        let Some(v) = self.view() else { return };
        let sel = self.imp().selection.borrow().clone();
        let chosen: Vec<&Note> = v.notes.iter().filter(|n| sel.contains(&n.id)).collect();
        let Some(first) = chosen.iter().map(|n| n.start).min() else {
            return;
        };
        let end = chosen
            .iter()
            .map(|n| n.start + n.len)
            .max()
            .unwrap_or(first);
        let shift = (end - first).max(1);
        let copies: Vec<NewNote> = chosen
            .iter()
            .map(|n| NewNote {
                start: n.start + shift,
                len: n.len,
                key: n.key,
                vel: n.vel,
            })
            .collect();
        if let Some(a) = self.app().edit(vec![Edit::AddNotes {
            pattern: v.pattern,
            channel: v.channel,
            notes: copies,
        }]) {
            *self.imp().selection.borrow_mut() = a.created.iter().map(|i| NoteId(*i)).collect();
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
            // Move the selection (an octave with Shift).
            let dkey = dir_k * if shift { 12 } else { 1 };
            app.edit_quiet(vec![Edit::MoveNotes {
                pattern: v.pattern,
                notes: sel,
                dt: dir_t * snap,
                dkey: dkey as i16,
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
            if dir_k != 0 {
                app.preview_pulse(v.channel, nk, DEFAULT_STEP_VEL, 150);
            }
            return true;
        }
        match key {
            gdk::Key::Return | gdk::Key::KP_Enter => {
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
                self.delete_selection();
                true
            }
            gdk::Key::plus
            | gdk::Key::equal
            | gdk::Key::KP_Add
            | gdk::Key::minus
            | gdk::Key::KP_Subtract
                if !ctrl && !sel.is_empty() =>
            {
                let up = matches!(key, gdk::Key::plus | gdk::Key::equal | gdk::Key::KP_Add);
                let edits: Vec<Edit> = v
                    .notes
                    .iter()
                    .filter(|n| sel.contains(&n.id))
                    .map(|n| Edit::SetNoteVelocity {
                        pattern: v.pattern,
                        notes: vec![n.id],
                        vel: (n.vel as i32 + if up { 8 } else { -8 }).clamp(1, 127) as u8,
                    })
                    .collect();
                app.edit_quiet(edits);
                true
            }
            gdk::Key::a if ctrl && !shift => {
                *imp.selection.borrow_mut() = v.notes.iter().map(|n| n.id).collect();
                self.after_input();
                true
            }
            gdk::Key::a if ctrl && shift => {
                imp.selection.borrow_mut().clear();
                self.after_input();
                true
            }
            gdk::Key::Page_Up | gdk::Key::Page_Down => {
                let mut vp = imp.vp.get();
                let d = if key == gdk::Key::Page_Up { -1.0 } else { 1.0 };
                vp.scroll_y += d * 12.0 * vp.row_h;
                vp.clamp_scroll(v.len_ticks);
                imp.vp.set(vp);
                self.sync_adjustments();
                self.queue_draw();
                true
            }
            gdk::Key::Home | gdk::Key::End => {
                let mut vp = imp.vp.get();
                vp.scroll_x = if key == gdk::Key::Home {
                    0.0
                } else {
                    vp.content_width(v.len_ticks)
                };
                vp.clamp_scroll(v.len_ticks);
                imp.vp.set(vp);
                self.sync_adjustments();
                self.queue_draw();
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

    fn static_key(&self) -> StaticKey {
        let app = self.app();
        let vp = self.imp().vp.get();
        let revision = app.session.borrow().document().revision;
        StaticKey {
            revision,
            pattern: app.current_pattern(),
            channel: app.current_channel(),
            palette_gen: palette::generation(),
            px_per_tick: vp.px_per_tick.to_bits(),
            row_h: vp.row_h.to_bits(),
            scroll_x: vp.scroll_x.to_bits(),
            scroll_y: vp.scroll_y.to_bits(),
            width: vp.width.to_bits(),
            height: vp.height.to_bits(),
            vel_h: vp.vel_h.to_bits(),
            snap: self.imp().snap_idx.get(),
        }
    }

    fn draw(&self, s: &gtk::Snapshot) {
        let _frame = perf::frame("piano-roll");
        let imp = self.imp();
        let Some(v) = self.view() else {
            // No channel: the page shows a status page instead; draw the
            // flat background so there is no flash.
            let pal = Palette::current();
            let vp = imp.vp.get();
            draw::fill(s, &pal.bg, 0.0, 0.0, vp.width, vp.height);
            return;
        };
        let key = self.static_key();
        {
            let mut cache = imp.cache.borrow_mut();
            cache.append(s, key, |rec| self.draw_static(rec, &v));
        }
        self.draw_dynamic(s, &v);
    }

    fn draw_static(&self, s: &gtk::Snapshot, v: &View) {
        let pal = Palette::current();
        let colors = palette::colors();
        let imp = self.imp();
        let vp = imp.vp.get();
        let (w, h) = (vp.width, vp.height);
        let snap = self.snap_ticks(v);
        let hc = colors.high_contrast;
        let mut text = imp.text.borrow_mut();
        draw::fill(s, &pal.bg, 0.0, 0.0, w, h);

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

        // Notes in the channel's color; louder is more opaque, and the
        // border keeps every note visible on any color.
        let body = colors.channel_color(v.channel.0);
        let edge = if hc {
            colors.get(Role::WindowFg)
        } else {
            mix(&pal.bg, &body, 0.45)
        };
        for n in &v.notes {
            let (x, y, nw, nh) = note_rect(&vp, n);
            if x + nw < vp.key_w || x > w || y + nh < vp.grid_top() || y > vp.grid_bottom() {
                continue;
            }
            let t = 0.55 + 0.45 * (n.vel as f32 / 127.0);
            let fill = mix(&body, &pal.bg, t);
            if nw > 6.0 && nh > 6.0 {
                draw::rounded(s, &edge, x, y + 1.0, nw, nh - 1.0, 3.0);
                draw::rounded(s, &fill, x + 1.0, y + 2.0, nw - 2.0, nh - 3.0, 2.0);
            } else {
                draw::fill(s, &fill, x, y + 1.0, nw, nh - 1.0);
            }
            if nw >= 30.0 && nh >= 14.0 {
                let l = text.get(self, &note_name(n.key), false);
                draw::layout_in(
                    s,
                    &l,
                    &palette::readable_on(&fill),
                    x + 4.0,
                    y + 1.0,
                    nw - 6.0,
                    nh - 1.0,
                );
            }
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
        for key in lo..=hi {
            let y = vp.key_to_y(key);
            if vm::is_black_key(key as u8) {
                draw::fill(s, &pal.key_black, 0.0, y, vp.key_w * 0.62, vp.row_h);
            }
            draw::hline(s, &pal.line_sub, y + vp.row_h, 0.0, vp.key_w);
            if key % 12 == 0 {
                let l = text.get(self, &note_name(key as u8), false);
                draw::layout_in(
                    s,
                    &l,
                    &pal.key_text,
                    vp.key_w * 0.64,
                    y,
                    vp.key_w * 0.36 - 2.0,
                    vp.row_h,
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
                    let l = text.get(self, &format!("{bar}"), true);
                    draw::layout_at(s, &l, &pal.text, x + 4.0, 3.0);
                }
                1 => draw::vline(s, &pal.line_beat, x, vp.ruler_h * 0.5, vp.ruler_h),
                _ => draw::vline(s, &pal.line_sub, x, vp.ruler_h * 0.75, vp.ruler_h),
            }
        }
        s.pop();
        draw::hline(s, &pal.line_bar, vp.ruler_h - 1.0, 0.0, w);

        // -- velocity lane --
        if vp.vel_h > 0.0 {
            let vt = vp.vel_top();
            draw::fill(s, &pal.row_odd, vp.key_w, vt, vp.grid_width(), vp.vel_h);
            draw::hline(s, &pal.line_bar, vt, 0.0, w);
            draw::fill(s, &pal.bg, 0.0, vt, vp.key_w, vp.vel_h);
            let l = text.get(self, "Velocity", false);
            draw::layout_at(s, &l, &pal.text_dim, 6.0, vt + 4.0);
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
                draw::fill(s, &body, x - 0.5, y, 2.0, base - y);
                draw::rounded(s, &body, x - 4.0, y - 4.0, 8.0, 8.0, 4.0);
            }
            s.pop();
        }
        // A separator between the keyboard and the grid.
        draw::vline(s, &pal.line_bar, vp.key_w - 1.0, vp.grid_top(), h);
    }

    /// Selection, cursor, box, held key, and playhead: light per-frame
    /// drawing on top of the cached layer.
    fn draw_dynamic(&self, s: &gtk::Snapshot, v: &View) {
        let imp = self.imp();
        let vp = imp.vp.get();
        let colors = palette::colors();
        let accent = colors.get(Role::Accent);
        let w = vp.width;
        let snap = self.snap_ticks(v);
        let focused = self.has_focus();

        s.push_clip(&draw::rect(
            vp.key_w,
            vp.grid_top(),
            vp.grid_width(),
            vp.grid_height(),
        ));
        {
            let sel = imp.selection.borrow();
            if !sel.is_empty() {
                let t = if colors.high_contrast { 3.0 } else { 2.0 };
                for n in v.notes.iter().filter(|n| sel.contains(&n.id)) {
                    let (x, y, nw, nh) = note_rect(&vp, n);
                    if x + nw < vp.key_w || x > w || y + nh < vp.grid_top() || y > vp.grid_bottom()
                    {
                        continue;
                    }
                    draw::outline(s, &accent, x - 1.0, y, nw + 2.0, nh + 1.0, t);
                    // Resize handle: three short vertical lines.
                    if nw >= 14.0 {
                        for i in 0..3 {
                            draw::fill(
                                s,
                                &accent,
                                x + nw - 7.0 + i as f64 * 2.0,
                                y + nh * 0.3,
                                1.0,
                                nh * 0.4,
                            );
                        }
                    }
                }
            }
        }
        // Cursor cell, only for keyboard focus.
        if focused && self.keyboard_focus_visible() {
            let (ct, ck) = imp.cursor.get();
            let cx = vp.tick_to_x(ct as f64);
            let cy = vp.key_to_y(ck as i32);
            let cw = (snap as f64 * vp.px_per_tick).max(3.0);
            let ring = if colors.high_contrast {
                colors.get(Role::WindowFg)
            } else {
                accent
            };
            draw::outline(s, &ring, cx, cy, cw, vp.row_h, 2.0);
        }
        // Box selection.
        if let Some(((x0, y0), (x1, y1))) = imp.marquee.get() {
            let (mx, my) = (x0.min(x1), y0.min(y1));
            let (mw, mh) = ((x1 - x0).abs(), (y1 - y0).abs());
            let mut c = accent;
            c.set_alpha(0.2);
            draw::fill(s, &c, mx, my, mw, mh);
            draw::outline(s, &accent, mx, my, mw, mh, 1.0);
        }
        s.pop();

        // Held or cursor key on the keyboard column.
        let key_hl = imp.held_key.get().or(if focused {
            Some(imp.cursor.get().1)
        } else {
            None
        });
        if let Some(k) = key_hl {
            let y = vp.key_to_y(k as i32);
            if y + vp.row_h > vp.grid_top() && y < vp.grid_bottom() {
                s.push_clip(&draw::rect(0.0, vp.grid_top(), vp.key_w, vp.grid_height()));
                let mut c = accent;
                c.set_alpha(if imp.held_key.get().is_some() {
                    0.7
                } else {
                    0.35
                });
                draw::fill(s, &c, 0.0, y, vp.key_w - 1.0, vp.row_h);
                s.pop();
            }
        }

        // Selected stems.
        if vp.vel_h > 0.0 {
            let sel = imp.selection.borrow();
            if !sel.is_empty() {
                s.push_clip(&draw::rect(
                    vp.key_w,
                    vp.vel_top(),
                    vp.grid_width(),
                    vp.vel_h,
                ));
                for n in v.notes.iter().filter(|n| sel.contains(&n.id)) {
                    let x = vp.tick_to_x(n.start as f64) + 1.0;
                    let y = vel_to_y(&vp, n.vel);
                    draw::outline(s, &accent, x - 5.0, y - 5.0, 10.0, 10.0, 2.0);
                }
                s.pop();
            }
        }

        // Playhead.
        if self.app().ui.borrow().playing && v.len_ticks > 0 {
            let tick = imp.last_playhead.get() % v.len_ticks as u64;
            let x = vp.tick_to_x(tick as f64);
            if x >= vp.key_w && x < w {
                draw::fill(s, &accent, x, 0.0, 2.0, vp.height);
                draw::fill(s, &accent, x - 3.0, 0.0, 8.0, 3.0);
                draw::fill(s, &accent, x - 1.0, 3.0, 4.0, 2.0);
            }
        }
    }

    fn keyboard_focus_visible(&self) -> bool {
        self.root()
            .and_then(|r| r.downcast::<gtk::Window>().ok())
            .map(|w| w.property::<bool>("focus-visible"))
            .unwrap_or(true)
    }
}

/// First tick, lowest key, and highest key of a channel's notes; for a
/// channel without notes, tick 0 and its root key.
pub fn notes_extent(notes: &[Note], root_key: u8) -> (u32, u8, u8) {
    if notes.is_empty() {
        return (0, root_key, root_key);
    }
    let first = notes.iter().map(|n| n.start).min().unwrap_or(0);
    let lo = notes.iter().map(|n| n.key).min().unwrap_or(root_key);
    let hi = notes.iter().map(|n| n.key).max().unwrap_or(root_key);
    (first, lo, hi)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(id: u32, start: u32, key: u8) -> Note {
        Note {
            id: NoteId(id),
            start,
            len: 240,
            key,
            vel: 100,
            off: 0,
            repeat: 1,
        }
    }

    #[test]
    fn extent_of_notes() {
        assert_eq!(notes_extent(&[], 36), (0, 36, 36));
        let notes = [n(1, 480, 40), n(2, 0, 36), n(3, 960, 43)];
        assert_eq!(notes_extent(&notes, 60), (0, 36, 43));
    }
}
