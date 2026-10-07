// SPDX-License-Identifier: GPL-3.0-or-later
//! The mixer panel (SPEC 13.1 item 6): a strip per channel (volume, pan,
//! mute, solo, route) and per mixer track (volume, pan, mute, solo, meter,
//! inserts), master last. Every change is an `Edit` through `App`.
//!
//! Faders send their changes as one gesture: the group opens on the first
//! change and closes when the value has been still for half a second, so a
//! drag, or a burst of arrow-key presses, is one undo step (6).

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use gtk::glib;
use gtk::prelude::*;

use protocol::consts::{MAX_GAIN_DB, MIN_GAIN_DB};
use protocol::edit::{Edit, MixValue};
use protocol::ids::{ChannelId, InstanceId, TrackId};
use protocol::model::{Insert, Instrument, Project};

use crate::app::App;
use crate::dialogs::{self, PluginKind};
use crate::widgets::meter::{Meter, peak_to_db};

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

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Target {
    Channel(ChannelId),
    Track(TrackId),
}

struct Strip {
    target: Target,
    fader: gtk::Scale,
    pan: gtk::Scale,
    mute: gtk::ToggleButton,
    solo: gtk::ToggleButton,
    route: Option<gtk::DropDown>,
    meter: Option<Meter>,
}

pub struct Mixer {
    app: Rc<App>,
    root: gtk::Box,
    body: gtk::Box,
    strips: RefCell<Vec<Strip>>,
    sig: RefCell<String>,
    updating: Cell<bool>,
    gesture_timer: RefCell<Option<glib::SourceId>>,
}

