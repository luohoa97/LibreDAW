// SPDX-License-Identifier: GPL-3.0-or-later
//! The Mixer page (docs/ui-design.md 3.5): one strip per mixer track as a
//! card (color bar, name, effects, sends, pan, fader with meter, level
//! readout, Mute and Solo), the master strip pinned at the right. Routing a
//! channel to a track is not here; it lives with the channel's sound.
//!
//! Faders send their changes as one gesture: the group opens on the first
//! change and closes when the value has been still for half a second, so a
//! drag, or a burst of arrow-key presses, is one undo step (6).

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::gdk;
use gtk::gio;
use gtk::glib;

use protocol::consts::{MAX_GAIN_DB, MIN_GAIN_DB};
use protocol::edit::{Edit, MixValue};
use protocol::ids::{InstanceId, TrackId};
use protocol::model::Project;

use crate::app::{App, MeterUser};
use crate::dialogs::{self, PluginKind};
use crate::menus::{self, STRIP_ACTIONS};
use crate::widgets::color_bar::ColorBar;
use crate::widgets::meter::{Meter, db_text, peak_to_db};
use crate::widgets::rename_label::RenameLabel;

/// Fader range shown. The bottom of the range means silence.
const SLIDER_MIN_DB: f64 = -60.0;

pub fn slider_to_db(v: f64) -> f64 {
    if v <= SLIDER_MIN_DB + 0.05 {
        MIN_GAIN_DB
    } else {
        v.clamp(SLIDER_MIN_DB, MAX_GAIN_DB)
    }
}

pub fn db_to_slider(db: f64) -> f64 {
    db.clamp(SLIDER_MIN_DB, MAX_GAIN_DB)
}

/// The readout under a fader: "-3.2 dB", "-inf".
pub fn fader_text(db: f64) -> String {
    if db <= SLIDER_MIN_DB + 0.05 {
        "-inf".to_string()
    } else {
        format!("{db:.1} dB")
    }
}

/// "30 percent left", "centered", "100 percent right" (the pan range is
/// -1 to 1).
pub fn pan_text(pan: f64) -> String {
    let pct = (pan.abs() * 100.0).round() as i32;
    if pct == 0 {
        "centered".to_string()
    } else if pan < 0.0 {
        format!("{pct} percent left")
    } else {
        format!("{pct} percent right")
    }
}

/// Whether a strip is silenced by someone else's solo.
pub fn silenced_by_solo(any_solo: bool, solo: bool) -> bool {
    any_solo && !solo
}

struct Strip {
    track: TrackId,
    root: gtk::Box,
    fader: gtk::Scale,
    pan: gtk::Scale,
    mute: gtk::ToggleButton,
    solo: gtk::ToggleButton,
    readout: gtk::Label,
    silent: gtk::Label,
    meter: Meter,
    peak: gtk::Label,
    name: Option<Rc<RenameLabel>>,
}

pub struct Mixer {
    app: Rc<App>,
    root: gtk::Box,
    stack: gtk::Stack,
    body: gtk::Box,
    master_slot: gtk::Box,
    strips: RefCell<Vec<Strip>>,
    /// The loudness reading on the Main Output strip.
    lufs: RefCell<Option<gtk::Label>>,
    sig: RefCell<String>,
    updating: Cell<bool>,
    editing: Cell<u32>,
}

