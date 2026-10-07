// SPDX-License-Identifier: GPL-3.0-or-later
//! The Sound pages of the 808 and the sampler (docs/ui-design.md 3.7, SPEC
//! 15.1, 15.2): a header, the structural settings as libadwaita rows, and
//! knobs for the continuous parameters. They are views of the document;
//! `sync` writes the channel's values in, and edits go through `App`.

use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;

use protocol::beats::{Bass808Param, SampleMode, SamplerParam};
use protocol::edit::Edit;
use protocol::ids::ChannelId;
use protocol::model::{Channel, Instrument};

use crate::app::App;
use crate::engine_adapter::SampleState;
use crate::native_logic::{bass808_specs, bass808_value, sampler_specs, sampler_value};
use crate::native_panel::ParamPanel;
use crate::samples_ui;
use crate::widgets::color_bar::ColorBar;

/// The name block at the top of a Sound page.
struct Header {
    widget: gtk::Box,
    bar: ColorBar,
    name: gtk::Label,
    kind: gtk::Label,
}

fn header(kind: &str) -> Header {
    let head = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let bar = ColorBar::new(0);
    bar.set_margin_top(2);
    bar.set_margin_bottom(2);
    let names = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let name = gtk::Label::new(None);
    name.add_css_class("heading");
    name.set_xalign(0.0);
    name.set_ellipsize(gtk::pango::EllipsizeMode::End);
    let k = gtk::Label::new(Some(kind));
    k.add_css_class("caption");
    k.add_css_class("dim-label");
    k.set_xalign(0.0);
    names.append(&name);
    names.append(&k);
    head.append(&bar);
    head.append(&names);
    Header {
        widget: head,
        bar,
        name,
        kind: k,
    }
}

fn page_column() -> gtk::Box {
    let col = gtk::Box::new(gtk::Orientation::Vertical, 18);
    col.set_margin_top(12);
    col.set_margin_bottom(12);
    col.set_margin_start(12);
    col.set_margin_end(12);
    col
}

fn scrolled(col: &gtk::Box) -> gtk::Widget {
    let clamp = adw::Clamp::builder().maximum_size(420).child(col).build();
    gtk::ScrolledWindow::builder()
        .child(&clamp)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .build()
        .upcast()
}

fn section_label(text: &str) -> gtk::Label {
    let l = gtk::Label::new(Some(text));
    l.add_css_class("caption-heading");
    l.set_xalign(0.0);
    l
}

// ---------------------------------------------------------------------------
// 808

pub struct Bass808Page {
    pub widget: gtk::Widget,
    head: Header,
    mono: adw::SwitchRow,
    panel: Rc<ParamPanel>,
    updating: Cell<bool>,
}

impl Bass808Page {
    pub fn new(app: &Rc<App>) -> Rc<Bass808Page> {
        let head = header("808 bass");
        let col = page_column();
        col.append(&head.widget);

        let list = gtk::ListBox::new();
        list.add_css_class("boxed-list");
        list.set_selection_mode(gtk::SelectionMode::None);
        let mono = adw::SwitchRow::new();
        mono.set_title("One note at a time");
        mono.set_subtitle("A new note slides from the last one (glide)");
        list.append(&mono);
        col.append(&list);

        let a = app.clone();
        let panel = ParamPanel::new(bass808_specs(), 0, 3, "808", move |i, value| {
            if let (Some(channel), Some(param)) = (a.current_channel(), Bass808Param::from_index(i))
            {
                a.edit_resting(
                    "Change sound",
                    vec![Edit::SetBass808Param {
                        channel,
                        param,
                        value,
                    }],
                );
            }
        });
        col.append(&panel.widget);

        let page = Rc::new(Bass808Page {
            widget: scrolled(&col),
            head,
            mono: mono.clone(),
            panel,
            updating: Cell::new(false),
        });
        let (p, a) = (page.clone(), app.clone());
        mono.connect_active_notify(move |r| {
            if p.updating.get() {
                return;
            }
            if let Some(channel) = a.current_channel() {
                a.edit(vec![Edit::SetBass808Mono {
                    channel,
                    mono: r.is_active(),
                }]);
            }
        });
        page
    }

    pub fn sync(&self, c: &Channel, slot: u32) {
        let Instrument::Bass808(b) = &c.instrument else {
            return;
        };
        self.updating.set(true);
        self.head.name.set_text(&c.name);
        self.head.bar.set_id(slot);
        if self.mono.is_active() != b.mono {
            self.mono.set_active(b.mono);
        }
        self.panel.set_values(|i| bass808_value(&b.params, i));
        self.updating.set(false);
    }
}

// ---------------------------------------------------------------------------
// Sampler

pub struct SamplerPage {
    pub widget: gtk::Widget,
    app: Rc<App>,
    head: Header,
    sample_row: adw::ActionRow,
    warn: gtk::Image,
    mode: adw::ComboRow,
    reverse: adw::SwitchRow,
    root: adw::SpinRow,
    panels: Vec<Rc<ParamPanel>>,
    updating: Cell<bool>,
}

