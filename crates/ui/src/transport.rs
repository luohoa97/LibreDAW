// SPDX-License-Identifier: GPL-3.0-or-later
//! The transport bar under the header bar (docs/ui-design.md 5): skip back,
//! play and stop, metronome, position, tempo, tap, and time signature. At
//! compact widths the extras fold into a "Transport Settings" popover; at
//! narrow widths the tempo button opens the same popover.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use protocol::consts::{MAX_TEMPO_BPM, MAX_TIME_SIG_NUM, MIN_TEMPO_BPM, MIN_TIME_SIG_NUM};
use protocol::edit::Edit;

use crate::app::{App, MeterUser};
use crate::shortcuts;
use crate::size_class::SizeClass;
use crate::transport_logic::{
    PositionFormat, TapTempo, format_position, parse_tempo, spoken_position,
};
use crate::widgets::meter::{Meter, peak_to_db};

pub struct Transport {
    pub bar: gtk::Box,
    app: Rc<App>,
    play: gtk::ToggleButton,
    metro: gtk::ToggleButton,
    position: gtk::Label,
    tempo: gtk::SpinButton,
    tempo_btn: gtk::MenuButton,
    beats_btn: gtk::MenuButton,
    // The shared popover's rows (compact and narrow).
    pop_tempo: adw::SpinRow,
    pop_beats: adw::SpinRow,
    pop_metro: adw::SwitchRow,
    // Groups shown or hidden by size class.
    extras: gtk::Box,
    tempo_group: gtk::Box,
    settings_btn: gtk::MenuButton,
    pop: gtk::Popover,
    master: Meter,
    master_btn: gtk::Button,
    metro_shown: Cell<bool>,
    updating: Cell<bool>,
    fmt: Cell<PositionFormat>,
    taps: RefCell<TapTempo>,
    last_text: RefCell<String>,
}

fn flat_icon(icon: &str, tip: &str, label: &str) -> gtk::Button {
    let b = gtk::Button::from_icon_name(icon);
    b.add_css_class("flat");
    b.set_tooltip_text(Some(tip));
    b.update_property(&[gtk::accessible::Property::Label(label)]);
    b
}

fn popover_list() -> gtk::ListBox {
    let l = gtk::ListBox::new();
    l.add_css_class("boxed-list");
    l.set_selection_mode(gtk::SelectionMode::None);
    l.set_margin_top(6);
    l.set_margin_bottom(6);
    l.set_margin_start(6);
    l.set_margin_end(6);
    l.set_width_request(280);
    l
}