impl Mixer {
    pub fn new(app: Rc<App>) -> Rc<Mixer> {
        let body = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        body.set_margin_start(8);
        body.set_margin_end(8);
        body.set_margin_top(8);
        body.set_margin_bottom(8);
        let scroller = gtk::ScrolledWindow::builder()
            .child(&body)
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .hexpand(true)
            .vexpand(true)
            .min_content_width(0)
            .build();
        let empty = adw::StatusPage::new();
        empty.set_icon_name(Some("audio-volume-high-symbolic"));
        empty.set_title("No Tracks Yet");
        empty.set_description(Some("Tracks appear here when you add instruments."));
        empty.add_css_class("compact");
        let stack = gtk::Stack::new();
        stack.add_named(&scroller, Some("strips"));
        stack.add_named(&empty, Some("empty"));
        stack.set_hexpand(true);

        let master_slot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        master_slot.set_margin_end(8);
        master_slot.set_margin_start(8);
        master_slot.set_margin_top(8);
        master_slot.set_margin_bottom(8);
        let root = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        root.append(&stack);
        root.append(&master_slot);
        root.set_accessible_role(gtk::AccessibleRole::Group);
        root.update_property(&[gtk::accessible::Property::Label("Mixer")]);
        let m = Rc::new(Mixer {
            app: app.clone(),
            root,
            stack,
            body,
            master_slot,
            strips: RefCell::new(Vec::new()),
            lufs: RefCell::new(None),
            sig: RefCell::new(String::new()),
            updating: Cell::new(false),
            editing: Cell::new(0),
        });
        let mm = m.clone();
        app.on_change(move || mm.sync());
        let mm = m.clone();
        app.on_view_change(move || mm.apply_size());
        let mm = Rc::downgrade(&m);
        m.root.add_tick_callback(move |_, _| {
            if let Some(m) = mm.upgrade() {
                m.feed_meters();
                glib::ControlFlow::Continue
            } else {
                glib::ControlFlow::Break
            }
        });
        m.sync();
        m
    }

    pub fn widget(&self) -> gtk::Widget {
        self.root.clone().upcast()
    }

    /// F2 on the Mixer page: renames the selected track (not Master).
    pub fn rename_selected(&self) {
        let track = self.app.ui.borrow().track;
        let label = self
            .strips
            .borrow()
            .iter()
            .find(|s| s.track == track)
            .and_then(|s| s.name.clone());
        if let Some(l) = label {
            l.start_editing();
        }
    }

    fn signature(p: &Project) -> String {
        let mut s = String::new();
        for t in &p.tracks {
            s.push_str(&format!("t{}:{}", t.id, t.name));
            // Every effect, built-in or plugin, changes the strip.
            for i in &t.inserts {
                s.push_str(&format!(",{}", i.instance()));
            }
            s.push(';');
        }
        for c in &p.channels {
            s.push_str(&format!("c{}>{}:{};", c.id, c.track, c.name));
        }
        s
    }

    fn sync(self: &Rc<Mixer>) {
        let sig = {
            let s = self.app.session.borrow();
            Self::signature(&s.document().project)
        };
        if *self.sig.borrow() != sig && self.editing.get() == 0 {
            *self.sig.borrow_mut() = sig;
            // Not inside the signal handler of a widget that is about to
            // be replaced.
            let me = self.clone();
            glib::idle_add_local_once(move || {
                me.rebuild();
                me.update_values();
            });
        } else {
            self.update_values();
        }
    }

    fn apply_size(&self) {
        let w = self.app.size_class().strip_width() as i32;
        for s in self.strips.borrow().iter() {
            s.root.set_width_request(w);
        }
        let h = self.app.size_class().meter_height() as i32;
        for s in self.strips.borrow().iter() {
            s.meter.set_height_request(h);
        }
    }

    fn rebuild(self: &Rc<Mixer>) {
        while let Some(c) = self.body.first_child() {
            self.body.remove(&c);
        }
        while let Some(c) = self.master_slot.first_child() {
            self.master_slot.remove(&c);
        }
        self.strips.borrow_mut().clear();
        *self.lufs.borrow_mut() = None;
        let proj = self.app.session.borrow().document().project.clone();
        let mut tracks = 0;
        for t in proj.tracks.iter().filter(|t| t.id != TrackId::MASTER) {
            let strip = self.build_strip(t.id, &proj);
            self.body.append(&strip);
            tracks += 1;
        }
        if proj.track(TrackId::MASTER).is_some() {
            let strip = self.build_strip(TrackId::MASTER, &proj);
            self.master_slot.append(&strip);
        }
        self.stack
            .set_visible_child_name(if tracks == 0 { "empty" } else { "strips" });
        self.apply_size();
    }

