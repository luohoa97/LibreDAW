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
//! loop, click it to turn the loop on or off. Audio clips (SPEC 21.1) trim
//! at their ends (the start moves the offset with it), fade at the corner
//! handles, and have no editor; Shift turns grid snapping off. Dropping a
//! sound from the file manager or the Sounds pane makes an audio clip. The
//! Patterns lane under the loop strip shows each pattern as one block
//! that moves, copies and deletes its member clips together (SPEC 20.7).
//! Shape lanes under a row draw an automation curve with points to drag
//! (SPEC 24.2-1). Keys: arrows choose clips, Return edits, Delete removes,
//! Ctrl+D duplicates, Ctrl+G makes a pattern, S splits at the playhead, 0
//! mutes, Ctrl+A selects all, Escape deselects. The menus are
//! `menus::clip_menu`, `menus::pattern_menu` and the shape menus.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, gio, gsk};

use protocol::edit::Edit;
use protocol::engine::EngineCommand;
use protocol::ids::{ChannelId, ClipId, GroupId, ShapeId};
use protocol::model::{Clip, Instrument, ShapePoint, ticks_per_bar};

use crate::app::App;
use crate::audio_clips;
use crate::clip_ops;
use crate::draw::{self, Palette, mix};
use crate::palette::{self, Role};
use crate::pattern_logic::{self, Block};
use crate::render_cache::{LayerCache, LayoutCache};
use crate::shape_logic;
use crate::timeline_logic::{self as tl, Hit, Part, Row, View};

/// Room above and below a shape's curve in its lane, and the size of its
/// points.
const LANE_PAD: f64 = 8.0;
const POINT_R: f64 = 5.0;