impl Transport {
    pub fn new(app: &Rc<App>) -> Rc<Transport> {
        let bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        bar.add_css_class("toolbar");
        bar.set_accessible_role(gtk::AccessibleRole::Group);
        bar.update_property(&[gtk::accessible::Property::Label("Transport")]);

        let back = flat_icon(
            "media-skip-backward-symbolic",
            &shortcuts::tooltip("Go to Start", "win.go-start"),
            "Go to Start",
        );
        back.set_action_name(Some("win.go-start"));

        let play = gtk::ToggleButton::new();
        play.set_icon_name("media-playback-start-symbolic");
        play.add_css_class("suggested-action");
        play.set_tooltip_text(Some(&shortcuts::tooltip("Play", "win.play-pause")));
        play.update_property(&[gtk::accessible::Property::Label("Play")]);

        let metro = gtk::ToggleButton::new();
        metro.set_icon_name("alarm-symbolic");
        metro.add_css_class("flat");
        metro.set_tooltip_text(Some(&shortcuts::tooltip("Metronome", "win.metronome")));
        metro.update_property(&[gtk::accessible::Property::Label("Metronome")]);

        let position = gtk::Label::new(Some("001:1:1"));
        position.add_css_class("numeric");
        position.add_css_class("heading");
        position.add_css_class("ldaw-position");
        position.set_width_chars(7);
        let pos_btn = gtk::Button::new();
        pos_btn.set_child(Some(&position));
        pos_btn.add_css_class("flat");
        pos_btn.set_tooltip_text(Some("Position - click to switch between bars and time"));
        pos_btn.update_property(&[
            gtk::accessible::Property::Label("Position"),
            gtk::accessible::Property::ValueText("Bar 1, beat 1, step 1"),
        ]);

        let tempo = gtk::SpinButton::with_range(MIN_TEMPO_BPM, MAX_TEMPO_BPM, 1.0);
        tempo.set_digits(1);
        tempo.set_width_chars(5);
        tempo.add_css_class("numeric");
        tempo.set_tooltip_text(Some("Tempo"));
        tempo.update_property(&[gtk::accessible::Property::Label(
            "Tempo in beats per minute",
        )]);
        let bpm = gtk::Label::new(Some("BPM"));
        bpm.add_css_class("dim-label");
        let tempo_group = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        tempo_group.append(&tempo);
        tempo_group.append(&bpm);

        // Narrow: the tempo is one button that opens the settings popover.
        let tempo_btn = gtk::MenuButton::new();
        tempo_btn.add_css_class("flat");
        tempo_btn.set_label("120");
        tempo_btn.set_tooltip_text(Some("Tempo"));
        tempo_btn.update_property(&[gtk::accessible::Property::Label("Tempo and settings")]);
        tempo_btn.set_visible(false);

        let tap = gtk::Button::with_label("Tap");
        tap.add_css_class("flat");
        tap.set_tooltip_text(Some("Tap the Tempo"));
        let beats_btn = gtk::MenuButton::new();
        beats_btn.add_css_class("flat");
        beats_btn.set_label("4/4");
        beats_btn.set_tooltip_text(Some("Time Signature"));
        beats_btn.update_property(&[gtk::accessible::Property::Label("Time signature")]);
        let extras = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        extras.append(&tap);
        extras.append(&beats_btn);

        // The settings popover: used by the compact menu and the narrow
        // tempo button. The beats button has its own small popover.
        let list = popover_list();
        let pop_tempo = adw::SpinRow::with_range(MIN_TEMPO_BPM, MAX_TEMPO_BPM, 1.0);
        pop_tempo.set_title("Tempo");
        pop_tempo.set_digits(1);
        let pop_beats =
            adw::SpinRow::with_range(MIN_TIME_SIG_NUM as f64, MAX_TIME_SIG_NUM as f64, 1.0);
        pop_beats.set_title("Beats per Bar");
        let pop_metro = adw::SwitchRow::new();
        pop_metro.set_title("Metronome");
        let tap_row = adw::ActionRow::new();
        tap_row.set_title("Tap the Tempo");
        tap_row.set_activatable(true);
        list.append(&pop_tempo);
        list.append(&pop_beats);
        list.append(&pop_metro);
        list.append(&tap_row);
        let pop = gtk::Popover::new();
        pop.set_child(Some(&list));
        let settings_btn = gtk::MenuButton::new();
        settings_btn.set_icon_name("view-more-symbolic");
        settings_btn.add_css_class("flat");
        settings_btn.set_tooltip_text(Some("Transport Settings"));
        settings_btn.update_property(&[gtk::accessible::Property::Label("Transport settings")]);
        settings_btn.set_popover(Some(&pop));
        settings_btn.set_visible(false);
        // A MenuButton owns its popover: `apply_size` moves this one
        // between the settings button and the narrow tempo button.

        // Time signature popover (wide).
        let beats_list = popover_list();
        let beats_row =
            adw::SpinRow::with_range(MIN_TIME_SIG_NUM as f64, MAX_TIME_SIG_NUM as f64, 1.0);
        beats_row.set_title("Beats per Bar");
        beats_row.set_subtitle("Every beat is a quarter note");
        beats_list.append(&beats_row);
        let beats_pop = gtk::Popover::new();
        beats_pop.set_child(Some(&beats_list));
        beats_btn.set_popover(Some(&beats_pop));

        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        spacer.set_hexpand(true);

        bar.append(&back);
        bar.append(&play);
        bar.append(&metro);
        // Groups are set apart by space, not by lines.
        pos_btn.set_margin_start(12);
        bar.append(&pos_btn);
        tempo_group.set_margin_start(12);
        bar.append(&tempo_group);
        bar.append(&tempo_btn);
        bar.append(&extras);
        bar.append(&settings_btn);
        bar.append(&spacer);
        let master = Meter::new_horizontal();
        master.set_label("Master level");
        master.set_size_request(96, 12);
        master.set_valign(gtk::Align::Center);
        let master_btn = gtk::Button::new();
        master_btn.set_child(Some(&master));
        master_btn.add_css_class("flat");
        master_btn.set_action_name(Some("win.view-mixer"));
        master_btn.set_tooltip_text(Some("Master Level - click to open the Mixer"));
        master_btn.update_property(&[gtk::accessible::Property::Label("Open the mixer")]);
        bar.append(&master_btn);

        let t = Rc::new(Transport {
            bar,
            app: app.clone(),
            play,
            metro,
            position,
            tempo,
            tempo_btn,
            beats_btn,
            pop_tempo,
            pop_beats,
            pop_metro,
            extras,
            tempo_group,
            settings_btn,
            pop,
            master,
            master_btn,
            metro_shown: Cell::new(true),
            updating: Cell::new(false),
            fmt: Cell::new(PositionFormat::Bars),
            taps: RefCell::new(TapTempo::default()),
            last_text: RefCell::new(String::new()),
        });
        t.wire(&pos_btn, &tap, &tap_row, &beats_row);
        t.sync();
        t
    }