    fn build_strip(self: &Rc<Mixer>, id: TrackId, proj: &Project) -> gtk::Widget {
        let Some(track) = proj.track(id) else {
            return gtk::Box::new(gtk::Orientation::Vertical, 0).upcast();
        };
        let name = track.name.clone();
        let is_master = id == TrackId::MASTER;
        let strip = gtk::Box::new(gtk::Orientation::Vertical, 6);
        strip.add_css_class("card");
        strip.add_css_class("ldaw-strip");
        crate::presence_ui::tag(&strip, protocol::control::Focus::Track(id));
        strip.set_valign(gtk::Align::Fill);
        strip.set_hexpand(false);
        strip.set_accessible_role(gtk::AccessibleRole::Group);
        strip.update_property(&[gtk::accessible::Property::Label(&format!(
            "Mixer strip: {name}"
        ))]);

        // The color of the first channel playing through this track.
        let slot = proj
            .channels
            .iter()
            .position(|c| c.track == id)
            .map(|i| i as u32)
            .unwrap_or(id.0);
        let bar = ColorBar::new(slot);
        bar.set_horizontal(true);
        bar.set_height_request(4);
        bar.set_margin_start(6);
        bar.set_margin_end(6);
        bar.set_margin_top(6);
        strip.append(&bar);

        let inner = gtk::Box::new(gtk::Orientation::Vertical, 6);
        inner.set_margin_start(6);
        inner.set_margin_end(6);
        inner.set_margin_bottom(8);
        strip.append(&inner);

        // Name.
        let mut name_label: Option<Rc<RenameLabel>> = None;
        if is_master {
            let l = gtk::Label::new(Some("Main Output"));
            l.add_css_class("heading");
            inner.append(&l);
        } else {
            // Renamed with F2 or the strip menu, never by a click.
            let l = RenameLabel::new(&name);
            l.label().set_xalign(0.5);
            l.label().set_width_chars(6);
            l.label().set_max_width_chars(10);
            l.label().add_css_class("heading");
            l.widget
                .set_tooltip_text(Some("Track name (F2 or the menu renames)"));
            {
                let m = self.clone();
                l.connect_editing(move |on| {
                    if on {
                        m.editing.set(m.editing.get() + 1);
                    } else {
                        m.editing.set(m.editing.get().saturating_sub(1));
                        let m2 = m.clone();
                        glib::idle_add_local_once(move || m2.sync());
                    }
                });
                let m = self.clone();
                l.connect_commit(move |new| {
                    m.app.edit(vec![Edit::RenameTrack {
                        track: id,
                        name: new.to_string(),
                    }]);
                });
            }
            inner.append(&l.widget);
            name_label = Some(l);
            // Which channels play through this track.
            let names: Vec<&str> = proj
                .channels
                .iter()
                .filter(|c| c.track == id)
                .map(|c| c.name.as_str())
                .collect();
            let cap = gtk::Label::new(Some(&names.join(", ")));
            cap.add_css_class("caption");
            cap.add_css_class("dim-label");
            cap.set_ellipsize(gtk::pango::EllipsizeMode::End);
            cap.set_max_width_chars(10);
            cap.set_tooltip_text(Some(&format!(
                "Instruments on this track: {}",
                names.join(", ")
            )));
            inner.append(&cap);
        }
        let silent = gtk::Label::new(Some("Silent"));
        silent.add_css_class("caption");
        silent.add_css_class("dim-label");
        silent.set_visible(false);
        inner.append(&silent);

        // Effects.
        let fx_label = gtk::Label::new(Some("Effects"));
        fx_label.add_css_class("caption-heading");
        fx_label.set_xalign(0.0);
        inner.append(&fx_label);
        // Every effect on the track, built-in or plugin, in signal order.
        for ins in &track.inserts {
            let row = match ins {
                protocol::model::Insert::Clap(r) => self.insert_row(id, r.instance, &r.plugin_id),
                protocol::model::Insert::Builtin { instance, fx, .. } => {
                    self.builtin_row(id, *instance, fx.kind())
                }
            };
            inner.append(&row);
        }
        // One "add effect" menu: the built-in effects, then a plugin.
        let add_tip = "Add an effect that changes how this track sounds";
        let add = gtk::MenuButton::new();
        add.set_icon_name("list-add-symbolic");
        add.add_css_class("flat");
        add.set_tooltip_text(Some(add_tip));
        add.update_property(&[gtk::accessible::Property::Label(add_tip)]);
        add.set_menu_model(Some(&menus::effects_menu()));
        let fx = gio::SimpleActionGroup::new();
        for n in menus::FX_ACTIONS {
            let action = if *n == "add" {
                gio::SimpleAction::new(n, Some(glib::VariantTy::STRING))
            } else {
                gio::SimpleAction::new(n, None)
            };
            let (m, b, n) = (self.clone(), add.clone(), *n);
            action.connect_activate(move |_, v| match n {
                "add" => {
                    let want = v.and_then(|v| v.get::<String>());
                    if let Some(kind) = menus::EFFECTS
                        .iter()
                        .find(|e| Some(e.1) == want.as_deref())
                        .map(|e| e.0)
                    {
                        m.add_builtin(id, kind);
                    }
                }
                "plugin" => m.add_effect(b.upcast_ref(), id),
                _ => {}
            });
            fx.add_action(&action);
        }
        add.insert_action_group("fx", Some(&fx));
        inner.append(&add);

        // Duck to Kick on a track; Loudness on Main Output.
        if is_master {
            let m = self.clone();
            let (card, reading) = crate::fx_panel::loudness_card(&self.app, proj, move |down| {
                if down {
                    m.editing.set(m.editing.get() + 1);
                } else {
                    m.editing.set(m.editing.get().saturating_sub(1));
                    m.sync();
                }
            });
            *self.lufs.borrow_mut() = Some(reading);
            inner.append(&card);
        } else if let Some(row) = crate::fx_panel::duck_row(&self.app, proj, id) {
            inner.append(&row);
        }

        // Pan, with its name.
        let pan_label = gtk::Label::new(Some("Left/Right"));
        pan_label.add_css_class("caption-heading");
        pan_label.set_xalign(0.0);
        inner.append(&pan_label);
        let pan = gtk::Scale::with_range(gtk::Orientation::Horizontal, -1.0, 1.0, 0.01);
        pan.set_draw_value(false);
        pan.add_mark(0.0, gtk::PositionType::Bottom, None);
        pan.set_tooltip_text(Some(
            "Moves the sound toward the left or right speaker; double-click to center it",
        ));
        pan.update_property(&[gtk::accessible::Property::Label(&format!(
            "Left/Right of {name}"
        ))]);
        {
            let m = self.clone();
            pan.connect_value_changed(move |s| {
                let v = s.value();
                let v = if v.abs() < 0.03 { 0.0 } else { v };
                s.update_property(&[gtk::accessible::Property::ValueText(&pan_text(v))]);
                if !m.updating.get() {
                    m.fader_changed(id, MixValue::Pan(v));
                }
            });
            let reset = gtk::GestureClick::new();
            let p = pan.clone();
            reset.connect_pressed(move |_, n, _, _| {
                if n == 2 {
                    p.set_value(0.0);
                }
            });
            pan.add_controller(reset);
        }
        // The scale would ask for 130 px; a clamp keeps the strip narrow.
        let pan_clamp = adw::Clamp::builder()
            .maximum_size(84)
            .tightening_threshold(60)
            .child(&pan)
            .build();
        inner.append(&pan_clamp);

        // Fader beside the meter.
        let fader =
            gtk::Scale::with_range(gtk::Orientation::Vertical, SLIDER_MIN_DB, MAX_GAIN_DB, 0.5);
        fader.add_css_class("ldaw-fader");
        fader.set_inverted(true);
        fader.set_draw_value(false);
        fader.set_vexpand(true);
        fader.set_height_request(120);
        fader.set_tooltip_text(Some(
            "How loud this track is; double-click to set it back to 0 dB",
        ));
        fader.add_mark(0.0, gtk::PositionType::Right, None);
        fader.adjustment().set_page_increment(3.0);
        fader.update_property(&[gtk::accessible::Property::Label(&format!(
            "Volume of {name}"
        ))]);
        let readout = gtk::Label::new(Some("0.0 dB"));
        readout.add_css_class("numeric");
        readout.add_css_class("caption");
        {
            let (m, ro) = (self.clone(), readout.clone());
            fader.connect_value_changed(move |s| {
                let db = slider_to_db(s.value());
                ro.set_text(&fader_text(db));
                s.update_property(&[gtk::accessible::Property::ValueText(&fader_text(db))]);
                if !m.updating.get() {
                    m.fader_changed(id, MixValue::VolumeDb(db));
                }
            });
            let reset = gtk::GestureClick::new();
            let f = fader.clone();
            reset.connect_pressed(move |_, n, _, _| {
                if n == 2 {
                    f.set_value(0.0);
                }
            });
            fader.add_controller(reset);
            // Home is 0 dB and End is silence; scrolling only acts on a
            // focused fader so scrolling the mixer never moves one.
            let keys = gtk::EventControllerKey::new();
            let f = fader.clone();
            keys.connect_key_pressed(move |_, key, _, st| match key {
                _ if !crate::keys::plain(st) => glib::Propagation::Proceed,
                gdk::Key::Home => {
                    f.set_value(0.0);
                    glib::Propagation::Stop
                }
                gdk::Key::End => {
                    f.set_value(SLIDER_MIN_DB);
                    glib::Propagation::Stop
                }
                _ => glib::Propagation::Proceed,
            });
            fader.add_controller(keys);
            let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
            scroll.set_propagation_phase(gtk::PropagationPhase::Capture);
            let f = fader.clone();
            scroll.connect_scroll(move |_, _, _| {
                if f.has_focus() {
                    glib::Propagation::Proceed
                } else {
                    // Let the page scroll instead of the fader.
                    glib::Propagation::Stop
                }
            });
            fader.add_controller(scroll);
        }
        let meter = Meter::new();
        meter.set_label(&format!("Level of {name}"));
        meter.set_vexpand(true);
        let peak = gtk::Label::new(Some("Silent"));
        peak.add_css_class("numeric");
        peak.add_css_class("caption");
        peak.add_css_class("dim-label");
        let mrow = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        mrow.set_halign(gtk::Align::Center);
        mrow.append(&meter);
        mrow.append(&fader);
        mrow.set_vexpand(true);
        inner.append(&mrow);
        inner.append(&peak);
        readout.set_tooltip_text(Some("How loud this track is, in decibels"));
        inner.append(&readout);

        // Mute and Solo, with their full names.
        let mute = gtk::ToggleButton::with_label("Mute");
        mute.add_css_class("caption");
        mute.set_tooltip_text(Some("Silence this track (M)"));
        mute.update_property(&[gtk::accessible::Property::Label(&format!("Mute {name}"))]);
        let solo = gtk::ToggleButton::with_label("Solo");
        solo.add_css_class("caption");
        solo.set_tooltip_text(Some("Hear only this track (S)"));
        solo.update_property(&[gtk::accessible::Property::Label(&format!("Solo {name}"))]);
        for (b, is_mute) in [(&mute, true), (&solo, false)] {
            let m = self.clone();
            b.connect_toggled(move |b| {
                if m.updating.get() {
                    return;
                }
                let v = if is_mute {
                    MixValue::Mute(b.is_active())
                } else {
                    MixValue::Solo(b.is_active())
                };
                m.app.edit(vec![Edit::SetTrackMix {
                    track: id,
                    value: v,
                }]);
            });
        }
        let ms = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        ms.set_halign(gtk::Align::Center);
        ms.add_css_class("linked");
        ms.append(&mute);
        ms.append(&solo);
        inner.append(&ms);

        // Keys on a focused strip: M and S.
        {
            let keys = gtk::EventControllerKey::new();
            let (mu, so) = (mute.clone(), solo.clone());
            keys.connect_key_pressed(move |c, key, _, st| {
                if st.intersects(gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::ALT_MASK) {
                    return glib::Propagation::Proceed;
                }
                // Not while a text field has the focus.
                let editing = c
                    .widget()
                    .and_then(|w| w.root())
                    .and_then(|r| r.focus())
                    .is_some_and(|f| f.is::<gtk::Editable>());
                if editing {
                    return glib::Propagation::Proceed;
                }
                match key {
                    gdk::Key::m | gdk::Key::M => {
                        mu.set_active(!mu.is_active());
                        glib::Propagation::Stop
                    }
                    gdk::Key::s | gdk::Key::S => {
                        so.set_active(!so.is_active());
                        glib::Propagation::Stop
                    }
                    _ => glib::Propagation::Proceed,
                }
            });
            strip.add_controller(keys);
        }

        // Right click: Rename, Remove Track, Reset Fader.
        if let Some(label) = name_label.clone() {
            let group = gio::SimpleActionGroup::new();
            for n in STRIP_ACTIONS {
                let a = gio::SimpleAction::new(n, None);
                let (m, f, l, n) = (self.clone(), fader.clone(), label.clone(), *n);
                a.connect_activate(move |_, _| match n {
                    "rename" => l.start_editing(),
                    "reset" => f.set_value(0.0),
                    "remove" => {
                        m.app.edit(vec![Edit::RemoveTrack { track: id }]);
                        let a = m.app.clone();
                        m.app
                            .toast_action("Track removed", "Undo", move || a.undo());
                    }
                    _ => {}
                });
                group.add_action(&a);
            }
            strip.insert_action_group("strip", Some(&group));
            let a = self.app.clone();
            crate::context_menu::attach(&strip, &menus::strip_menu(), move |_| {
                a.select_track(id);
                true
            });
            // F2 on a focused strip renames it.
            let keys = gtk::EventControllerKey::new();
            keys.connect_key_pressed(move |_, key, _, st| {
                if key == gdk::Key::F2 && crate::keys::plain(st) {
                    label.start_editing();
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                }
            });
            strip.add_controller(keys);
        }

        if crate::perf::enabled() && std::env::var_os("LIBREDAW_MIN_DEBUG").is_some() {
            let mut c = inner.first_child();
            while let Some(w) = c {
                eprintln!(
                    "libredaw: strip {name}: {} min width {}",
                    w.type_().name(),
                    w.measure(gtk::Orientation::Horizontal, -1).0
                );
                c = w.next_sibling();
            }
        }
        self.strips.borrow_mut().push(Strip {
            track: id,
            root: strip.clone(),
            fader,
            pan,
            mute,
            solo,
            readout,
            silent,
            meter,
            peak,
            name: name_label,
        });
        strip.upcast()
    }

