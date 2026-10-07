// SPDX-License-Identifier: GPL-3.0-or-later
//! The Timeline canvas (SPEC 20): one lane per instrument, clips on the
//! lanes, the bar ruler with the loop strip, and the playhead. The
//! instrument names are stock rows beside it (`ChannelList`), scrolled by
//! the same vertical adjustment. Geometry and rules live in
//! `timeline_logic`; what each clip action does lives in `clip_ops`.
//!
//! Pointer: click an empty spot to add a one-bar clip; click a clip to
//! select it (Shift or Ctrl adds); drag it to move (Ctrl or Alt drags a
//! linked copy); drag its ends to resize; double-click opens it in the
//! editor; the ruler moves the playhead; drag in the loop strip to set the
//! loop, click it to turn the loop on or off. Keys: arrows choose clips,
//! Return edits, Delete removes, Ctrl+D duplicates, S splits at the
//! playhead, 0 mutes, Ctrl+A selects all, Escape deselects. The menu is
//! `menus::clip_menu`.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, gio, gsk};

use protocol::edit::Edit;
use protocol::engine::EngineCommand;
use protocol::ids::{ChannelId, ClipId};
use protocol::model::{Clip, ticks_per_bar};

use crate::app::App;
use crate::clip_ops;
use crate::draw::{self, Palette, mix};
use crate::palette::{self, Role};
use crate::render_cache::{LayerCache, LayoutCache};
use crate::timeline_logic::{self as tl, Hit, Part, View};

/// What the static layer depends on.
#[derive(Clone, PartialEq)]
pub struct StaticKey {
    revision: u64,
    view: (u64, u64, u64, u64),
    size: (i32, i32),
    selected: Vec<ClipId>,
    palette_gen: u64,
}

/// A drag in progress. Moves, copies and resizes show a ghost and apply
/// one edit when the button is released (one undo step).
#[derive(Clone, Debug, PartialEq)]
pub enum Drag {
    Move {
        ids: Vec<ClipId>,
        press_tick: f64,
        dt: i64,
        copy: bool,
    },
    Resize {
        ids: Vec<ClipId>,
        from_start: bool,
        press_tick: f64,
        dlen: i64,
    },
    Loop {
        anchor: u32,
        end: u32,
        moved: bool,
    },
    Seek,
}

mod imp {
    use super::*;

    pub struct Timeline {
        pub app: RefCell<Option<Rc<App>>>,
        pub view: Cell<View>,
        pub hadj: gtk::Adjustment,
        pub vadj: gtk::Adjustment,
        pub updating_adj: Cell<bool>,
        pub selected: RefCell<Vec<ClipId>>,
        pub drag: RefCell<Option<Drag>>,
        pub press: Cell<(f64, f64)>,
        pub pointer: Cell<(f64, f64)>,
        pub last_playhead: Cell<u64>,
        pub cache: RefCell<LayerCache<StaticKey>>,
        pub text: RefCell<LayoutCache>,
    }