    fn wire(
        self: &Rc<Transport>,
        pos_btn: &gtk::Button,
        tap: &gtk::Button,
        tap_row: &adw::ActionRow,
        beats_row: &adw::SpinRow,
    ) {
        let t = self.clone();
        self.play.connect_toggled(move |b| {
            if t.updating.get() {
                return;
            }
            let playing = t.app.ui.borrow().playing;
            if b.is_active() != playing {
                t.app.toggle_play();
            }
        });
        let t = self.clone();
        self.metro.connect_toggled(move |b| {
            if !t.updating.get() {
                t.set_metronome(b.is_active());
            }
        });
        let t = self.clone();
        self.pop_metro.connect_active_notify(move |r| {
            if !t.updating.get() {
                t.set_metronome(r.is_active());
            }
        });
        let t = self.clone();
        pos_btn.connect_clicked(move |_| {
            t.fmt.set(t.fmt.get().toggle());
            t.last_text.borrow_mut().clear();
            t.update_position();
        });

        // Tempo: typed text is validated; out-of-range text reverts and a
        // toast says why (no modal).
        let t = self.clone();
        self.tempo.connect_input(move |s| {
            match parse_tempo(&s.text(), MIN_TEMPO_BPM, MAX_TEMPO_BPM) {
                Some(v) => Some(Ok(v)),
                None => {
                    t.app.toast(&format!(
                        "Tempo must be between {} and {} BPM",
                        MIN_TEMPO_BPM as u32, MAX_TEMPO_BPM as u32
                    ));
                    s.add_css_class("error");
                    let s2 = s.clone();
                    glib::timeout_add_local_once(std::time::Duration::from_secs(1), move || {
                        s2.remove_css_class("error")
                    });
                    Some(Err(()))
                }
            }
        });
        let t = self.clone();
        self.tempo.connect_value_changed(move |s| {
            if !t.updating.get() {
                t.set_tempo(s.value());
            }
        });
        let t = self.clone();
        self.pop_tempo.connect_value_notify(move |r| {
            if !t.updating.get() {
                t.set_tempo(r.value());
            }
        });
        for row in [beats_row.clone(), self.pop_beats.clone()] {
            let t = self.clone();
            row.connect_value_notify(move |r| {
                if !t.updating.get() {
                    t.app.edit(vec![Edit::SetTimeSigNum {
                        num: r.value() as u8,
                    }]);
                }
            });
        }
        let t = self.clone();
        let do_tap = move || {
            let now = glib::monotonic_time() as f64 / 1000.0;
            let v = t.taps.borrow_mut().tap(now);
            if let Some(v) = v {
                t.set_tempo(v.clamp(MIN_TEMPO_BPM, MAX_TEMPO_BPM).round());
            }
        };
        let dt = do_tap.clone();
        tap.connect_clicked(move |_| dt());
        tap_row.connect_activated(move |_| do_tap());

        // Keep the readout and the buttons in step.
        let t = self.clone();
        self.app.on_change(move || t.sync());
        let t = Rc::downgrade(self);
        self.bar.add_tick_callback(move |_, _| {
            if let Some(t) = t.upgrade() {
                t.update_position();
                glib::ControlFlow::Continue
            } else {
                glib::ControlFlow::Break
            }
        });
    }

    fn set_metronome(&self, on: bool) {
        let gain = self
            .app
            .session
            .borrow()
            .document()
            .project
            .metronome
            .gain_db;
        self.app.edit(vec![Edit::SetMetronome {
            enabled: on,
            gain_db: gain,
        }]);
    }