    fn insert_row(
        self: &Rc<Mixer>,
        track: TrackId,
        inst: InstanceId,
        plugin_id: &str,
    ) -> gtk::Widget {
        let name = {
            let s = self.app.session.borrow();
            s.registry
                .find_desc(plugin_id)
                .map(|d| d.name.clone())
                .unwrap_or_else(|| plugin_id.to_string())
        };
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        let open = gtk::Button::with_label(&name);
        open.add_css_class("flat");
        open.set_hexpand(true);
        if let Some(l) = open.child().and_then(|c| c.downcast::<gtk::Label>().ok()) {
            l.set_ellipsize(gtk::pango::EllipsizeMode::End);
            l.set_max_width_chars(8);
            l.set_xalign(0.0);
        }
        open.set_tooltip_text(Some(&format!("Open the window of {name}")));
        let (m, nm) = (self.clone(), name.clone());
        open.connect_clicked(move |_| m.show_gui(inst, &nm));
        row.append(&open);
        let del = gtk::Button::from_icon_name("window-close-symbolic");
        del.add_css_class("flat");
        del.add_css_class("circular");
        del.set_tooltip_text(Some("Remove Effect"));
        del.update_property(&[gtk::accessible::Property::Label(&format!("Remove {name}"))]);
        let m = self.clone();
        del.connect_clicked(move |_| {
            m.app.edit(vec![Edit::RemoveInsert {
                track,
                instance: inst,
            }]);
        });
        row.append(&del);
        row.upcast()
    }