/// What the static layer depends on.
#[derive(Clone, PartialEq)]
pub struct StaticKey {
    revision: u64,
    view: (u64, u64, u64, u64),
    size: (i32, i32),
    selected: Vec<ClipId>,
    palette_gen: u64,
    peaks: u64,
    block: Option<(GroupId, u32)>,
    shape: Option<ShapeId>,
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
    /// A fade handle of an audio clip: the fades as they would be.
    Fade {
        clip: ClipId,
        fade_in: bool,
        fades: (u32, u32),
    },
    /// A pattern block dragged along the lane.
    Block {
        block: Block,
        press_tick: f64,
        dt: i64,
    },
    /// A point of a shape: the points as they would be.
    Point {
        shape: ShapeId,
        row: usize,
        index: usize,
        range: (f32, f32),
        orig: Vec<ShapePoint>,
        points: Vec<ShapePoint>,
    },
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
        /// The clips with a waveform still to come.
        pub waiting_peaks: Cell<bool>,
        /// The pattern block chosen in the lane.
        pub block_sel: RefCell<Option<(GroupId, u32)>>,
        /// The shape lane last clicked.
        pub shape_sel: Cell<Option<ShapeId>>,
        /// What the open pattern or shape menu is about.
        pub menu_block: RefCell<Option<Block>>,
        pub menu_shape: Cell<Option<(ShapeId, u32, Option<usize>)>>,
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
                waiting_peaks: Cell::new(false),
                block_sel: RefCell::new(None),
                shape_sel: Cell::new(None),
                menu_block: RefCell::new(None),
                menu_shape: Cell::new(None),
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
            drag.connect_drag_update(move |g, dx, dy| {
                if let Some(o) = w.upgrade() {
                    let (sx, sy) = o.imp().press.get();
                    o.drag_to(sx + dx, sy + dy, g.current_event_state());
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
                let text = o.tip_at(x as f64, y as f64);
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

/// The tooltip for what the pointer is over. `audio` is whether the hit
/// clip, or the row, plays audio.
pub fn tooltip_at(hit: &Hit, audio: bool) -> &'static str {
    match hit {
        Hit::Ruler { .. } => {
            "Bars: each number is one bar of music, four beats long; click to move the playhead"
        }
        Hit::Loop { .. } => {
            "Loop: drag here to choose the part that repeats; click to turn looping on or off"
        }
        Hit::Pattern { .. } => pattern_logic::TOOLTIP,
        Hit::Shape { .. } => shape_logic::TOOLTIP,
        Hit::Clip {
            part: Part::FadeIn, ..
        } => "Drag to make the sound start softly",
        Hit::Clip {
            part: Part::FadeOut,
            ..
        } => "Drag to make the sound end softly",
        Hit::Clip { .. } if audio => {
            "A sound: drag to move it, drag its ends to trim it, drag a corner dot to fade it"
        }
        Hit::Clip { .. } => {
            "A clip: a piece of music on a row; drag to move it, double-click to change its notes"
        }
        Hit::Lane { .. } if audio => "Drop a sound here to add it to this row",
        Hit::Lane { .. } => "Click to add a one-bar clip here",
        Hit::Below { .. } => "Add an instrument to get a new row, or drop a sound here",
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
        crate::presence_ui::watch_redraw(&o);
        *o.imp().app.borrow_mut() = Some(app);
        o.apply_size_class();
        o.install_menu();
        o.install_drop();
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

    /// The displayed rows: each instrument, then the lanes of its shapes.
    fn rows(&self) -> Vec<Row> {
        let app = self.app();
        let s = app.session.borrow();
        shape_logic::layout(&s.document().project)
    }

    fn blocks(&self) -> Vec<Block> {
        pattern_logic::blocks(&self.app().session.borrow().document().project)
    }

    /// Whether the instrument is an Audio row.
    fn is_audio_row(&self, ch: ChannelId) -> bool {
        let app = self.app();
        let s = app.session.borrow();
        s.document()
            .project
            .channel(ch)
            .is_some_and(|c| matches!(c.instrument, Instrument::Audio))
    }

    /// The tooltip at a point.
    fn tip_at(&self, x: f64, y: f64) -> &'static str {
        let rows = self.rows();
        let clips = self.project_clips();
        let hit = tl::hit(&self.imp().view.get(), &rows, &clips, x, y);
        let audio = match hit {
            Hit::Clip { clip, .. } => clips.iter().any(|c| c.id == clip && c.audio.is_some()),
            Hit::Lane { row, .. } => match rows[row] {
                Row::Instrument(ch) => self.is_audio_row(ch),
                Row::Shape(_) => false,
            },
            _ => false,
        };
        tooltip_at(&hit, audio)
    }

    /// The length in ticks of the sound an audio clip plays, once its
    /// waveform is known.
    fn sample_ticks(&self, c: &Clip) -> Option<u32> {
        let a = c.audio?;
        let app = self.app();
        let bpm = app.session.borrow().document().project.tempo_bpm;
        app.wave_peaks.peek(&a.sample).map(|p| p.ticks(bpm))
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
            Hit::Clip {
                part: Part::FadeIn | Part::FadeOut,
                ..
            } => Some("pointer"),
            _ => None,
        };
        self.set_cursor_from_name(name);
    }

    /// The grid unit under the pointer's modifiers: Shift snaps to nothing.
    fn unit(&self, st: gdk::ModifierType) -> u32 {
        if st.contains(gdk::ModifierType::SHIFT_MASK) {
            1
        } else {
            tl::snap_unit(self.imp().view.get().px_per_tick, self.bar_ticks())
        }
    }

    /// The points of a shape and the range of its target.
    fn shape_info(&self, id: ShapeId) -> Option<(Vec<ShapePoint>, (f32, f32))> {
        let app = self.app();
        let s = app.session.borrow();
        let p = &s.document().project;
        let sh = p.shapes.iter().find(|x| x.id == id)?;
        Some((sh.points.clone(), shape_logic::range(p, &sh.target)))
    }

    fn lane_xy(&self, row: usize, range: (f32, f32)) -> impl Fn(&ShapePoint) -> (f64, f64) {
        let v = self.imp().view.get();
        move |p| {
            (
                v.tick_to_x(p.tick as f64),
                shape_logic::value_y(p.value, range, v.row_y(row), v.row_h, LANE_PAD),
            )
        }
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
        let hit = tl::hit(&v, &rows, &clips, x, y);
        if !matches!(hit, Hit::Pattern { .. }) {
            *imp.block_sel.borrow_mut() = None;
        }
        if !matches!(hit, Hit::Shape { .. }) {
            imp.shape_sel.set(None);
        }
        let drag = match hit {
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
            Hit::Pattern { tick } => {
                let blocks = self.blocks();
                match pattern_logic::block_at(&blocks, tick) {
                    Some(b) => {
                        self.set_selected(Vec::new());
                        *imp.block_sel.borrow_mut() = Some((b.group, b.instance));
                        Some(Drag::Block {
                            block: b.clone(),
                            press_tick: v.x_to_tick(x),
                            dt: 0,
                        })
                    }
                    None => None,
                }
            }
            Hit::Shape { row, shape, .. } => {
                imp.shape_sel.set(Some(shape));
                self.set_selected(Vec::new());
                self.shape_info(shape).and_then(|(points, range)| {
                    shape_logic::nearest(&points, self.lane_xy(row, range), x, y, POINT_R + 3.0)
                        .map(|index| Drag::Point {
                            shape,
                            row,
                            index,
                            range,
                            orig: points.clone(),
                            points,
                        })
                })
            }
            Hit::Clip { clip, part, .. } => {
                let mut sel = self.selected();
                let fade_part = matches!(part, Part::FadeIn | Part::FadeOut);
                if additive && !copy && !fade_part {
                    if let Some(i) = sel.iter().position(|c| *c == clip) {
                        sel.remove(i);
                    } else {
                        sel.push(clip);
                    }
                    self.set_selected(sel);
                    None
                } else {
                    if !sel.contains(&clip) || fade_part {
                        sel = vec![clip];
                    } else {
                        // The pressed clip comes first: the editor shows it.
                        sel.retain(|c| *c != clip);
                        sel.insert(0, clip);
                    }
                    self.set_selected(sel.clone());
                    let press_tick = v.x_to_tick(x);
                    let c = clips.iter().find(|c| c.id == clip).copied();
                    match part {
                        Part::Body => Some(Drag::Move {
                            ids: sel,
                            press_tick,
                            dt: 0,
                            copy,
                        }),
                        Part::Start | Part::End => Some(Drag::Resize {
                            ids: sel,
                            from_start: part == Part::Start,
                            press_tick,
                            dlen: 0,
                        }),
                        Part::FadeIn | Part::FadeOut => {
                            c.and_then(|c| c.audio).map(|a| Drag::Fade {
                                clip,
                                fade_in: part == Part::FadeIn,
                                fades: (a.fade_in, a.fade_out),
                            })
                        }
                    }
                }
            }
            Hit::Lane { row, tick } => {
                let Row::Instrument(ch) = rows[row] else {
                    return;
                };
                if self.is_audio_row(ch) {
                    // Sounds come by dropping them: nothing to add by hand.
                    self.set_selected(Vec::new());
                    self.app().select_channel(ch);
                } else if let Some(id) = clip_ops::add_at(&self.app(), ch, tick) {
                    // One click adds a one-bar clip (undoable).
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

    fn drag_to(&self, x: f64, y: f64, st: gdk::ModifierType) {
        let imp = self.imp();
        let v = imp.view.get();
        let unit = self.unit(st);
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
            Some(Drag::Block { press_tick, dt, .. }) => {
                *dt = tl::snap_round((v.x_to_tick(x) - *press_tick) as i64, unit);
            }
            Some(Drag::Resize {
                ids,
                from_start,
                press_tick,
                dlen,
            }) => {
                let delta = tl::snap_round((v.x_to_tick(x) - *press_tick) as i64, unit);
                let want = if *from_start { -delta } else { delta };
                let inside = clips
                    .iter()
                    .filter(|c| ids.contains(&c.id) && c.audio.is_some())
                    .all(|c| audio_clips::trim_ok(c, want, *from_start, self.sample_ticks(c)));
                if tl::fits_resize(&clips, ids, want, *from_start) && inside {
                    *dlen = want;
                }
            }
            Some(Drag::Fade {
                clip,
                fade_in,
                fades,
            }) => {
                if let Some(c) = clips.iter().find(|c| c.id == *clip) {
                    let at = tl::snap_round(v.x_to_tick(x) as i64 - c.start as i64, unit);
                    let f = audio_clips::fade_from_drag(c.len, *fade_in, at);
                    *fades = if *fade_in {
                        audio_clips::fades_after(c.len, f, fades.1, true)
                    } else {
                        audio_clips::fades_after(c.len, fades.0, f, false)
                    };
                }
            }
            Some(Drag::Point {
                row,
                index,
                range,
                orig,
                points,
                ..
            }) => {
                let tick = tl::snap_round(v.x_to_tick(x) as i64, unit).max(0) as u32;
                let value = shape_logic::y_value(y, *range, v.row_y(*row), v.row_h, LANE_PAD);
                *points = shape_logic::moved(orig, *index, tick, value);
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
            Some(Drag::Block { block, dt, .. }) if dragged && dt != 0 => {
                if pattern_logic::fits_move(&clips, &block, dt) {
                    app.edit(vec![pattern_logic::move_edit(&block, dt)]);
                } else {
                    app.toast("A pattern cannot overlap other clips on a row");
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
            Some(Drag::Fade {
                clip,
                fades: (fi, fo),
                ..
            }) if dragged => {
                if let Some(a) = clips.iter().find(|c| c.id == clip).and_then(|c| c.audio)
                    && (a.fade_in, a.fade_out) != (fi, fo)
                {
                    app.edit(vec![Edit::SetClipAudio {
                        clip,
                        gain_mdb: a.gain_mdb,
                        fade_in: fi,
                        fade_out: fo,
                    }]);
                }
            }
            Some(Drag::Point {
                shape,
                orig,
                points,
                ..
            }) if dragged && points != orig => {
                app.edit(vec![Edit::SetShapePoints { shape, points }]);
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
        let rows = self.rows();
        let clips = self.project_clips();
        match tl::hit(&v, &rows, &clips, x, y) {
            Hit::Clip { clip, .. } => {
                self.imp().drag.borrow_mut().take();
                // An audio clip has no notes to open.
                clip_ops::perform(&self.app(), &[clip], "edit");
            }
            Hit::Pattern { tick } => {
                self.imp().drag.borrow_mut().take();
                if let Some(b) = pattern_logic::block_at(&self.blocks(), tick).cloned() {
                    self.open_block(&b);
                }
            }
            Hit::Shape { row, shape, .. } => {
                // A point already there stays: the first click took hold of it.
                let imp = self.imp();
                imp.drag.borrow_mut().take();
                let Some((points, range)) = self.shape_info(shape) else {
                    return;
                };
                if shape_logic::nearest(&points, self.lane_xy(row, range), x, y, POINT_R + 3.0)
                    .is_some()
                {
                    return;
                }
                let tick = tl::snap_round(
                    v.x_to_tick(x) as i64,
                    tl::snap_unit(v.px_per_tick, self.bar_ticks()),
                )
                .max(0) as u32;
                let value = shape_logic::y_value(y, range, v.row_y(row), v.row_h, LANE_PAD);
                let (points, _) = shape_logic::with_point(&points, tick, value);
                self.app()
                    .edit(vec![Edit::SetShapePoints { shape, points }]);
            }
            _ => {}
        }
    }

    /// Double-click on a pattern block: the first member opens in the
    /// editor (it shows one clip at a time) and the others are named.
    fn open_block(&self, b: &Block) {
        let app = self.app();
        let note = {
            let s = app.session.borrow();
            pattern_logic::open_note(&s.document().project, b)
        };
        if let Some(first) = b.clips.first() {
            self.set_selected(vec![*first]);
            clip_ops::perform(&app, &[*first], "edit");
        }
        if b.clips.len() > 1 {
            app.toast(&note);
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

    /// The pattern block chosen in the lane, if it still exists.
    fn chosen_block(&self) -> Option<Block> {
        let (g, i) = (*self.imp().block_sel.borrow())?;
        self.blocks()
            .into_iter()
            .find(|b| b.group == g && b.instance == i)
    }

    /// Keyboard (INTERACTIONS.md): only the combinations listed there.
    fn key(&self, key: gdk::Key, st: gdk::ModifierType) -> bool {
        let app = self.app();
        let sel = self.selected();
        let plain = crate::keys::plain(st);
        let ctrl = crate::keys::only(st, gdk::ModifierType::CONTROL_MASK);
        let block = if sel.is_empty() {
            self.chosen_block()
        } else {
            None
        };
        match key {
            gdk::Key::Delete | gdk::Key::BackSpace if plain && block.is_some() => {
                if let Some(b) = block {
                    app.edit(vec![pattern_logic::delete_edit(&b)]);
                }
                true
            }
            gdk::Key::Delete | gdk::Key::BackSpace if plain && !sel.is_empty() => {
                clip_ops::delete(&app, &sel);
                true
            }
            gdk::Key::Return | gdk::Key::KP_Enter if plain && !sel.is_empty() => {
                clip_ops::perform(&app, &sel, "edit");
                true
            }
            gdk::Key::d if ctrl && block.is_some() => {
                if let Some(b) = block {
                    match pattern_logic::duplicate_edit(&self.project_clips(), &b) {
                        Some(e) => {
                            app.edit(vec![e]);
                        }
                        None => app.toast("No room after this pattern for a copy"),
                    }
                }
                true
            }
            gdk::Key::d if ctrl && !sel.is_empty() => {
                let made = clip_ops::duplicate(&app, &sel);
                if !made.is_empty() {
                    self.set_selected(made);
                }
                true
            }
            gdk::Key::g if ctrl => {
                clip_ops::make_pattern(&app, &sel);
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
            gdk::Key::Escape if plain && (!sel.is_empty() || block.is_some()) => {
                *self.imp().block_sel.borrow_mut() = None;
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
        let rows: Vec<ChannelId> = {
            let app = self.app();
            let s = app.session.borrow();
            s.document().project.channels.iter().map(|c| c.id).collect()
        };
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

    /// The menus: the clip menu for clips (`menus::clip_menu`), the pattern
    /// menu for a block in the lane, and the shape menus for a lane or one
    /// of its points. Each opens only where it applies.
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

        let group = gio::SimpleActionGroup::new();
        for name in crate::menus::PATTERN_ACTIONS {
            let a = gio::SimpleAction::new(name, None);
            let (w, n) = (self.downgrade(), *name);
            a.connect_activate(move |_, _| {
                if let Some(o) = w.upgrade() {
                    o.pattern_action(n);
                }
            });
            group.add_action(&a);
        }
        self.insert_action_group("pattern", Some(&group));
        let w = self.downgrade();
        crate::context_menu::attach_at(self, &crate::menus::pattern_menu(), move |at| {
            let o = w.upgrade()?;
            let (x, y) = at?;
            let hit = tl::hit(&o.imp().view.get(), &o.rows(), &o.project_clips(), x, y);
            let Hit::Pattern { tick } = hit else {
                return None;
            };
            let b = pattern_logic::block_at(&o.blocks(), tick).cloned()?;
            o.grab_focus();
            *o.imp().block_sel.borrow_mut() = Some((b.group, b.instance));
            *o.imp().menu_block.borrow_mut() = Some(b);
            o.queue_draw();
            Some(crate::context_menu::Anchor::Pointer(x, y))
        });

        let group = gio::SimpleActionGroup::new();
        for name in crate::menus::SHAPE_ACTIONS {
            let a = if matches!(*name, "preset" | "curve") {
                gio::SimpleAction::new(name, Some(glib::VariantTy::INT32))
            } else {
                gio::SimpleAction::new(name, None)
            };
            let (w, n) = (self.downgrade(), *name);
            a.connect_activate(move |_, v| {
                if let Some(o) = w.upgrade() {
                    o.shape_action(n, v.and_then(|v| v.get::<i32>()));
                }
            });
            group.add_action(&a);
        }
        self.insert_action_group("shape", Some(&group));
        for (menu, on_point) in [
            (crate::menus::shape_lane_menu(), false),
            (crate::menus::shape_point_menu(), true),
        ] {
            let w = self.downgrade();
            crate::context_menu::attach_at(self, &menu, move |at| {
                w.upgrade()?.prepare_shape_menu(at?, on_point)
            });
        }
    }

    fn prepare_shape_menu(
        &self,
        at: (f64, f64),
        on_point: bool,
    ) -> Option<crate::context_menu::Anchor> {
        let (x, y) = at;
        let rows = self.rows();
        let Hit::Shape { row, shape, tick } =
            tl::hit(&self.imp().view.get(), &rows, &self.project_clips(), x, y)
        else {
            return None;
        };
        let (points, range) = self.shape_info(shape)?;
        let near = shape_logic::nearest(&points, self.lane_xy(row, range), x, y, POINT_R + 3.0);
        if near.is_some() != on_point {
            return None;
        }
        self.grab_focus();
        self.imp().shape_sel.set(Some(shape));
        self.imp().menu_shape.set(Some((shape, tick, near)));
        Some(crate::context_menu::Anchor::Pointer(x, y))
    }

    fn pattern_action(&self, name: &str) {
        let app = self.app();
        let Some(b) = self.imp().menu_block.borrow().clone() else {
            return;
        };
        match name {
            "duplicate" => match pattern_logic::duplicate_edit(&self.project_clips(), &b) {
                Some(e) => {
                    app.edit(vec![e]);
                }
                None => app.toast("No room after this pattern for a copy"),
            },
            "place" => {
                let at = app.playhead_tick().min(u32::MAX as u64) as u32;
                app.edit(vec![pattern_logic::place_edit(b.group, at)]);
            }
            "rename" => {
                let (a, g) = (app.clone(), b.group);
                crate::dialogs::ask_name(self, "Rename Pattern", &b.name, move |n| {
                    a.edit(vec![Edit::RenameGroup { group: g, name: n }]);
                });
            }
            "delete" => {
                app.edit(vec![pattern_logic::delete_edit(&b)]);
            }
            _ => {}
        }
    }

    fn shape_action(&self, name: &str, arg: Option<i32>) {
        let app = self.app();
        let Some((id, _tick, point)) = self.imp().menu_shape.get() else {
            return;
        };
        let playhead = app.playhead_tick().min(u32::MAX as u64) as u32;
        let edits = {
            let s = app.session.borrow();
            let p = &s.document().project;
            let Some(shape) = p.shapes.iter().find(|x| x.id == id) else {
                return;
            };
            match name {
                "preset" => {
                    let Some((_, preset)) = arg.and_then(|i| shape_logic::PRESETS.get(i as usize))
                    else {
                        return;
                    };
                    let (a, b) = shape_logic::span(p, None, playhead);
                    shape_logic::preset_edits(p, shape, *preset, a, b)
                }
                "curve" => {
                    let (Some(i), Some((_, curve))) =
                        (point, arg.and_then(|i| shape_logic::CURVES.get(i as usize)))
                    else {
                        return;
                    };
                    vec![Edit::SetShapePoints {
                        shape: id,
                        points: shape_logic::with_curve(&shape.points, i, *curve),
                    }]
                }
                "remove-point" => {
                    let Some(i) = point else { return };
                    vec![Edit::SetShapePoints {
                        shape: id,
                        points: shape_logic::without(&shape.points, i),
                    }]
                }
                "remove" => vec![Edit::RemoveShape { shape: id }],
                _ => return,
            }
        };
        app.edit(edits);
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
                let row = tl::row_of(&rows, c.instrument)?;
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

    // ---- dropping sounds (SPEC 21.1) ----

    /// Accepts audio files dropped on the timeline.
    fn install_drop(&self) {
        let target = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
        let w = self.downgrade();
        target.connect_drop(move |_, value, x, y| {
            let (Some(o), Ok(list)) = (w.upgrade(), value.get::<gdk::FileList>()) else {
                return false;
            };
            let paths: Vec<std::path::PathBuf> =
                list.files().iter().filter_map(|f| f.path()).collect();
            !paths.is_empty() && o.drop_files(paths, x, y)
        });
        self.add_controller(target);
    }

    /// A sound dropped at (`x`, `y`): on the empty area below the rows a new
    /// Audio row with the whole sound as one clip; on an Audio row a clip
    /// there; on an instrument row the sound goes into its sampler.
    pub fn drop_files(&self, paths: Vec<std::path::PathBuf>, x: f64, y: f64) -> bool {
        let app = self.app();
        let files: Vec<std::path::PathBuf> = paths
            .into_iter()
            .filter(|p| crate::samples_ui::is_audio_file(p))
            .collect();
        if files.is_empty() {
            app.toast("Only WAV, FLAC, Ogg, MP3 and WavPack files can be used as sounds");
            return true;
        }
        let v = self.imp().view.get();
        let rows = self.rows();
        let hit = tl::hit(&v, &rows, &self.project_clips(), x, y);
        let tick = tl::snap_floor(
            v.x_to_tick(x) as u32,
            tl::snap_unit(v.px_per_tick, self.bar_ticks()),
        );
        let row = match hit {
            Hit::Lane { row, .. } | Hit::Clip { row, .. } => Some(row),
            Hit::Shape { .. } => {
                app.toast("Drop the sound on an instrument's row, or below the rows");
                return true;
            }
            _ => None,
        };
        let channel = row.and_then(|r| match rows[r] {
            Row::Instrument(c) => Some(c),
            Row::Shape(_) => None,
        });
        if let Some(ch) = channel
            && !self.is_audio_row(ch)
        {
            // An instrument row: the current behaviour (SPEC 15.8.4).
            let sampler = matches!(
                app.session
                    .borrow()
                    .document()
                    .project
                    .channel(ch)
                    .map(|c| &c.instrument),
                Some(Instrument::Sampler(_))
            );
            match files.into_iter().next() {
                Some(f) if sampler => crate::samples_ui::set_channel_sample(
                    &app,
                    ch,
                    crate::samples_ui::ImportItem::file(f),
                ),
                Some(f) => crate::samples_ui::add_channels_from_files(&app, vec![f]),
                None => {}
            }
            return true;
        }
        let items = files
            .into_iter()
            .map(crate::samples_ui::ImportItem::file)
            .collect();
        let a = app.clone();
        audio_clips::import_and_probe(&app, items, move |results| {
            let bpm = a.session.borrow().document().project.tempo_bpm;
            let grouped = a.gesture_begin("Add audio");
            let (mut at, mut made) = (tick, 0usize);
            for r in results {
                match r {
                    Ok(p) => {
                        let to = match channel {
                            Some(channel) => audio_clips::DropAt::Row { channel, tick: at },
                            None => audio_clips::DropAt::NewRow { tick: at },
                        };
                        if audio_clips::add_dropped(&a, to, &p.sample, p.secs).is_some() {
                            made += 1;
                            if channel.is_some() {
                                at += audio_clips::full_len(p.secs, bpm);
                            }
                        }
                    }
                    Err(e) => a.toast(&e),
                }
            }
            if grouped {
                a.gesture_end();
            }
            if made > 0 {
                let a2 = a.clone();
                a.toast_action(
                    &if made == 1 {
                        "Added a sound".to_string()
                    } else {
                        format!("Added {made} sounds")
                    },
                    "Undo",
                    move || a2.undo(),
                );
            }
        });
        true
    }

    fn tick(&self) {
        let t = self.app().playhead_tick();
        let imp = self.imp();
        if imp.last_playhead.get() != t {
            imp.last_playhead.set(t);
            self.queue_draw();
        }
        if imp.waiting_peaks.get() {
            self.poll_peaks();
        }
    }

    /// Starts and collects the waveforms of the audio clips on the page.
    fn poll_peaks(&self) {
        let app = self.app();
        let mut hashes: Vec<protocol::model::SampleHash> = self
            .project_clips()
            .iter()
            .filter_map(|c| c.audio.map(|a| a.sample))
            .collect();
        hashes.sort();
        hashes.dedup();
        let all = hashes.iter().all(|h| app.wave_peaks.get(&app, h).is_some());
        if all {
            self.imp().waiting_peaks.set(false);
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
            peaks: app.wave_peaks.generation(),
            block: *self.imp().block_sel.borrow(),
            shape: self.imp().shape_sel.get(),
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
        let layout = shape_logic::layout(p);
        let lanes_bottom = h;
        let bpm = p.tempo_bpm;
        let blocks = pattern_logic::blocks(p);
        let mut waiting = false;

        // Each row's lane is a soft rounded band: rows read without lines,
        // and the end of the arrangement is visible. A shape's lane is
        // washed with the accent so it reads as belonging to the row above.
        s.push_clip(&draw::rect(0.0, tl::RULER_H, w, (h - tl::RULER_H).max(0.0)));
        let first_bar = (v.x_to_tick(0.0) as u32) / bar;
        let last_bar = (v.x_to_tick(w) as u32) / bar + 1;
        let mut lane = pal.fg;
        lane.set_alpha(0.035);
        let mut shape_lane = colors.get(Role::Accent);
        shape_lane.set_alpha(0.07);
        let x_end = v.tick_to_x(self.end_tick() as f64).min(w);
        for (row, kind) in layout.iter().enumerate() {
            let y = v.row_y(row);
            if y + v.row_h >= tl::RULER_H && y <= lanes_bottom {
                let c = if matches!(kind, Row::Shape(_)) {
                    &shape_lane
                } else {
                    &lane
                };
                draw::rounded(
                    s,
                    c,
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

        // Clips and shapes.
        let sel = self.selected();
        let mut text = self.imp().text.borrow_mut();
        for (row, kind) in layout.iter().enumerate() {
            let y = v.row_y(row);
            if y + v.row_h < tl::RULER_H || y > h {
                continue;
            }
            let ch = match kind {
                Row::Instrument(id) => match p.channel(*id) {
                    Some(c) => c,
                    None => continue,
                },
                Row::Shape(id) => {
                    if let Some(shape) = p.shapes.iter().find(|x| x.id == *id) {
                        let line = colors.get(Role::Accent);
                        let range = shape_logic::range(p, &shape.target);
                        draw_curve(s, &v, row, &shape.points, range, &line, true);
                        draw_points(s, &v, row, &shape.points, range, &line, &pal.bg);
                        let name = shape_logic::lane_name(p, shape);
                        let l = text.get(self, &name, false);
                        draw::layout_at(s, &l, &pal.text_dim, 8.0, y + 3.0);
                    }
                    continue;
                }
            };
            let ci = p.channels.iter().position(|c| c.id == ch.id).unwrap_or(0);
            let body = colors.channel_color(ci as u32);
            for c in p.clips.iter().filter(|c| c.instrument == ch.id) {
                let x0 = v.tick_to_x(c.start as f64);
                let x1 = v.tick_to_x(c.end() as f64);
                if x1 < 0.0 || x0 > w {
                    continue;
                }
                let (cy, chh) = (y + 3.0, v.row_h - 6.0);
                let cw = (x1 - x0 - 2.0).max(2.0);
                let mut fill = mix(&body, &pal.bg, 0.85);
                // A member of a pattern takes a tint of its colour.
                let tint = c
                    .group
                    .and_then(|g| p.groups.iter().find(|x| x.id == g.group))
                    .map(|g| rgba(g.color));
                if let Some(t) = &tint {
                    fill = mix(t, &fill, 0.28);
                }
                if c.muted || ch.mix.mute {
                    fill = mix(&fill, &pal.bg, 0.4);
                }
                draw::rounded(s, &fill, x0 + 1.0, cy, cw, chh, 6.0);
                if let Some(t) = &tint {
                    draw::rounded(s, t, x0 + 1.0, cy + 5.0, 3.0, chh - 10.0, 1.5);
                }
                let ink = palette::readable_on(&fill);
                if let Some(a) = &c.audio {
                    match app.wave_peaks.get(&app, &a.sample) {
                        Some(peaks) => {
                            draw_wave(s, &v, c, a, &peaks, bpm, x0, cw, cy, chh, w, &ink);
                        }
                        None => {
                            waiting = true;
                            let mut flat = ink;
                            flat.set_alpha(0.35);
                            draw::fill(
                                s,
                                &flat,
                                x0 + 4.0,
                                cy + chh / 2.0,
                                (cw - 8.0).max(0.0),
                                1.0,
                            );
                        }
                    }
                } else if let Some(pat) = p.pattern(c.pattern)
                    && !pat.notes.is_empty()
                {
                    // A preview of the notes, repeated where the clip loops.
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
                    let name = match &c.audio {
                        Some(a) => p
                            .samples
                            .iter()
                            .find(|x| x.hash == a.sample.to_hex())
                            .map(|x| audio_clips::row_name(&x.orig_name))
                            .unwrap_or_default(),
                        None => p
                            .pattern(c.pattern)
                            .map(|pt| pt.name.clone())
                            .unwrap_or_default(),
                    };
                    let linked =
                        c.audio.is_none() && crate::selection::linked_count(p, c.pattern) > 1;
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
                if let Some(a) = &c.audio
                    && cw > 3.0 * tl::EDGE_PX
                {
                    // The fade handles on the top edge.
                    for fade_in in [true, false] {
                        let hx = tl::handle_x(&v, c, fade_in);
                        let mut hc = ink;
                        hc.set_alpha(0.8);
                        let r = 4.0;
                        draw::rounded(
                            s,
                            &hc,
                            hx - r,
                            cy + tl::HANDLE_R + 1.0 - r,
                            2.0 * r,
                            2.0 * r,
                            r,
                        );
                    }
                    let _ = a;
                }
                if sel.contains(&c.id) {
                    let ring = if colors.high_contrast {
                        colors.get(Role::WindowFg)
                    } else {
                        colors.get(Role::Accent)
                    };
                    // A rounded ring just outside the clip.
                    s.append_border(
                        &gsk::RoundedRect::from_rect(
                            draw::rect(x0 - 1.0, cy - 2.0, cw + 4.0, chh + 4.0),
                            8.0,
                        ),
                        &[2.0; 4],
                        &[ring; 4],
                    );
                }
            }
        }
        s.pop();
        self.imp().waiting_peaks.set(waiting);

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
            draw::rounded(s, &c, x0, tl::LOOP_Y + 2.0, x1 - x0, tl::LOOP_H - 4.0, 3.0);
        }

        // The Patterns lane: one block per placed pattern.
        let mut band = pal.fg;
        band.set_alpha(0.05);
        draw::rounded(
            s,
            &band,
            0.0,
            tl::PATTERN_Y + 1.0,
            w,
            tl::PATTERN_H - 2.0,
            6.0,
        );
        let chosen = *self.imp().block_sel.borrow();
        s.push_clip(&draw::rect(0.0, tl::PATTERN_Y, w, tl::PATTERN_H));
        for b in &blocks {
            let x0 = v.tick_to_x(b.start as f64);
            let x1 = v.tick_to_x(b.end as f64);
            if x1 < 0.0 || x0 > w {
                continue;
            }
            let base = rgba(b.color);
            let fill = mix(&base, &pal.bg, 0.8);
            let bw = (x1 - x0 - 2.0).max(3.0);
            draw::rounded(
                s,
                &fill,
                x0 + 1.0,
                tl::PATTERN_Y + 2.0,
                bw,
                tl::PATTERN_H - 4.0,
                5.0,
            );
            if bw > 20.0 {
                let l = text.get(self, &b.name, false);
                s.push_clip(&draw::rect(
                    x0 + 1.0,
                    tl::PATTERN_Y,
                    bw - 4.0,
                    tl::PATTERN_H,
                ));
                draw::layout_at(
                    s,
                    &l,
                    &palette::readable_on(&fill),
                    x0 + 6.0,
                    tl::PATTERN_Y + 2.0,
                );
                s.pop();
            }
            if chosen == Some((b.group, b.instance)) {
                let ring = colors.get(Role::Accent);
                s.append_border(
                    &gsk::RoundedRect::from_rect(
                        draw::rect(x0, tl::PATTERN_Y + 1.0, bw + 2.0, tl::PATTERN_H - 2.0),
                        6.0,
                    ),
                    &[2.0; 4],
                    &[ring; 4],
                );
            }
        }
        s.pop();
    }

    /// Playhead and drag ghosts.
    fn draw_dynamic(&self, s: &gtk::Snapshot) {
        let imp = self.imp();
        let v = imp.view.get();
        let colors = palette::colors();
        let accent = colors.get(Role::Accent);
        let rows = self.rows();
        let clips = self.project_clips();
        self.draw_agent_glow(s, &rows, &clips);
        match imp.drag.borrow().as_ref() {
            Some(Drag::Move { ids, dt, copy, .. }) if *dt != 0 => {
                let ok = *copy || tl::fits_move(&clips, ids, *dt);
                let c = if ok { accent } else { colors.get(Role::Error) };
                for cl in clips.iter().filter(|c| ids.contains(&c.id)) {
                    let Some(row) = tl::row_of(&rows, cl.instrument) else {
                        continue;
                    };
                    let x0 = v.tick_to_x((cl.start as i64 + dt) as f64);
                    let x1 = v.tick_to_x((cl.end() as i64 + dt) as f64);
                    draw::outline(s, &c, x0, v.row_y(row) + 3.0, x1 - x0, v.row_h - 6.0, 2.0);
                }
            }
            Some(Drag::Block { block, dt, .. }) if *dt != 0 => {
                let ok = pattern_logic::fits_move(&clips, block, *dt);
                let c = if ok { accent } else { colors.get(Role::Error) };
                let x0 = v.tick_to_x((block.start as i64 + dt) as f64);
                let x1 = v.tick_to_x((block.end as i64 + dt) as f64);
                draw::outline(
                    s,
                    &c,
                    x0,
                    tl::PATTERN_Y + 1.0,
                    x1 - x0,
                    tl::PATTERN_H - 2.0,
                    2.0,
                );
                for cl in clips.iter().filter(|c| block.clips.contains(&c.id)) {
                    let Some(row) = tl::row_of(&rows, cl.instrument) else {
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
                    let Some(row) = tl::row_of(&rows, cl.instrument) else {
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
            Some(Drag::Fade {
                clip,
                fade_in,
                fades,
            }) => {
                if let Some(cl) = clips.iter().find(|c| c.id == *clip)
                    && let Some(row) = tl::row_of(&rows, cl.instrument)
                {
                    let mut moved = *cl;
                    if let Some(a) = &mut moved.audio {
                        a.fade_in = fades.0;
                        a.fade_out = fades.1;
                    }
                    let hx = tl::handle_x(&v, &moved, *fade_in);
                    let cy = v.row_y(row) + 3.0;
                    let r = 5.0;
                    draw::rounded(
                        s,
                        &accent,
                        hx - r,
                        cy + tl::HANDLE_R + 1.0 - r,
                        2.0 * r,
                        2.0 * r,
                        r,
                    );
                }
            }
            Some(Drag::Point {
                row,
                index,
                range,
                orig,
                points,
                ..
            }) if points != orig => {
                s.push_clip(&draw::rect(0.0, v.row_y(*row), v.width, v.row_h));
                draw::fill(
                    s,
                    &Palette::current().bg,
                    0.0,
                    v.row_y(*row),
                    v.width,
                    v.row_h,
                );
                draw_curve(s, &v, *row, points, *range, &accent, true);
                let bg = Palette::current().bg;
                draw_points(s, &v, *row, points, *range, &accent, &bg);
                if let Some(p) = points.get(*index) {
                    let (px, py) = (
                        v.tick_to_x(p.tick as f64),
                        shape_logic::value_y(p.value, *range, v.row_y(*row), v.row_h, LANE_PAD),
                    );
                    draw::rounded(s, &accent, px - 7.0, py - 7.0, 14.0, 14.0, 7.0);
                }
                s.pop();
            }
            Some(Drag::Loop { anchor, end, moved }) if *moved => {
                let (a, b) = (*anchor.min(end), *anchor.max(end));
                let x0 = v.tick_to_x(a as f64);
                let x1 = v.tick_to_x(b as f64);
                let mut c = accent;
                c.set_alpha(0.35);
                draw::rounded(s, &c, x0, tl::LOOP_Y + 2.0, x1 - x0, tl::LOOP_H - 4.0, 3.0);
            }
            _ => {}
        }
        // Playhead: a 2 px accent line with a small cap in the ruler.
        let x = v.tick_to_x(imp.last_playhead.get() as f64);
        if (0.0..=v.width).contains(&x) {
            draw::fill(s, &accent, x - 1.0, tl::LOOP_Y, 2.0, v.height);
            draw::rounded(s, &accent, x - 5.0, 0.0, 10.0, 6.0, 3.0);
        }
    }
}

/// A colour of the form 0xRRGGBB.
fn rgba(c: u32) -> gdk::RGBA {
    gdk::RGBA::new(
        ((c >> 16) & 0xff) as f32 / 255.0,
        ((c >> 8) & 0xff) as f32 / 255.0,
        (c & 0xff) as f32 / 255.0,
        1.0,
    )
}

/// The waveform of an audio clip: one bar per two pixels from the peak
/// summaries, thinned by the fades so they can be seen.
#[allow(clippy::too_many_arguments)]
fn draw_wave(
    s: &gtk::Snapshot,
    v: &View,
    c: &Clip,
    a: &protocol::model::AudioSource,
    peaks: &audio_clips::Peaks,
    bpm: f64,
    x0: f64,
    cw: f64,
    cy: f64,
    chh: f64,
    width: f64,
    ink: &gdk::RGBA,
) {
    let (wy, wh) = (cy + 4.0, chh - 8.0);
    let mid = wy + wh / 2.0;
    let amp = wh / 2.0 * 0.95;
    let mut col = *ink;
    col.set_alpha(0.6);
    let step = 2.0;
    let from = (x0 + 1.0).max(0.0);
    let to = (x0 + 1.0 + cw).min(width);
    let mut px = from;
    while px < to {
        let ta = (v.x_to_tick(px) - c.start as f64).max(0.0);
        let tb = ta + step / v.px_per_tick;
        let (lo, hi) = peaks.range(c.offset as f64 + ta, c.offset as f64 + tb, bpm);
        let fin = if a.fade_in > 0 {
            (ta / a.fade_in as f64).min(1.0)
        } else {
            1.0
        };
        let fout = if a.fade_out > 0 {
            ((c.len as f64 - ta) / a.fade_out as f64).clamp(0.0, 1.0)
        } else {
            1.0
        };
        let e = (fin * fout) as f32;
        let top = mid - (hi.min(1.0) * e) as f64 * amp;
        let bot = mid - (lo.max(-1.0) * e) as f64 * amp;
        draw::fill(s, &col, px, top, step - 0.5, (bot - top).max(1.0));
        px += step;
    }
}

/// A shape's curve in its lane, with the area under it faintly filled.
fn draw_curve(
    s: &gtk::Snapshot,
    v: &View,
    row: usize,
    points: &[ShapePoint],
    range: (f32, f32),
    line: &gdk::RGBA,
    fill_under: bool,
) {
    let (top, h) = (v.row_y(row), v.row_h);
    let bottom = top + h - LANE_PAD;
    let mut under = *line;
    under.set_alpha(0.12);
    let mut px = 0.0;
    while px < v.width {
        let tick = v.x_to_tick(px);
        if let Some(val) = shape_logic::value_at(points, tick) {
            let y = shape_logic::value_y(val, range, top, h, LANE_PAD);
            if fill_under {
                draw::fill(s, &under, px, y, 2.0, (bottom - y).max(0.0));
            }
            draw::fill(s, line, px, y - 1.0, 2.0, 2.0);
        }
        px += 2.0;
    }
}

/// The points of a shape as small dots.
fn draw_points(
    s: &gtk::Snapshot,
    v: &View,
    row: usize,
    points: &[ShapePoint],
    range: (f32, f32),
    line: &gdk::RGBA,
    bg: &gdk::RGBA,
) {
    for p in points {
        let x = v.tick_to_x(p.tick as f64);
        if x < -POINT_R || x > v.width + POINT_R {
            continue;
        }
        let y = shape_logic::value_y(p.value, range, v.row_y(row), v.row_h, LANE_PAD);
        draw::rounded(
            s,
            bg,
            x - POINT_R - 1.0,
            y - POINT_R - 1.0,
            2.0 * POINT_R + 2.0,
            2.0 * POINT_R + 2.0,
            POINT_R + 1.0,
        );
        draw::rounded(
            s,
            line,
            x - POINT_R + 1.0,
            y - POINT_R + 1.0,
            2.0 * POINT_R - 2.0,
            2.0 * POINT_R - 2.0,
            POINT_R - 1.0,
        );
    }
}

impl Timeline {
    /// A soft orange outline on the clips an agent is working on, and a
    /// faint wash on the row of an instrument it is working on (18.1).
    fn draw_agent_glow(&self, s: &gtk::Snapshot, rows: &[Row], clips: &[Clip]) {
        let v = self.imp().view.get();
        let base = crate::presence::glow_color();
        for (row, kind) in rows.iter().enumerate() {
            let Row::Instrument(id) = kind else { continue };
            let k = crate::presence::channel_glow(*id);
            if k <= 0.0 {
                continue;
            }
            let mut c = base;
            c.set_alpha(0.10 * k);
            draw::rounded(s, &c, 0.0, v.row_y(row) + 2.0, v.width, v.row_h - 4.0, 8.0);
        }
        s.push_clip(&draw::rect(
            0.0,
            tl::RULER_H,
            v.width,
            (v.height - tl::RULER_H).max(0.0),
        ));
        for cl in clips {
            let k = crate::presence::clip_glow(cl);
            if k <= 0.0 {
                continue;
            }
            let Some(row) = tl::row_of(rows, cl.instrument) else {
                continue;
            };
            let x0 = v.tick_to_x(cl.start as f64);
            let x1 = v.tick_to_x(cl.end() as f64);
            let rr = gsk::RoundedRect::from_rect(
                draw::rect(
                    x0 + 1.0,
                    v.row_y(row) + 3.0,
                    (x1 - x0 - 2.0).max(2.0),
                    v.row_h - 6.0,
                ),
                6.0,
            );
            let mut c = base;
            c.set_alpha(0.6 * k);
            s.append_outset_shadow(&rr, &c, 0.0, 0.0, 1.0, 8.0);
            c.set_alpha(0.95 * k);
            s.append_border(&rr, &[2.0; 4], &[c; 4]);
        }
        s.pop();
    }
}