    fn set_tempo(&self, bpm: f64) {
        let cur = self.app.session.borrow().document().project.tempo_bpm;
        if (cur - bpm).abs() > 1e-9 {
            self.app.edit(vec![Edit::SetTempo { bpm }]);
        }
    }

    /// Writes document and engine state into the widgets.
    pub fn sync(&self) {
        self.updating.set(true);
        let (tempo, beats, metro) = {
            let s = self.app.session.borrow();
            let p = &s.document().project;
            (p.tempo_bpm, p.time_sig_num, p.metronome.enabled)
        };
        if (self.tempo.value() - tempo).abs() > 1e-9 {
            self.tempo.set_value(tempo);
        }
        if (self.pop_tempo.value() - tempo).abs() > 1e-9 {
            self.pop_tempo.set_value(tempo);
        }
        self.tempo_btn
            .set_label(&format!("{}", tempo.round() as i64));
        self.pop_beats.set_value(beats as f64);
        self.beats_btn.set_label(&format!("{beats}/4"));
        if let Some(pop) = self.beats_btn.popover()
            && let Some(l) = pop.child().and_then(|c| c.first_child())
            && let Ok(row) = l.downcast::<adw::SpinRow>()
        {
            row.set_value(beats as f64);
        }
        self.metro.set_active(metro);
        self.pop_metro.set_active(metro);
        let (playing, played_once) = {
            let ui = self.app.ui.borrow();
            (ui.playing, ui.played_once)
        };
        self.play.set_active(playing);
        self.play.set_icon_name(if playing {
            "media-playback-stop-symbolic"
        } else {
            "media-playback-start-symbolic"
        });
        let tip = if playing { "Stop" } else { "Play" };
        self.play
            .set_tooltip_text(Some(&shortcuts::tooltip(tip, "win.play-pause")));
        self.play
            .update_property(&[gtk::accessible::Property::Label(tip)]);
        // The one suggested action until the first play of this project.
        if played_once {
            self.play.remove_css_class("suggested-action");
            self.play.add_css_class("flat");
        } else {
            self.play.remove_css_class("flat");
            self.play.add_css_class("suggested-action");
        }
        self.updating.set(false);
        self.update_position();
    }

    fn update_position(&self) {
        let p = self
            .app
            .take_peaks(protocol::ids::TrackId::MASTER, MeterUser::Transport);
        self.master.update([peak_to_db(p[0]), peak_to_db(p[1])]);
        let (tick, beats, bpm) = {
            let s = self.app.session.borrow();
            let p = &s.document().project;
            (self.app.playhead_tick(), p.time_sig_num, p.tempo_bpm)
        };
        let text = format_position(tick, beats, bpm, self.fmt.get());
        if *self.last_text.borrow() != text {
            self.position.set_text(&text);
            self.position
                .update_property(&[gtk::accessible::Property::ValueText(&spoken_position(
                    tick, beats,
                ))]);
            *self.last_text.borrow_mut() = text;
        }
    }

    /// Declarative changes at compact widths (they lower the bar's minimum
    /// width, so they must be breakpoint setters).
    pub fn add_compact_setters(&self, bp: &adw::Breakpoint) {
        bp.add_setter(&self.extras, "visible", Some(&false.to_value()));
        bp.add_setter(&self.settings_btn, "visible", Some(&true.to_value()));
    }

    /// Declarative changes at narrow widths.
    pub fn add_narrow_setters(&self, bp: &adw::Breakpoint) {
        bp.add_setter(&self.tempo_group, "visible", Some(&false.to_value()));
        bp.add_setter(&self.tempo_btn, "visible", Some(&true.to_value()));
        bp.add_setter(&self.metro, "visible", Some(&false.to_value()));
        bp.add_setter(&self.settings_btn, "visible", Some(&false.to_value()));
        bp.add_setter(&self.master_btn, "visible", Some(&false.to_value()));
    }

    /// Moves the settings popover to the button that is visible.
    pub fn apply_size(&self, c: SizeClass) {
        let narrow = c.width == crate::size_class::Width::Narrow;
        if narrow {
            self.settings_btn.set_popover(None::<&gtk::Popover>);
            self.tempo_btn.set_popover(Some(&self.pop));
        } else {
            self.tempo_btn.set_popover(None::<&gtk::Popover>);
            self.settings_btn.set_popover(Some(&self.pop));
        }
        self.metro_shown.set(!narrow);
    }
}