    /// A built-in effect on the strip: its plain name (the explanation in
    /// the tooltip) and a Remove button.
    fn builtin_row(
        self: &Rc<Mixer>,
        track: TrackId,
        inst: InstanceId,
        kind: protocol::beats::BuiltinFxKind,
    ) -> gtk::Widget {
        let (name, what) = menus::effect_name(kind);
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        // The name opens the effect's panel: styles first, knobs behind More.
        let open = gtk::Button::with_label(name);
        open.add_css_class("flat");
        open.set_hexpand(true);
        if let Some(l) = open.child().and_then(|c| c.downcast::<gtk::Label>().ok()) {
            l.set_ellipsize(gtk::pango::EllipsizeMode::End);
            l.set_xalign(0.0);
        }
        open.set_tooltip_text(Some(&format!("{what}. Click to change how it sounds")));
        let m = self.clone();
        open.connect_clicked(move |b| crate::fx_panel::show(&m.app, b.upcast_ref(), track, inst));
        row.append(&open);
        let del = gtk::Button::from_icon_name("window-close-symbolic");
        del.add_css_class("flat");
        del.add_css_class("circular");
        let tip = format!("Remove {name}");
        del.set_tooltip_text(Some(&tip));
        del.update_property(&[gtk::accessible::Property::Label(&tip)]);
        let m = self.clone();
        del.connect_clicked(move |_| {
            m.app.edit(vec![Edit::RemoveInsert {
                track,
                instance: inst,
            }]);
        });
        row.append(&del);
        row.upcast()
    }