impl SamplerPage {
    pub fn new(app: &Rc<App>) -> Rc<SamplerPage> {
        let head = header("Sampler");
        let col = page_column();
        col.append(&head.widget);

        let list = gtk::ListBox::new();
        list.add_css_class("boxed-list");
        list.set_selection_mode(gtk::SelectionMode::None);

        let sample_row = adw::ActionRow::new();
        sample_row.set_title("Sound");
        sample_row.set_subtitle_lines(2);
        let warn = gtk::Image::from_icon_name("dialog-warning-symbolic");
        warn.set_visible(false);
        warn.set_tooltip_text(Some("The sound file cannot be found"));
        let choose = gtk::Button::with_label("Choose…");
        choose.set_valign(gtk::Align::Center);
        choose.set_tooltip_text(Some("Choose a WAV File for This Instrument"));
        choose.update_property(&[gtk::accessible::Property::Label("Choose a sound file")]);
        sample_row.add_prefix(&warn);
        sample_row.add_suffix(&choose);
        list.append(&sample_row);

        let mode = adw::ComboRow::new();
        mode.set_title("Plays");
        mode.set_model(Some(&gtk::StringList::new(&[
            "Once, to the end",
            "Across the keyboard",
        ])));
        list.append(&mode);

        let root = adw::SpinRow::with_range(0.0, 127.0, 1.0);
        root.set_title("Original note");
        root.set_subtitle("The key the sound was recorded at (60 is middle C)");
        list.append(&root);

        let reverse = adw::SwitchRow::new();
        reverse.set_title("Play backwards");
        list.append(&reverse);
        col.append(&list);

        let specs = sampler_specs();
        let mut panels = Vec::new();
        for (title, from, to) in [
            ("Trim and pitch", 0usize, 4usize),
            ("Envelope", 4, 8),
            ("Level", 8, 9),
        ] {
            col.append(&section_label(title));
            let a = app.clone();
            let panel = ParamPanel::new(
                specs[from..to].to_vec(),
                from,
                4,
                "sampler",
                move |i, value| {
                    if let (Some(channel), Some(param)) =
                        (a.current_channel(), SamplerParam::from_index(i))
                    {
                        a.edit_resting(
                            "Change sound",
                            vec![Edit::SetSamplerParam {
                                channel,
                                param,
                                value,
                            }],
                        );
                    }
                },
            );
            col.append(&panel.widget);
            panels.push(panel);
        }

        let page = Rc::new(SamplerPage {
            widget: scrolled(&col),
            app: app.clone(),
            head,
            sample_row,
            warn,
            mode: mode.clone(),
            reverse: reverse.clone(),
            root: root.clone(),
            panels,
            updating: Cell::new(false),
        });

        let p = page.clone();
        choose.connect_clicked(move |b| {
            if let Some(ch) = p.app.current_channel() {
                samples_ui::choose_for_channel(b, &p.app, ch);
            }
        });
        let p = page.clone();
        let mode_or_reverse = move || {
            if p.updating.get() {
                return;
            }
            if let Some(channel) = p.app.current_channel() {
                let mode = if p.mode.selected() == 1 {
                    SampleMode::Pitched
                } else {
                    SampleMode::OneShot
                };
                p.app.edit(vec![Edit::SetSamplerMode {
                    channel,
                    mode,
                    reverse: p.reverse.is_active(),
                }]);
            }
        };
        let f = mode_or_reverse.clone();
        mode.connect_selected_notify(move |_| f());
        reverse.connect_active_notify(move |_| mode_or_reverse());
        let p = page.clone();
        root.connect_value_notify(move |r| {
            if p.updating.get() {
                return;
            }
            if let Some(channel) = p.app.current_channel() {
                p.app.edit_resting(
                    "Change sound",
                    vec![Edit::SetRootKey {
                        channel,
                        key: r.value().round().clamp(0.0, 127.0) as u8,
                    }],
                );
            }
        });
        page
    }