    impl Default for Timeline {
        fn default() -> Timeline {
            Timeline {
                app: RefCell::new(None),
                view: Cell::new(View::default()),
                hadj: gtk::Adjustment::new(0.0, 0.0, 1.0, 20.0, 200.0, 1.0),
                vadj: gtk::Adjustment::new(0.0, 0.0, 1.0, 20.0, 200.0, 1.0),
                updating_adj: Cell::new(false),
                selected: RefCell::new(Vec::new()),
                drag: RefCell::new(None),
                press: Cell::new((0.0, 0.0)),
                pointer: Cell::new((0.0, 0.0)),
                last_playhead: Cell::new(0),
                cache: RefCell::new(LayerCache::new("timeline static")),
                text: RefCell::new(LayoutCache::default()),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Timeline {
        const NAME: &'static str = "LibreDawTimeline";
        type Type = super::Timeline;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_accessible_role(gtk::AccessibleRole::Grid);
            klass.set_css_name("timeline");
        }
    }

    impl ObjectImpl for Timeline {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            obj.set_focusable(true);
            obj.set_hexpand(true);
            obj.set_vexpand(true);
            obj.update_property(&[gtk::accessible::Property::Label("Timeline")]);
            palette::watch(&*obj);

            let drag = gtk::GestureDrag::new();
            drag.set_button(gdk::BUTTON_PRIMARY);
            let w = obj.downgrade();
            drag.connect_drag_begin(move |g, x, y| {
                if let Some(o) = w.upgrade() {
                    o.press(x, y, g.current_event_state());
                }
            });
            let w = obj.downgrade();
            drag.connect_drag_update(move |_, dx, dy| {
                if let Some(o) = w.upgrade() {
                    let (sx, sy) = o.imp().press.get();
                    o.drag_to(sx + dx, sy + dy);
                }
            });
            let w = obj.downgrade();
            drag.connect_drag_end(move |_, dx, dy| {
                if let Some(o) = w.upgrade() {
                    o.release(dx.abs() + dy.abs() > 3.0);
                }
            });
            obj.add_controller(drag);

            let dbl = gtk::GestureClick::new();
            dbl.set_button(gdk::BUTTON_PRIMARY);
            let w = obj.downgrade();
            dbl.connect_pressed(move |_, n, x, y| {
                if n == 2
                    && let Some(o) = w.upgrade()
                {
                    o.double_click(x, y);
                }
            });
            obj.add_controller(dbl);

            let motion = gtk::EventControllerMotion::new();
            let w = obj.downgrade();
            motion.connect_motion(move |_, x, y| {
                if let Some(o) = w.upgrade() {
                    o.imp().pointer.set((x, y));
                    o.update_cursor(x, y);
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

            // Plain-language help where the pointer is (SPEC 20.6): what a
            // bar, the loop strip, a clip and an empty row are for.
            obj.set_has_tooltip(true);
            let w = obj.downgrade();
            obj.connect_query_tooltip(move |_, x, y, _, tip| {
                let Some(o) = w.upgrade() else { return false };
                let text = tooltip_at(&tl::hit(
                    &o.imp().view.get(),
                    &o.rows(),
                    &o.project_clips(),
                    x as f64,
                    y as f64,
                ));
                tip.set_text(Some(text));
                true
            });

            let keys = gtk::EventControllerKey::new();
            let w = obj.downgrade();
            keys.connect_key_pressed(move |_, key, _, st| match w.upgrade() {
                Some(o) if o.key(key, st) => glib::Propagation::Stop,
                _ => glib::Propagation::Proceed,
            });
            obj.add_controller(keys);

            for adj in [&self.hadj, &self.vadj] {
                let w = obj.downgrade();
                adj.connect_value_changed(move |_| {
                    if let Some(o) = w.upgrade()
                        && !o.imp().updating_adj.get()
                    {
                        let mut v = o.imp().view.get();
                        v.scroll_x = o.imp().hadj.value();
                        v.scroll_y = o.imp().vadj.value();
                        o.imp().view.set(v);
                        o.queue_draw();
                    }
                });
            }

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
            crate::context_menu::unparent_popovers(self.obj().upcast_ref());
        }
    }

    impl WidgetImpl for Timeline {
        fn measure(&self, o: gtk::Orientation, _: i32) -> (i32, i32, i32, i32) {
            let (min, nat) = if o == gtk::Orientation::Horizontal {
                (160, 800)
            } else {
                (tl::RULER_H as i32 + 60, 320)
            };
            (min, nat, -1, -1)
        }

        fn size_allocate(&self, w: i32, h: i32, _baseline: i32) {
            let mut v = self.view.get();
            v.width = w as f64;
            v.height = h as f64;
            self.view.set(v);
            self.obj().sync_adjustments();
            crate::context_menu::present_popovers(self.obj().upcast_ref());
        }

        fn snapshot(&self, s: &gtk::Snapshot) {
            self.obj().draw(s);
        }
    }
}

/// The tooltip for what the pointer is over.
pub fn tooltip_at(hit: &Hit) -> &'static str {
    match hit {
        Hit::Ruler { .. } => {
            "Bars: each number is one bar of music, four beats long; click to move the playhead"
        }
        Hit::Loop { .. } => {
            "Loop: drag here to choose the part that repeats; click to turn looping on or off"
        }
        Hit::Clip { .. } => {
            "A clip: a piece of music on a row; drag to move it, double-click to change its notes"
        }
        Hit::Lane { .. } => "Click to add a one-bar clip here",
        Hit::Below { .. } => "Add an instrument to get a new row",
    }
}

glib::wrapper! {
    pub struct Timeline(ObjectSubclass<imp::Timeline>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Timeline {
    pub fn new(app: Rc<App>) -> Timeline {
        let o: Timeline = glib::Object::new();
        let w = o.downgrade();
        app.on_change(move || {
            if let Some(o) = w.upgrade() {
                o.after_change();
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
        o.install_menu();
        o
    }

    fn app(&self) -> Rc<App> {
        self.imp().app.borrow().clone().expect("app set")
    }

    pub fn hadj(&self) -> gtk::Adjustment {
        self.imp().hadj.clone()
    }

    /// The vertical scroll, shared with the instrument names.
    pub fn vadj(&self) -> gtk::Adjustment {
        self.imp().vadj.clone()
    }

    /// Row height: the instrument names use the same.
    pub fn row_h(&self) -> f64 {
        self.imp().view.get().row_h
    }

    fn apply_size_class(&self) {
        let touch = self.app().size_class().touch();
        let mut v = self.imp().view.get();
        v.row_h = if touch { tl::ROW_H_TOUCH } else { tl::ROW_H };
        self.imp().view.set(v);
        self.sync_adjustments();
        self.queue_draw();
    }

    /// The selected clips (the editor shows the first).
    pub fn selected(&self) -> Vec<ClipId> {
        self.imp().selected.borrow().clone()
    }

    fn set_selected(&self, ids: Vec<ClipId>) {
        let first = ids.first().copied();
        *self.imp().selected.borrow_mut() = ids;
        let app = self.app();
        match first {
            Some(c) if app.current_clip() != Some(c) => app.select_clip(c),
            None if app.current_clip().is_some() => {
                app.deselect_clip();
            }
            _ => self.queue_draw(),
        }
        self.update_label();
    }

    /// Keeps the selection pointing at clips that exist, and follows the
    /// app's selected clip when something else chose it.
    fn after_change(&self) {
        let app = self.app();
        let (alive, current): (Vec<ClipId>, Option<ClipId>) = {
            let s = app.session.borrow();
            let p = &s.document().project;
            (p.clips.iter().map(|c| c.id).collect(), app.current_clip())
        };
        {
            let mut sel = self.imp().selected.borrow_mut();
            sel.retain(|c| alive.contains(c));
            match current {
                Some(c) if !sel.contains(&c) => *sel = vec![c],
                None => sel.clear(),
                _ => {}
            }
        }
        self.sync_adjustments();
        self.update_label();
        self.queue_draw();
    }

    fn rows(&self) -> Vec<ChannelId> {
        let app = self.app();
        let s = app.session.borrow();
        s.document().project.channels.iter().map(|c| c.id).collect()
    }

    fn project_clips(&self) -> Vec<Clip> {
        self.app().session.borrow().document().project.clips.clone()
    }

    fn bar_ticks(&self) -> u32 {
        ticks_per_bar(self.app().session.borrow().document().project.time_sig_num)
    }

    fn end_tick(&self) -> u32 {
        let app = self.app();
        let s = app.session.borrow();
        let p = &s.document().project;
        // At least 16 bars of room, and a few bars past the last clip.
        let bar = ticks_per_bar(p.time_sig_num);
        (tl::end_tick(&p.clips, p.loop_region.end) + 4 * bar).max(16 * bar)
    }

    fn sync_adjustments(&self) {
        let imp = self.imp();
        let rows = self.rows().len();
        let mut v = imp.view.get();
        let end = self.end_tick();
        v.clamp_scroll(end, rows);
        imp.view.set(v);
        imp.updating_adj.set(true);
        let content_w = end as f64 * v.px_per_tick;
        imp.hadj.configure(
            v.scroll_x,
            0.0,
            content_w.max(v.width),
            20.0,
            v.width * 0.9,
            v.width,
        );
        let lanes_h = (v.height - tl::RULER_H).max(1.0);
        let content_h = rows as f64 * v.row_h + v.row_h;
        imp.vadj.configure(
            v.scroll_y,
            0.0,
            content_h.max(lanes_h),
            v.row_h / 2.0,
            lanes_h * 0.9,
            lanes_h,
        );
        imp.updating_adj.set(false);
    }

    /// Zooms time by `factor` around the pointer (Ctrl+scroll) or the
    /// middle (buttons, Ctrl++ and Ctrl+-).
    pub fn zoom_x(&self, factor: f64) {
        let mut v = self.imp().view.get();
        let anchor = self.imp().pointer.get().0.clamp(0.0, v.width);
        v.zoom_x(factor, if anchor > 0.0 { anchor } else { v.width / 2.0 });
        self.imp().view.set(v);
        self.sync_adjustments();
        self.queue_draw();
    }

    pub fn reset_zoom(&self) {
        let mut v = self.imp().view.get();
        v.px_per_tick = View::default().px_per_tick;
        self.imp().view.set(v);
        self.sync_adjustments();
        self.queue_draw();
    }

    fn update_label(&self) {
        let app = self.app();
        let s = app.session.borrow();
        let p = &s.document().project;
        let bar = ticks_per_bar(p.time_sig_num);
        let sel = self.imp().selected.borrow();
        let text = match sel
            .first()
            .and_then(|id| p.clips.iter().find(|c| c.id == *id))
        {
            Some(c) => {
                let inst = p
                    .channel(c.instrument)
                    .map(|ch| ch.name.clone())
                    .unwrap_or_default();
                let bars = c.len as f64 / bar as f64;
                format!(
                    "{inst} clip at {}, {bars:.2} bars{}",
                    tl::spoken(c.start, bar),
                    if c.muted { ", muted" } else { "" }
                )
            }
            None => "Timeline, no clip selected".to_string(),
        };
        drop(sel);
        drop(s);
        self.update_property(&[
            gtk::accessible::Property::Label("Timeline"),
            gtk::accessible::Property::ValueText(&text),
        ]);
    }

    // ---- input ----

    fn update_cursor(&self, x: f64, y: f64) {
        if self.imp().drag.borrow().is_some() {
            return;
        }
        let hit = tl::hit(
            &self.imp().view.get(),
            &self.rows(),
            &self.project_clips(),
            x,
            y,
        );
        let name = match hit {
            Hit::Clip {
                part: Part::Start | Part::End,
                ..
            } => Some("ew-resize"),
            _ => None,
        };
        self.set_cursor_from_name(name);
    }

    fn press(&self, x: f64, y: f64, state: gdk::ModifierType) {
        self.grab_focus();
        let imp = self.imp();
        imp.press.set((x, y));
        let v = imp.view.get();
        let rows = self.rows();
        let clips = self.project_clips();
        let additive =
            state.intersects(gdk::ModifierType::SHIFT_MASK | gdk::ModifierType::CONTROL_MASK);
        let copy = state.intersects(gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::ALT_MASK);
        let drag = match tl::hit(&v, &rows, &clips, x, y) {
            Hit::Ruler { tick } => {
                self.seek(tick);
                Some(Drag::Seek)
            }
            Hit::Loop { tick } => {
                let t = tl::snap_floor(tick, tl::snap_unit(v.px_per_tick, self.bar_ticks()));
                Some(Drag::Loop {
                    anchor: t,
                    end: t,
                    moved: false,
                })
            }
            Hit::Clip { clip, part, .. } => {
                let mut sel = self.selected();
                if additive && !copy {
                    if let Some(i) = sel.iter().position(|c| *c == clip) {
                        sel.remove(i);
                    } else {
                        sel.push(clip);
                    }
                    self.set_selected(sel);
                    None
                } else {
                    if !sel.contains(&clip) {
                        sel = vec![clip];
                    } else {
                        // The pressed clip comes first: the editor shows it.
                        sel.retain(|c| *c != clip);
                        sel.insert(0, clip);
                    }
                    self.set_selected(sel.clone());
                    let press_tick = v.x_to_tick(x);
                    Some(match part {
                        Part::Body => Drag::Move {
                            ids: sel,
                            press_tick,
                            dt: 0,
                            copy,
                        },
                        Part::Start | Part::End => Drag::Resize {
                            ids: sel,
                            from_start: part == Part::Start,
                            press_tick,
                            dlen: 0,
                        },
                    })
                }
            }
            Hit::Lane { row, tick } => {
                // One click adds a one-bar clip (undoable).
                if let Some(id) = clip_ops::add_at(&self.app(), rows[row], tick) {
                    *imp.selected.borrow_mut() = vec![id];
                }
                None
            }
            Hit::Below { .. } => {
                self.set_selected(Vec::new());
                None
            }
        };
        *imp.drag.borrow_mut() = drag;
        self.queue_draw();
    }

    fn drag_to(&self, x: f64, _y: f64) {
        let imp = self.imp();
        let v = imp.view.get();
        let unit = tl::snap_unit(v.px_per_tick, self.bar_ticks());
        let clips = self.project_clips();
        let mut d = imp.drag.borrow_mut();
        match d.as_mut() {
            Some(Drag::Move {
                ids,
                press_tick,
                dt,
                ..
            }) => {
                let want = tl::snap_round((v.x_to_tick(x) - *press_tick) as i64, unit);
                if tl::fits_move(&clips, ids, want) || !ids.is_empty() {
                    *dt = want;
                }
            }
            Some(Drag::Resize {
                ids,
                from_start,
                press_tick,
                dlen,
            }) => {
                let delta = tl::snap_round((v.x_to_tick(x) - *press_tick) as i64, unit);
                let want = if *from_start { -delta } else { delta };
                if tl::fits_resize(&clips, ids, want, *from_start) {
                    *dlen = want;
                }
            }
            Some(Drag::Loop { anchor, end, moved }) => {
                *end = tl::snap_round(v.x_to_tick(x) as i64, unit).max(0) as u32;
                *moved = *end != *anchor;
            }
            Some(Drag::Seek) => {
                drop(d);
                self.seek(v.x_to_tick(x) as u32);
                return;
            }
            None => {}
        }
        drop(d);
        self.queue_draw();
    }

    fn release(&self, dragged: bool) {
        let imp = self.imp();
        let d = imp.drag.borrow_mut().take();
        let app = self.app();
        let clips = self.project_clips();
        match d {
            Some(Drag::Move { ids, dt, copy, .. }) if dragged && dt != 0 => {
                if copy {
                    let ok = app
                        .edit(vec![Edit::DuplicateClips {
                            clips: ids,
                            dt,
                            linked: true,
                        }])
                        .is_some();
                    if !ok {
                        app.toast("The copy would overlap another clip");
                    }
                } else if tl::fits_move(&clips, &ids, dt) {
                    app.edit(vec![Edit::MoveClips { clips: ids, dt }]);
                } else {
                    app.toast("Clips cannot overlap on a row");
                }
            }
            Some(Drag::Resize {
                ids,
                from_start,
                dlen,
                ..
            }) if dlen != 0 => {
                app.edit(vec![Edit::ResizeClips {
                    clips: ids,
                    dlen,
                    from_start,
                }]);
            }
            Some(Drag::Loop { anchor, end, moved }) => {
                let lr = app.session.borrow().document().project.loop_region;
                let e = if moved {
                    let (a, b) = (anchor.min(end), anchor.max(end));
                    Edit::SetLoopRegion {
                        start: a,
                        end: b,
                        enabled: true,
                    }
                } else if lr.end > lr.start {
                    // A click turns the loop on or off.
                    Edit::SetLoopRegion {
                        start: lr.start,
                        end: lr.end,
                        enabled: !lr.enabled,
                    }
                } else {
                    return self.queue_draw();
                };
                app.edit(vec![e]);
            }
            _ => {}
        }
        self.queue_draw();
    }

    fn double_click(&self, x: f64, y: f64) {
        let v = self.imp().view.get();
        if let Hit::Clip { clip, .. } = tl::hit(&v, &self.rows(), &self.project_clips(), x, y) {
            self.imp().drag.borrow_mut().take();
            clip_ops::perform(&self.app(), &[clip], "edit");
        }
    }

    fn seek(&self, tick: u32) {
        let _ = self
            .app()
            .session
            .borrow_mut()
            .link
            .command(EngineCommand::Seek { tick: tick as u64 });
    }

    fn scroll(&self, dx: f64, dy: f64, st: gdk::ModifierType) {
        let ctrl = st.contains(gdk::ModifierType::CONTROL_MASK);
        let shift = st.contains(gdk::ModifierType::SHIFT_MASK);
        if ctrl {
            self.zoom_x(1.15f64.powf(-dy));
            return;
        }
        let mut v = self.imp().view.get();
        if shift {
            v.scroll_x += (dy + dx) * 60.0;
        } else {
            v.scroll_y += dy * v.row_h;
            v.scroll_x += dx * 60.0;
        }
        self.imp().view.set(v);
        self.sync_adjustments();
        self.queue_draw();
    }

    /// Keyboard (INTERACTIONS.md): only the combinations listed there.
    fn key(&self, key: gdk::Key, st: gdk::ModifierType) -> bool {
        let app = self.app();
        let sel = self.selected();
        let plain = crate::keys::plain(st);
        let ctrl = crate::keys::only(st, gdk::ModifierType::CONTROL_MASK);
        match key {
            gdk::Key::Delete | gdk::Key::BackSpace if plain && !sel.is_empty() => {
                clip_ops::delete(&app, &sel);
                true
            }
            gdk::Key::Return | gdk::Key::KP_Enter if plain && !sel.is_empty() => {
                clip_ops::perform(&app, &sel, "edit");
                true
            }
            gdk::Key::d if ctrl && !sel.is_empty() => {
                let made = clip_ops::duplicate(&app, &sel);
                if !made.is_empty() {
                    self.set_selected(made);
                }
                true
            }
            gdk::Key::a if ctrl => {
                let all: Vec<ClipId> = self.project_clips().iter().map(|c| c.id).collect();
                self.set_selected(all);
                true
            }
            gdk::Key::s | gdk::Key::S if plain && !sel.is_empty() => {
                clip_ops::perform(&app, &sel, "split");
                true
            }
            gdk::Key::_0 | gdk::Key::KP_0 if plain && !sel.is_empty() => {
                clip_ops::toggle_mute(&app, &sel);
                true
            }
            gdk::Key::Escape if plain && !sel.is_empty() => {
                self.set_selected(Vec::new());
                true
            }
            gdk::Key::Left | gdk::Key::Right | gdk::Key::Up | gdk::Key::Down if plain => {
                self.move_selection(key);
                true
            }
            _ => false,
        }
    }

    /// Arrows: the next clip on the row (Left, Right), or the nearest clip
    /// on the row above or below (Up, Down).
    fn move_selection(&self, key: gdk::Key) {
        let rows = self.rows();
        let clips = self.project_clips();
        let cur = self
            .selected()
            .first()
            .and_then(|id| clips.iter().find(|c| c.id == *id))
            .copied();
        let next = match cur {
            None => clips
                .iter()
                .min_by_key(|c| (rows.iter().position(|r| *r == c.instrument), c.start))
                .copied(),
            Some(c) => {
                let row = rows.iter().position(|r| *r == c.instrument).unwrap_or(0);
                match key {
                    gdk::Key::Left => clips
                        .iter()
                        .filter(|x| x.instrument == c.instrument && x.start < c.start)
                        .max_by_key(|x| x.start)
                        .copied(),
                    gdk::Key::Right => clips
                        .iter()
                        .filter(|x| x.instrument == c.instrument && x.start > c.start)
                        .min_by_key(|x| x.start)
                        .copied(),
                    _ => {
                        let target = if key == gdk::Key::Up {
                            row.checked_sub(1)
                        } else {
                            Some(row + 1)
                        };
                        target.and_then(|r| rows.get(r)).and_then(|inst| {
                            clips
                                .iter()
                                .filter(|x| x.instrument == *inst)
                                .min_by_key(|x| (x.start as i64 - c.start as i64).abs())
                                .copied()
                        })
                    }
                }
            }
        };
        if let Some(n) = next {
            let mut v = self.imp().view.get();
            v.reveal_tick(n.start as f64);
            self.imp().view.set(v);
            self.sync_adjustments();
            self.set_selected(vec![n.id]);
        }
    }

    /// The clip menu (`menus::clip_menu`) for the selected clips.
    fn install_menu(&self) {
        let group = gio::SimpleActionGroup::new();
        for name in crate::menus::CLIP_ACTIONS {
            let a = gio::SimpleAction::new(name, None);
            let (w, n) = (self.downgrade(), *name);
            a.connect_activate(move |_, _| {
                if let Some(o) = w.upgrade() {
                    clip_ops::perform(&o.app(), &o.selected(), n);
                }
            });
            group.add_action(&a);
        }
        self.insert_action_group("clip", Some(&group));
        let w = self.downgrade();
        crate::context_menu::attach_at(self, &crate::menus::clip_menu(), move |at| {
            let o = w.upgrade()?;
            o.prepare_menu(at)
        });
    }

    fn prepare_menu(&self, at: Option<(f64, f64)>) -> Option<crate::context_menu::Anchor> {
        use crate::context_menu::Anchor;
        self.grab_focus();
        let v = self.imp().view.get();
        let clips = self.project_clips();
        match at {
            Some((x, y)) => match tl::hit(&v, &self.rows(), &clips, x, y) {
                Hit::Clip { clip, .. } => {
                    if !self.selected().contains(&clip) {
                        self.set_selected(vec![clip]);
                    }
                    Some(Anchor::Pointer(x, y))
                }
                _ => None,
            },
            None => {
                let rows = self.rows();
                let c = self
                    .selected()
                    .first()
                    .and_then(|id| clips.iter().find(|c| c.id == *id))
                    .copied()?;
                let row = rows.iter().position(|r| *r == c.instrument)?;
                let x = v.tick_to_x(c.start as f64).max(0.0);
                let w = (v.tick_to_x(c.end() as f64) - x).clamp(4.0, v.width);
                Some(Anchor::Rect(gdk::Rectangle::new(
                    x as i32,
                    v.row_y(row) as i32,
                    w as i32,
                    v.row_h as i32,
                )))
            }
        }
    }

    fn tick(&self) {
        let t = self.app().playhead_tick();
        let imp = self.imp();
        if imp.last_playhead.get() != t {
            imp.last_playhead.set(t);
            self.queue_draw();
        }
    }

    // ---- drawing ----

    fn static_key(&self) -> StaticKey {
        let app = self.app();
        let v = self.imp().view.get();
        StaticKey {
            revision: app.session.borrow().document().revision,
            view: (
                v.px_per_tick.to_bits(),
                v.scroll_x.to_bits(),
                v.scroll_y.to_bits(),
                v.row_h.to_bits(),
            ),
            size: (self.width(), self.height()),
            selected: self.selected(),
            palette_gen: palette::generation(),
        }
    }

    fn draw(&self, s: &gtk::Snapshot) {
        let key = self.static_key();
        {
            let mut cache = self.imp().cache.borrow_mut();
            cache.append(s, key, |rec| self.draw_static(rec));
        }
        self.draw_dynamic(s);
    }

    fn draw_static(&self, s: &gtk::Snapshot) {
        let pal = Palette::current();
        let colors = palette::colors();
        let v = self.imp().view.get();
        let (w, h) = (v.width, v.height);
        draw::fill(s, &pal.bg, 0.0, 0.0, w, h);
        let app = self.app();
        let sess = app.session.borrow();
        let p = &sess.document().project;
        let bar = ticks_per_bar(p.time_sig_num);
        let rows = p.channels.len();
        let lanes_bottom = h;

        // Each instrument's lane is a soft rounded band: rows read without
        // lines, and the end of the arrangement is visible.
        s.push_clip(&draw::rect(0.0, tl::RULER_H, w, (h - tl::RULER_H).max(0.0)));
        let first_bar = (v.x_to_tick(0.0) as u32) / bar;
        let last_bar = (v.x_to_tick(w) as u32) / bar + 1;
        let mut lane = pal.fg;
        lane.set_alpha(0.035);
        let x_end = v.tick_to_x(self.end_tick() as f64).min(w);
        for row in 0..rows {
            let y = v.row_y(row);
            if y + v.row_h >= tl::RULER_H && y <= lanes_bottom {
                draw::rounded(
                    s,
                    &lane,
                    v.tick_to_x(0.0),
                    y + 2.0,
                    x_end - v.tick_to_x(0.0),
                    v.row_h - 4.0,
                    8.0,
                );
            }
        }
        // The loop region tints the lanes while it is on.
        let lr = p.loop_region;
        if lr.enabled && lr.end > lr.start {
            let mut c = colors.get(Role::Accent);
            c.set_alpha(0.06);
            let x0 = v.tick_to_x(lr.start as f64);
            let x1 = v.tick_to_x(lr.end as f64);
            draw::fill(s, &c, x0, tl::RULER_H, x1 - x0, lanes_bottom - tl::RULER_H);
        }

        // Clips.
        let sel = self.selected();
        let mut text = self.imp().text.borrow_mut();
        for (row, ch) in p.channels.iter().enumerate() {
            let y = v.row_y(row);
            if y + v.row_h < tl::RULER_H || y > h {
                continue;
            }
            let body = colors.channel_color(row as u32);
            for c in p.clips.iter().filter(|c| c.instrument == ch.id) {
                let x0 = v.tick_to_x(c.start as f64);
                let x1 = v.tick_to_x(c.end() as f64);
                if x1 < 0.0 || x0 > w {
                    continue;
                }
                let (cy, chh) = (y + 3.0, v.row_h - 6.0);
                let cw = (x1 - x0 - 2.0).max(2.0);
                let mut fill = mix(&body, &pal.bg, 0.85);
                if c.muted || ch.mix.mute {
                    fill = mix(&fill, &pal.bg, 0.4);
                }
                draw::rounded(s, &fill, x0 + 1.0, cy, cw, chh, 6.0);
                let ink = palette::readable_on(&fill);
                // A preview of the notes, repeated where the clip loops.
                if let Some(pat) = p.pattern(c.pattern)
                    && !pat.notes.is_empty()
                {
                    let (lo, hi) = pat
                        .notes
                        .iter()
                        .fold((127u8, 0u8), |(l, h), n| (l.min(n.key), h.max(n.key)));
                    let span = (hi - lo) as f64 + 1.0;
                    let top = cy + 18.0;
                    let ph = (chh - 22.0).max(4.0);
                    let mut nc = ink;
                    nc.set_alpha(0.55);
                    s.push_clip(&draw::rect(x0 + 1.0, cy, cw, chh));
                    for rep in tl::repeats(c.len, pat.length_ticks(), c.offset) {
                        for n in &pat.notes {
                            let t = rep + n.start as i64;
                            if t < 0 || t >= c.len as i64 {
                                continue;
                            }
                            let nx = v.tick_to_x((c.start as i64 + t) as f64);
                            let nw = (n.len as f64 * v.px_per_tick).max(2.0);
                            let ny = top + ph * (1.0 - ((n.key - lo) as f64 + 0.5) / span);
                            draw::fill(s, &nc, nx, ny - 1.0, nw, 2.0);
                        }
                    }
                    s.pop();
                }
                // Name (the content's), and a mark when it is a linked copy.
                if cw > 24.0 {
                    let name = p
                        .pattern(c.pattern)
                        .map(|pt| pt.name.clone())
                        .unwrap_or_default();
                    let linked = crate::selection::linked_count(p, c.pattern) > 1;
                    let label = if c.muted {
                        format!("{name} (muted)")
                    } else {
                        name
                    };
                    let l = text.get(self, &label, false);
                    s.push_clip(&draw::rect(x0 + 1.0, cy, cw - 6.0, chh));
                    draw::layout_at(s, &l, &ink, x0 + 7.0, cy + 1.0);
                    s.pop();
                    if linked && cw > 40.0 {
                        // Two small overlapping squares: "linked copy".
                        let (lx, ly) = (x0 + cw - 13.0, cy + 5.0);
                        draw::outline(s, &ink, lx, ly, 7.0, 7.0, 1.0);
                        draw::outline(s, &ink, lx + 3.0, ly + 3.0, 7.0, 7.0, 1.0);
                    }
                }
                if sel.contains(&c.id) {
                    let ring = if colors.high_contrast {
                        colors.get(Role::WindowFg)
                    } else {
                        colors.get(Role::Accent)
                    };
                    s.push_rounded_clip(&gsk::RoundedRect::from_rect(
                        draw::rect(x0 - 1.0, cy - 2.0, cw + 4.0, chh + 4.0),
                        8.0,
                    ));
                    draw::outline(s, &ring, x0 - 1.0, cy - 2.0, cw + 4.0, chh + 4.0, 2.0);
                    s.pop();
                }
            }
        }
        s.pop();

        // Ruler: bar numbers, dimmed, no hairline; the loop strip under them.
        draw::fill(s, &pal.bg, 0.0, 0.0, w, tl::RULER_H);
        let every = tl::bar_label_every(&v, bar, 34.0);
        for b in first_bar..=last_bar {
            if b % every != 0 {
                continue;
            }
            let x = v.tick_to_x((b * bar) as f64);
            let l = text.get(self, &format!("{}", b + 1), false);
            draw::layout_at(s, &l, &pal.text_dim, x + 3.0, 2.0);
        }
        if lr.end > lr.start {
            let x0 = v.tick_to_x(lr.start as f64);
            let x1 = v.tick_to_x(lr.end as f64);
            let c = if lr.enabled {
                let mut c = colors.get(Role::Accent);
                c.set_alpha(0.55);
                c
            } else {
                mix(&pal.fg, &pal.bg, 0.18)
            };
            draw::rounded(
                s,
                &c,
                x0,
                tl::RULER_H - tl::LOOP_H + 2.0,
                x1 - x0,
                tl::LOOP_H - 4.0,
                3.0,
            );
        }
    }

    /// Playhead and drag ghosts.
    fn draw_dynamic(&self, s: &gtk::Snapshot) {
        let imp = self.imp();
        let v = imp.view.get();
        let colors = palette::colors();
        let accent = colors.get(Role::Accent);
        let rows = self.rows();
        let clips = self.project_clips();
        match imp.drag.borrow().as_ref() {
            Some(Drag::Move { ids, dt, copy, .. }) if *dt != 0 => {
                let ok = *copy || tl::fits_move(&clips, ids, *dt);
                let c = if ok { accent } else { colors.get(Role::Error) };
                for cl in clips.iter().filter(|c| ids.contains(&c.id)) {
                    let Some(row) = rows.iter().position(|r| *r == cl.instrument) else {
                        continue;
                    };
                    let x0 = v.tick_to_x((cl.start as i64 + dt) as f64);
                    let x1 = v.tick_to_x((cl.end() as i64 + dt) as f64);
                    draw::outline(s, &c, x0, v.row_y(row) + 3.0, x1 - x0, v.row_h - 6.0, 2.0);
                }
            }
            Some(Drag::Resize {
                ids,
                from_start,
                dlen,
                ..
            }) if *dlen != 0 => {
                for cl in clips.iter().filter(|c| ids.contains(&c.id)) {
                    let Some(row) = rows.iter().position(|r| *r == cl.instrument) else {
                        continue;
                    };
                    let (a, b) = if *from_start {
                        (cl.start as i64 - dlen, cl.end() as i64)
                    } else {
                        (cl.start as i64, cl.end() as i64 + dlen)
                    };
                    let x0 = v.tick_to_x(a as f64);
                    let x1 = v.tick_to_x(b as f64);
                    draw::outline(
                        s,
                        &accent,
                        x0,
                        v.row_y(row) + 3.0,
                        x1 - x0,
                        v.row_h - 6.0,
                        2.0,
                    );
                }
            }
            Some(Drag::Loop { anchor, end, moved }) if *moved => {
                let (a, b) = (*anchor.min(end), *anchor.max(end));
                let x0 = v.tick_to_x(a as f64);
                let x1 = v.tick_to_x(b as f64);
                let mut c = accent;
                c.set_alpha(0.35);
                draw::rounded(
                    s,
                    &c,
                    x0,
                    tl::RULER_H - tl::LOOP_H + 2.0,
                    x1 - x0,
                    tl::LOOP_H - 4.0,
                    3.0,
                );
            }
            _ => {}
        }
        // Playhead: a 2 px accent line with a small cap in the ruler.
        let x = v.tick_to_x(imp.last_playhead.get() as f64);
        if (0.0..=v.width).contains(&x) {
            draw::fill(s, &accent, x - 1.0, tl::RULER_H - tl::LOOP_H, 2.0, v.height);
            draw::rounded(s, &accent, x - 5.0, 0.0, 10.0, 6.0, 3.0);
        }
    }
}