    /// Adds a built-in effect at the end of the track's effects.
    fn add_builtin(&self, track: TrackId, fx: protocol::beats::BuiltinFxKind) {
        let n = self
            .app
            .session
            .borrow()
            .document()
            .project
            .track(track)
            .map(|t| t.inserts.len())
            .unwrap_or(0);
        self.app.edit(vec![Edit::AddBuiltinInsert {
            track,
            index: n.min(255) as u8,
            fx,
        }]);
    }

    fn show_gui(&self, inst: InstanceId, title: &str) {
        let r = self.app.session.borrow_mut().registry.show_gui(inst, title);
        if let Err(e) = r {
            self.app
                .toast(&format!("Cannot open the plugin window: {e}"));
        }
    }

    pub fn add_effect(self: &Rc<Mixer>, parent: &gtk::Widget, track: TrackId) {
        let app = self.app.clone();
        dialogs::choose_plugin(
            parent,
            &self.app,
            PluginKind::Effect,
            "Add Effect",
            move |d| {
                let n = app
                    .session
                    .borrow()
                    .document()
                    .project
                    .track(track)
                    .map(|t| t.inserts.len())
                    .unwrap_or(0);
                app.edit(vec![Edit::AddInsert {
                    track,
                    index: n.min(255) as u8,
                    plugin_id: d.id.clone(),
                }]);
            },
        );
    }