    pub fn sync(&self, c: &Channel, slot: u32) {
        let Instrument::Sampler(sm) = &c.instrument else {
            return;
        };
        self.updating.set(true);
        self.head.name.set_text(&c.name);
        self.head.bar.set_id(slot);
        self.head.kind.set_text("Sampler");
        let (title, missing, loading) = {
            let s = self.app.session.borrow();
            let p = &s.document().project;
            match &sm.sample {
                None => ("No sound chosen".to_string(), false, false),
                Some(h) => {
                    let name = p
                        .samples
                        .iter()
                        .find(|r| &r.hash == h)
                        .map(|r| r.orig_name.clone())
                        .unwrap_or_else(|| "Unknown sound".to_string());
                    let missing = s.missing_samples.contains(h)
                        || matches!(s.store.state(h), Some(SampleState::Failed(_)));
                    let loading = matches!(s.store.state(h), Some(SampleState::Loading));
                    (name, missing, loading)
                }
            }
        };
        let subtitle = if missing {
            format!("{title} (file missing, choose it again)")
        } else if loading {
            format!("{title} (loading)")
        } else {
            title
        };
        self.sample_row.set_subtitle(&subtitle);
        self.warn.set_visible(missing);
        let want = if sm.mode == SampleMode::Pitched { 1 } else { 0 };
        if self.mode.selected() != want {
            self.mode.set_selected(want);
        }
        if self.reverse.is_active() != sm.reverse {
            self.reverse.set_active(sm.reverse);
        }
        self.root.set_visible(sm.mode == SampleMode::Pitched);
        if (self.root.value() - c.root_key as f64).abs() > 0.5 {
            self.root.set_value(c.root_key as f64);
        }
        for p in &self.panels {
            p.set_values(|i| sampler_value(&sm.params, i));
        }
        self.updating.set(false);
    }

    pub fn channel_of(&self) -> Option<ChannelId> {
        self.app.current_channel()
    }
}

/// The Sound page of an Audio row (SPEC 21.1): the row's Volume, and the
/// Gain of the selected sound.
pub struct AudioPage {
    pub widget: gtk::Widget,
    app: Rc<App>,
    head: Header,
    volume: adw::SpinRow,
    gain: adw::SpinRow,
    updating: Cell<bool>,
}

impl AudioPage {
    pub fn new(app: &Rc<App>) -> Rc<AudioPage> {
        let head = header("Audio row");
        let col = page_column();
        col.append(&head.widget);
        let list = gtk::ListBox::new();
        list.add_css_class("boxed-list");
        list.set_selection_mode(gtk::SelectionMode::None);
        let volume = adw::SpinRow::with_range(-60.0, 6.0, 0.5);
        volume.set_title("Volume");
        volume.set_subtitle("How loud this row is, in dB (0 is unchanged)");
        volume.set_digits(1);
        list.append(&volume);
        let gain = adw::SpinRow::with_range(-24.0, 24.0, 0.5);
        gain.set_title("Gain");
        gain.set_subtitle("Makes the selected sound louder or softer, in dB");
        gain.set_digits(1);
        list.append(&gain);
        col.append(&list);
        let page = Rc::new(AudioPage {
            widget: scrolled(&col),
            app: app.clone(),
            head,
            volume,
            gain,
            updating: Cell::new(false),
        });
        let p = page.clone();
        page.volume.connect_value_notify(move |r| {
            if p.updating.get() {
                return;
            }
            let (Some(ch), v) = (p.app.current_channel(), r.value()) else {
                return;
            };
            let track = {
                let s = p.app.session.borrow();
                s.document().project.channel(ch).map(|c| c.track)
            };
            if let Some(track) = track {
                p.app.edit_resting(
                    "Change volume",
                    vec![Edit::SetTrackMix {
                        track,
                        value: protocol::edit::MixValue::VolumeDb(v),
                    }],
                );
            }
        });
        let p = page.clone();
        page.gain.connect_value_notify(move |r| {
            if p.updating.get() {
                return;
            }
            let Some(clip) = p.selected_audio_clip() else {
                return;
            };
            if let Some(a) = clip.audio {
                let mdb = crate::audio_clips::gain_mdb(r.value());
                if mdb != a.gain_mdb {
                    p.app.edit_resting(
                        "Change gain",
                        vec![Edit::SetClipAudio {
                            clip: clip.id,
                            gain_mdb: mdb,
                            fade_in: a.fade_in,
                            fade_out: a.fade_out,
                        }],
                    );
                }
            }
        });
        page
    }

    /// The selected clip, when it plays audio.
    fn selected_audio_clip(&self) -> Option<protocol::model::Clip> {
        let id = self.app.current_clip()?;
        let s = self.app.session.borrow();
        s.document()
            .project
            .clips
            .iter()
            .find(|c| c.id == id && c.audio.is_some())
            .copied()
    }

    pub fn sync(&self, c: &Channel, slot: u32) {
        if !matches!(c.instrument, Instrument::Audio) {
            return;
        }
        self.updating.set(true);
        self.head.name.set_text(&c.name);
        self.head.bar.set_id(slot);
        let vol = {
            let s = self.app.session.borrow();
            s.document()
                .project
                .track(c.track)
                .map_or(0.0, |t| t.mix.volume_db)
        };
        if (self.volume.value() - vol).abs() > 1e-9 {
            self.volume.set_value(vol);
        }
        let clip = self.selected_audio_clip();
        self.gain.set_sensitive(clip.is_some());
        let g = clip
            .and_then(|c| c.audio)
            .map_or(0.0, |a| a.gain_mdb as f64 / 1000.0);
        if (self.gain.value() - g).abs() > 1e-9 {
            self.gain.set_value(g);
        }
        self.updating.set(false);
    }
}