impl Mixer {
    pub fn new(app: Rc<App>) -> Rc<Mixer> {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let body = gtk::Box::new(gtk::Orientation::Horizontal, 12);
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
            .build();
        scroller.add_css_class("view");
        let header = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        header.add_css_class("toolbar");
        let hl = gtk::Label::new(Some("Mixer"));
        hl.add_css_class("heading");
        header.append(&hl);
        root.append(&header);
        root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        root.append(&scroller);
        root.set_accessible_role(gtk::AccessibleRole::Group);
        root.update_property(&[gtk::accessible::Property::Label("Mixer")]);
        let m = Rc::new(Mixer {
            app: app.clone(),
            root,
            body,
            strips: RefCell::new(Vec::new()),
            sig: RefCell::new(String::new()),
            updating: Cell::new(false),
            gesture_timer: RefCell::new(None),
        });
        let mm = m.clone();
        app.on_change(move || mm.sync());
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

    fn signature(p: &Project) -> String {
        let mut s = String::new();
        for c in &p.channels {
            let clap = matches!(c.instrument, Instrument::Clap(_));
            s.push_str(&format!("c{}:{}:{}:{};", c.id, c.track, clap as u8, c.name));
        }
        for t in &p.tracks {
            s.push_str(&format!("t{}:{}", t.id, t.name));
            for Insert::Clap(r) in &t.inserts {
                s.push_str(&format!(",{}={}", r.instance, r.plugin_id));
            }
            s.push(';');
        }
        s
    }

    fn sync(self: &Rc<Mixer>) {
        let sig = {
            let s = self.app.session.borrow();
            Self::signature(&s.document().project)
        };
        if *self.sig.borrow() != sig {
            *self.sig.borrow_mut() = sig;
            self.rebuild();
        }
        self.update_values();
    }

    fn rebuild(self: &Rc<Mixer>) {
        while let Some(c) = self.body.first_child() {
            self.body.remove(&c);
        }
        self.strips.borrow_mut().clear();
        let proj = self.app.session.borrow().document().project.clone();

        let chan_label = gtk::Label::new(Some("Channels"));
        chan_label.add_css_class("heading");
        chan_label.set_halign(gtk::Align::Start);
        let chans = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        for c in &proj.channels {
            let strip = self.build_strip(Target::Channel(c.id), &c.name, &proj);
            if chans.first_child().is_some() {
                chans.append(&gtk::Separator::new(gtk::Orientation::Vertical));
            }
            chans.append(&strip);
        }
        if proj.channels.is_empty() {
            let l = gtk::Label::new(Some("No channels"));
            l.add_css_class("dim-label");
            chans.append(&l);
        }
        let left = gtk::Box::new(gtk::Orientation::Vertical, 6);
        left.append(&chan_label);
        left.append(&chans);

        let track_label = gtk::Label::new(Some("Tracks"));
        track_label.add_css_class("heading");
        track_label.set_halign(gtk::Align::Start);
        let tracks = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        // Master last.
        let mut order: Vec<_> = proj
            .tracks
            .iter()
            .filter(|t| t.id != TrackId::MASTER)
            .collect();
        if let Some(m) = proj.track(TrackId::MASTER) {
            order.push(m);
        }
        for t in order {
            let strip = self.build_strip(Target::Track(t.id), &t.name, &proj);
            if tracks.first_child().is_some() {
                tracks.append(&gtk::Separator::new(gtk::Orientation::Vertical));
            }
            tracks.append(&strip);
        }
        let right = gtk::Box::new(gtk::Orientation::Vertical, 6);
        right.append(&track_label);
        right.append(&tracks);

        self.body.append(&left);
        self.body
            .append(&gtk::Separator::new(gtk::Orientation::Vertical));
        self.body.append(&right);
    }

    fn build_strip(self: &Rc<Mixer>, target: Target, name: &str, proj: &Project) -> gtk::Widget {
        let strip = gtk::Box::new(gtk::Orientation::Vertical, 6);
        strip.set_width_request(104);
        let inner = gtk::Box::new(gtk::Orientation::Vertical, 6);
        inner.set_margin_top(8);
        inner.set_margin_bottom(8);
        inner.set_margin_start(8);
        inner.set_margin_end(8);
        strip.append(&inner);

        let title = gtk::Label::new(Some(name));
        title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        title.set_max_width_chars(12);
        title.add_css_class("heading");
        inner.append(&title);

        let mut route = None;
        if let Target::Channel(cid) = target {
            let names: Vec<String> = proj.tracks.iter().map(|t| t.name.clone()).collect();
            let refs: Vec<&str> = names.iter().map(String::as_str).collect();
            let dd = gtk::DropDown::from_strings(&refs);
            dd.set_tooltip_text(Some("Mixer track this channel plays through"));
            let m = self.clone();
            dd.connect_selected_notify(move |d| {
                if m.updating.get() {
                    return;
                }
                let i = d.selected() as usize;
                let tid = m
                    .app
                    .session
                    .borrow()
                    .document()
                    .project
                    .tracks
                    .get(i)
                    .map(|t| t.id);
                if let Some(t) = tid {
                    m.app.edit(vec![Edit::SetChannelTrack {
                        channel: cid,
                        track: t,
                    }]);
                }
            });
            inner.append(&dd);
            route = Some(dd);
        }

        let fader =
            gtk::Scale::with_range(gtk::Orientation::Vertical, SLIDER_MIN_DB, MAX_GAIN_DB, 0.5);
        fader.set_inverted(true);
        fader.set_draw_value(true);
        fader.set_value_pos(gtk::PositionType::Bottom);
        fader.set_height_request(150);
        fader.set_vexpand(true);
        fader.set_tooltip_text(Some("Volume in dB. Double-click resets to 0."));
        fader.add_mark(0.0, gtk::PositionType::Right, None);
        fader.update_property(&[gtk::accessible::Property::Label(&format!("{name} volume"))]);
        fader.set_format_value_func(|_, v| {
            if v <= SLIDER_MIN_DB + 0.05 {
                "-inf".to_string()
            } else {
                format!("{v:.1}")
            }
        });
        {
            let m = self.clone();
            fader.connect_value_changed(move |s| {
                if m.updating.get() {
                    return;
                }
                m.fader_changed(target, MixValue::VolumeDb(slider_to_db(s.value())));
            });
            let reset = gtk::GestureClick::new();
            let f = fader.clone();
            reset.connect_pressed(move |_, n, _, _| {
                if n == 2 {
                    f.set_value(0.0);
                }
            });
            fader.add_controller(reset);
        }
        let fader_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        fader_row.set_halign(gtk::Align::Center);
        fader_row.append(&fader);
        let mut meter = None;
        if matches!(target, Target::Track(_)) {
            let mt = Meter::new();
            fader_row.append(&mt);
            meter = Some(mt);
        }
        inner.append(&fader_row);

        let pan = gtk::Scale::with_range(gtk::Orientation::Horizontal, -1.0, 1.0, 0.01);
        pan.set_draw_value(false);
        pan.set_width_request(84);
        pan.add_mark(0.0, gtk::PositionType::Bottom, None);
        pan.set_tooltip_text(Some("Pan. Double-click centers."));
        pan.update_property(&[gtk::accessible::Property::Label(&format!("{name} pan"))]);
        {
            let m = self.clone();
            pan.connect_value_changed(move |s| {
                if m.updating.get() {
                    return;
                }
                let v = s.value();
                let v = if v.abs() < 0.03 { 0.0 } else { v };
                m.fader_changed(target, MixValue::Pan(v));
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
        inner.append(&pan);

        let mute = gtk::ToggleButton::with_label("M");
        mute.set_tooltip_text(Some("Mute"));
        mute.update_property(&[gtk::accessible::Property::Label(&format!("Mute {name}"))]);
        let solo = gtk::ToggleButton::with_label("S");
        solo.set_tooltip_text(Some("Solo"));
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
                m.discrete_mix(target, v);
            });
        }
        let ms = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        ms.set_halign(gtk::Align::Center);
        ms.append(&mute);
        ms.append(&solo);
        inner.append(&ms);

        match target {
            Target::Track(tid) => {
                if let Some(t) = proj.track(tid) {
                    for Insert::Clap(r) in &t.inserts {
                        inner.append(&self.insert_row(tid, r.instance, &r.plugin_id));
                    }
                }
                let add = gtk::Button::new();
                add.set_child(Some(
                    &adw::ButtonContent::builder()
                        .icon_name("list-add-symbolic")
                        .label("Effect")
                        .build(),
                ));
                add.set_tooltip_text(Some("Add a CLAP effect to this track"));
                let m = self.clone();
                add.connect_clicked(move |b| m.add_effect(b.upcast_ref(), tid));
                inner.append(&add);
            }
            Target::Channel(cid) => {
                let actions = gtk::Box::new(gtk::Orientation::Horizontal, 4);
                actions.set_halign(gtk::Align::Center);
                if let Some(c) = proj.channel(cid)
                    && let Instrument::Clap(r) = &c.instrument
                {
                    let gui = gtk::Button::from_icon_name("window-new-symbolic");
                    gui.set_tooltip_text(Some("Open the plugin window"));
                    let (m, inst, nm) = (self.clone(), r.instance, c.name.clone());
                    gui.connect_clicked(move |_| m.show_gui(inst, &nm));
                    actions.append(&gui);
                }
                let del = gtk::Button::from_icon_name("user-trash-symbolic");
                del.set_tooltip_text(Some("Remove this channel"));
                let m = self.clone();
                del.connect_clicked(move |_| {
                    m.app.edit(vec![Edit::RemoveChannel { channel: cid }]);
                });
                actions.append(&del);
                inner.append(&actions);
            }
        }

        self.strips.borrow_mut().push(Strip {
            target,
            fader,
            pan,
            mute,
            solo,
            route,
            meter,
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
        let l = gtk::Label::new(Some(&name));
        l.set_ellipsize(gtk::pango::EllipsizeMode::End);
        l.set_max_width_chars(9);
        l.set_xalign(0.0);
        l.set_hexpand(true);
        l.set_tooltip_text(Some(plugin_id));
        row.append(&l);
        let gui = gtk::Button::from_icon_name("window-new-symbolic");
        gui.add_css_class("flat");
        gui.set_tooltip_text(Some("Open the plugin window"));
        let (m, nm) = (self.clone(), name.clone());
        gui.connect_clicked(move |_| m.show_gui(inst, &nm));
        row.append(&gui);
        let del = gtk::Button::from_icon_name("edit-delete-symbolic");
        del.add_css_class("flat");
        del.set_tooltip_text(Some("Remove this effect"));
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
            "Add effect",
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

    fn mix_edit(target: Target, v: MixValue) -> Edit {
        match target {
            Target::Channel(c) => Edit::SetChannelMix {
                channel: c,
                value: v,
            },
            Target::Track(t) => Edit::SetTrackMix { track: t, value: v },
        }
    }

    fn discrete_mix(&self, target: Target, v: MixValue) {
        self.app.edit(vec![Self::mix_edit(target, v)]);
    }

    /// Fader and pan moves: one gesture until the control rests.
    fn fader_changed(self: &Rc<Mixer>, target: Target, v: MixValue) {
        let e = Self::mix_edit(target, v);
        if !self.app.session.borrow().editor.gesture_open() {
            self.app.gesture_begin("Mixer");
        }
        self.app.gesture_edit(vec![e]);
        if let Some(id) = self.gesture_timer.borrow_mut().take() {
            id.remove();
        }
        let m = self.clone();
        let id = glib::timeout_add_local_once(Duration::from_millis(500), move || {
            m.gesture_timer.borrow_mut().take();
            if m.app.session.borrow().editor.gesture_open() {
                m.app.gesture_end();
            }
        });
        *self.gesture_timer.borrow_mut() = Some(id);
    }

    /// Writes document values into the widgets without sending edits.
    fn update_values(&self) {
        let s = self.app.session.borrow();
        let p = &s.document().project;
        self.updating.set(true);
        for st in self.strips.borrow().iter() {
            let (mix, track_idx) = match st.target {
                Target::Channel(c) => match p.channel(c) {
                    Some(ch) => (ch.mix, p.tracks.iter().position(|t| t.id == ch.track)),
                    None => continue,
                },
                Target::Track(t) => match p.track(t) {
                    Some(tr) => (tr.mix, None),
                    None => continue,
                },
            };
            let want = db_to_slider(mix.volume_db);
            if (st.fader.value() - want).abs() > 1e-6 {
                st.fader.set_value(want);
            }
            if (st.pan.value() - mix.pan).abs() > 1e-6 {
                st.pan.set_value(mix.pan);
            }
            st.mute.set_active(mix.mute);
            st.solo.set_active(mix.solo);
            if let (Some(dd), Some(i)) = (&st.route, track_idx)
                && dd.selected() as usize != i
            {
                dd.set_selected(i as u32);
            }
        }
        self.updating.set(false);
    }

    fn feed_meters(&self) {
        let s = self.app.session.borrow();
        let status = s.link.status.clone();
        for st in self.strips.borrow().iter() {
            let (Target::Track(t), Some(m)) = (st.target, &st.meter) else {
                continue;
            };
            let Some((slot, _)) = s.slots.track_slot(t) else {
                continue;
            };
            let mut db = [-80.0f32; 2];
            for (ch, d) in db.iter_mut().enumerate() {
                let bits = status.track_peaks[slot.0 as usize * 2 + ch].swap(0, Ordering::Relaxed);
                *d = peak_to_db(f32::from_bits(bits));
            }
            m.update(db);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn signature_changes_with_structure_and_names_only() {
        use crate::document::{Document, apply};
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