    /// Fader and pan moves: one gesture until the control rests.
    fn fader_changed(self: &Rc<Mixer>, track: TrackId, v: MixValue) {
        self.app
            .edit_resting("Mixer", vec![Edit::SetTrackMix { track, value: v }]);
    }

    /// Writes document values into the widgets without sending edits.
    fn update_values(&self) {
        let s = self.app.session.borrow();
        let p = &s.document().project;
        let any_solo = p.tracks.iter().any(|t| t.mix.solo);
        self.updating.set(true);
        for st in self.strips.borrow().iter() {
            let Some(tr) = p.track(st.track) else {
                continue;
            };
            let mix = tr.mix;
            let want = db_to_slider(mix.volume_db);
            if (st.fader.value() - want).abs() > 1e-6 {
                st.fader.set_value(want);
            }
            st.readout.set_text(&fader_text(mix.volume_db));
            if (st.pan.value() - mix.pan).abs() > 1e-6 {
                st.pan.set_value(mix.pan);
            }
            st.mute.set_active(mix.mute);
            st.solo.set_active(mix.solo);
            // State is visible without color: a silenced strip dims and says
            // so, a soloed one gets an outline.
            let silent = mix.mute || silenced_by_solo(any_solo, mix.solo);
            st.silent.set_visible(silent && !mix.mute);
            if silent {
                st.root.add_css_class("silenced");
            } else {
                st.root.remove_css_class("silenced");
            }
            if mix.solo {
                st.root.add_css_class("soloed");
            } else {
                st.root.remove_css_class("soloed");
            }
        }
        self.updating.set(false);
    }

    fn feed_meters(&self) {
        if let Some(l) = self.lufs.borrow().as_ref() {
            let text = crate::fx_panel::lufs_text(self.app.session.borrow().link.loudness.lufs());
            if l.text() != text {
                l.set_text(&text);
            }
        }
        for st in self.strips.borrow().iter() {
            let p = self.app.take_peaks(st.track, MeterUser::Mixer);
            st.meter.update([peak_to_db(p[0]), peak_to_db(p[1])]);
            let text = match db_text(st.meter.peak_db()) {
                t if t == "-inf" => "Silent".to_string(),
                t => t,
            };
            if st.peak.text() != text {
                st.peak.set_text(&text);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::ids::ChannelId;

    #[test]
    fn slider_maps_to_db_and_back() {
        assert_eq!(slider_to_db(-60.0), MIN_GAIN_DB);
        assert_eq!(slider_to_db(-59.97), MIN_GAIN_DB);
        assert_eq!(slider_to_db(-30.0), -30.0);
        assert_eq!(slider_to_db(0.0), 0.0);
        assert_eq!(slider_to_db(99.0), MAX_GAIN_DB);
        assert_eq!(db_to_slider(MIN_GAIN_DB), SLIDER_MIN_DB);
        assert_eq!(db_to_slider(-12.5), -12.5);
        assert_eq!(db_to_slider(20.0), MAX_GAIN_DB);
    }

    #[test]
    fn readouts() {
        assert_eq!(fader_text(-3.24), "-3.2 dB");
        assert_eq!(fader_text(0.0), "0.0 dB");
        assert_eq!(fader_text(MIN_GAIN_DB), "-inf");
        assert_eq!(pan_text(0.0), "centered");
        assert_eq!(pan_text(-0.3), "30 percent left");
        assert_eq!(pan_text(1.0), "100 percent right");
    }

    #[test]
    fn solo_silences_the_others() {
        assert!(!silenced_by_solo(false, false));
        assert!(silenced_by_solo(true, false));
        assert!(!silenced_by_solo(true, true));
    }

    #[test]
    fn signature_changes_with_structure_and_names_only() {
        use doc::document::{Document, apply};
        use protocol::edit::NewInstrument;
        let (d, ids) = apply(
            &Document::new(),
            &Edit::AddChannel {
                name: "A".into(),
                instrument: NewInstrument::Synth {
                    params: protocol::model::SynthParams::default(),
                },
                root_key: 60,
                track: TrackId::MASTER,
            },
        )
        .unwrap();
        let s0 = Mixer::signature(&d.project);
        let (d2, _) = apply(
            &d,
            &Edit::SetChannelMix {
                channel: ChannelId(ids[0]),
                value: MixValue::VolumeDb(-3.0),
            },
        )
        .unwrap();
        assert_eq!(Mixer::signature(&d2.project), s0, "fader moves keep strips");
        let (d3, _) = apply(
            &d,
            &Edit::RenameChannel {
                channel: ChannelId(ids[0]),
                name: "B".into(),
            },
        )
        .unwrap();
        assert_ne!(Mixer::signature(&d3.project), s0);
        let (d4, _) = apply(&d, &Edit::AddTrack { name: "T".into() }).unwrap();
        assert_ne!(Mixer::signature(&d4.project), s0);
    }
}
