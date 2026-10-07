// SPDX-License-Identifier: GPL-3.0-or-later
//! The right utility pane (docs/ui-design.md 3.7, 3.9, 3.10): Sound, Agent,
//! and History pages behind a switcher bar. The Sound page shows the
//! selected channel: eight knobs and "More Controls" for the built-in synth,
//! the plugin's own window for a CLAP instrument.

use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use protocol::edit::Edit;
use protocol::ids::{ChannelId, InstanceId};
use protocol::model::{Instrument, SynthParam, SynthParams, Wave};

use crate::app::{App, UiCommand};
use crate::inspector_native::{Bass808Page, SamplerPage};
use crate::knob_logic::{MACROS, MORE, format_value, from_unit, to_unit, vary};
use crate::shortcuts;
use crate::widgets::color_bar::ColorBar;
use crate::widgets::knob::Knob;
use doc::presets;

pub struct Inspector {
    pub widget: gtk::Widget,
    pub stack: adw::ViewStack,
}

fn status(icon: &str, title: &str, description: &str) -> adw::StatusPage {
    let p = adw::StatusPage::new();
    p.set_icon_name(Some(icon));
    p.set_title(title);
    p.set_description(Some(description));
    p.add_css_class("compact");
    p
}

const WAVES: [(Wave, &str); 4] = [
    (Wave::Sine, "Sine"),
    (Wave::Triangle, "Triangle"),
    (Wave::Saw, "Saw"),
    (Wave::Square, "Square"),
];

/// The widgets of the Sound page that show values.
struct SoundPage {
    app: Rc<App>,
    stack: gtk::Stack,
    name: gtk::Label,
    engine: gtk::Label,
    bar: ColorBar,
    preset_label: gtk::Label,
    knobs: Vec<(Knob, gtk::Label)>,
    more: Vec<(SynthParam, gtk::Scale, gtk::Label)>,
    waves: [adw::ComboRow; 2],
    plugin_name: gtk::Label,
    expert: gtk::Button,
    no_window: gtk::Label,
    bass: Rc<Bass808Page>,
    samp: Rc<SamplerPage>,
    updating: Cell<bool>,
    seed: Cell<u64>,
    instance: Cell<Option<InstanceId>>,
}

pub fn build(app: &Rc<App>) -> Inspector {
    let stack = adw::ViewStack::new();
    let sound = SoundPage::new(app);
    stack.add_titled_with_icon(
        &sound.widget(),
        Some("sound"),
        "Sound",
        "audio-x-generic-symbolic",
    );
    let agent = crate::agent_panel::AgentPanel::new(app);
    stack.add_titled_with_icon(
        &agent.widget,
        Some("agent"),
        "Agent",
        "network-workgroup-symbolic",
    );
    // The pane's own header, level with the content header: its pages
    // are switched there.
    let switcher = adw::ViewSwitcher::builder()
        .policy(adw::ViewSwitcherPolicy::Narrow)
        .stack(&stack)
        .build();
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&switcher));
    let view = adw::ToolbarView::new();
    view.add_top_bar(&header);
    view.set_content(Some(&stack));
    Inspector {
        widget: view.upcast(),
        stack,
    }
}

impl SoundPage {
    fn widget(self: &Rc<SoundPage>) -> gtk::Widget {
        self.stack.clone().upcast()
    }

    fn new(app: &Rc<App>) -> Rc<SoundPage> {
        let stack = gtk::Stack::new();
        stack.add_named(
            &status(
                "audio-x-generic-symbolic",
                "No Sound Selected",
                "Choose a channel to change its sound.",
            ),
            Some("none"),
        );

        // ---- the built-in synth ----
        let synth = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let top = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        top.add_css_class("toolbar");
        let prev = gtk::Button::from_icon_name("go-previous-symbolic");
        prev.add_css_class("flat");
        prev.set_tooltip_text(Some("Previous Sound"));
        prev.update_property(&[gtk::accessible::Property::Label("Previous sound")]);
        let next = gtk::Button::from_icon_name("go-next-symbolic");
        next.add_css_class("flat");
        next.set_tooltip_text(Some("Next Sound"));
        next.update_property(&[gtk::accessible::Property::Label("Next sound")]);
        let preset_label = gtk::Label::new(Some("Custom"));
        preset_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        let preset_btn = gtk::Button::new();
        preset_btn.set_child(Some(&preset_label));
        preset_btn.add_css_class("flat");
        preset_btn.set_hexpand(true);
        preset_btn.set_tooltip_text(Some(&shortcuts::tooltip("Choose a Sound", "win.sounds")));
        preset_btn.update_property(&[gtk::accessible::Property::Label("Choose a sound")]);
        let vary_btn = gtk::Button::with_label("Vary");
        vary_btn.set_tooltip_text(Some("Try a Small Random Change"));
        top.append(&prev);
        top.append(&preset_btn);
        top.append(&next);
        top.append(&vary_btn);
        synth.append(&top);

        let col = gtk::Box::new(gtk::Orientation::Vertical, 18);
        col.set_margin_top(12);
        col.set_margin_bottom(12);
        col.set_margin_start(12);
        col.set_margin_end(12);

        // Channel header.
        let head = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let bar = ColorBar::new(0);
        bar.set_margin_top(2);
        bar.set_margin_bottom(2);
        let names = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let name = gtk::Label::new(None);
        name.add_css_class("heading");
        name.set_xalign(0.0);
        name.set_ellipsize(gtk::pango::EllipsizeMode::End);
        let engine = gtk::Label::new(Some("Built-in synth"));
        engine.add_css_class("caption");
        engine.add_css_class("dim-label");
        engine.set_xalign(0.0);
        names.append(&name);
        names.append(&engine);
        head.append(&bar);
        head.append(&names);
        col.append(&head);

        // Eight knobs, four per row.
        let grid = gtk::Grid::new();
        grid.set_row_spacing(12);
        grid.set_column_spacing(6);
        grid.set_halign(gtk::Align::Center);
        let mut knobs = Vec::new();
        for (i, spec) in MACROS.iter().enumerate() {
            let cell = gtk::Box::new(gtk::Orientation::Vertical, 2);
            cell.set_size_request(72, -1);
            let knob = Knob::new(spec.name);
            knob.set_tooltip_text(Some(spec.tip));
            let cap = gtk::Label::new(Some(spec.name));
            cap.add_css_class("caption");
            let read = gtk::Label::new(Some(" "));
            read.add_css_class("caption");
            read.add_css_class("numeric");
            read.add_css_class("dim-label");
            cell.append(&knob);
            cell.append(&cap);
            cell.append(&read);
            grid.attach(&cell, (i % 4) as i32, (i / 4) as i32, 1, 1);
            knobs.push((knob, read));
        }
        col.append(&grid);

        // More controls.
        let list = gtk::ListBox::new();
        list.add_css_class("boxed-list");
        list.set_selection_mode(gtk::SelectionMode::None);
        let expander = adw::ExpanderRow::new();
        expander.set_title("More Controls");
        let wave_names: Vec<&str> = WAVES.iter().map(|w| w.1).collect();
        let mk_wave = |title: &str| {
            let r = adw::ComboRow::new();
            r.set_title(title);
            r.set_model(Some(&gtk::StringList::new(&wave_names)));
            r
        };
        let waves = [mk_wave("Source 1 wave"), mk_wave("Source 2 wave")];
        expander.add_row(&waves[0]);
        expander.add_row(&waves[1]);
        let mut more = Vec::new();
        for (param, title) in MORE {
            let (lo, hi) = param.range();
            let step = ((hi - lo) / 200.0).max(0.001);
            let scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, lo, hi, step);
            scale.set_draw_value(false);
            scale.set_size_request(120, -1);
            scale.set_valign(gtk::Align::Center);
            scale.update_property(&[gtk::accessible::Property::Label(title)]);
            let read = gtk::Label::new(None);
            read.add_css_class("caption");
            read.add_css_class("numeric");
            read.set_width_chars(10);
            read.set_xalign(1.0);
            let row = adw::ActionRow::new();
            row.set_title(title);
            row.set_title_lines(2);
            row.add_suffix(&scale);
            row.add_suffix(&read);
            expander.add_row(&row);
            more.push((param, scale, read));
        }
        list.append(&expander);
        col.append(&list);
        let clamp = adw::Clamp::builder().maximum_size(420).child(&col).build();
        synth.append(
            &gtk::ScrolledWindow::builder()
                .child(&clamp)
                .vexpand(true)
                .hscrollbar_policy(gtk::PolicyType::Never)
                .build(),
        );
        stack.add_named(&synth, Some("synth"));

        // ---- a plugin instrument ----
        let plug = gtk::Box::new(gtk::Orientation::Vertical, 12);
        plug.set_margin_top(12);
        plug.set_margin_start(12);
        plug.set_margin_end(12);
        let plugin_name = gtk::Label::new(None);
        plugin_name.add_css_class("heading");
        plugin_name.set_xalign(0.0);
        let expert = gtk::Button::new();
        expert.set_child(Some(
            &adw::ButtonContent::builder()
                .icon_name("window-new-symbolic")
                .label("Advanced: Edit Sound…")
                .build(),
        ));
        expert.set_tooltip_text(Some("Opens the plugin's own window, for experts"));
        let no_window = gtk::Label::new(Some("This plugin has no window of its own."));
        no_window.add_css_class("dim-label");
        no_window.set_xalign(0.0);
        no_window.set_visible(false);
        plug.append(&plugin_name);
        plug.append(&expert);
        plug.append(&no_window);
        stack.add_named(&plug, Some("clap"));
        let bass = Bass808Page::new(app);
        stack.add_named(&bass.widget, Some("bass808"));
        let samp = SamplerPage::new(app);
        stack.add_named(&samp.widget, Some("sampler"));

        let page = Rc::new(SoundPage {
            app: app.clone(),
            stack,
            name,
            engine,
            bar,
            preset_label,
            knobs,
            more,
            waves,
            plugin_name,
            expert,
            no_window,
            bass,
            samp,
            updating: Cell::new(false),
            seed: Cell::new(0x9e37_79b9_7f4a_7c15),
            instance: Cell::new(None),
        });
        page.wire(&prev, &next, &preset_btn, &vary_btn);
        let p = page.clone();
        app.on_change(move || p.sync());
        page.sync();
        page
    }

    fn channel(&self) -> Option<ChannelId> {
        self.app.current_channel()
    }

    fn params(&self) -> Option<SynthParams> {
        let s = self.app.session.borrow();
        let c = s.document().project.channel(self.channel()?)?;
        match &c.instrument {
            Instrument::Synth(p) => Some(*p),
            Instrument::Clap(_) | Instrument::Sampler(_) | Instrument::Bass808(_) => None,
        }
    }

    fn wire(
        self: &Rc<SoundPage>,
        prev: &gtk::Button,
        next: &gtk::Button,
        preset_btn: &gtk::Button,
        vary_btn: &gtk::Button,
    ) {
        for (i, (knob, _)) in self.knobs.iter().enumerate() {
            let p = self.clone();
            let spec = MACROS[i];
            knob.connect_edited(move |u| {
                if let Some(ch) = p.channel() {
                    p.set_param(ch, spec.param, from_unit(&spec, u));
                }
            });
            knob.set_default_unit(to_unit(&spec, SynthParams::default().get(spec.param)));
        }
        for (param, scale, _) in &self.more {
            let (p, param) = (self.clone(), *param);
            scale.connect_value_changed(move |s| {
                if p.updating.get() {
                    return;
                }
                if let Some(ch) = p.channel() {
                    p.set_param(ch, param, s.value());
                }
            });
        }
        for (i, row) in self.waves.iter().enumerate() {
            let p = self.clone();
            row.connect_selected_notify(move |r| {
                if p.updating.get() {
                    return;
                }
                let wave = WAVES[(r.selected() as usize).min(3)].0;
                if let Some(ch) = p.channel() {
                    p.app.edit(vec![Edit::SetSynthWave {
                        channel: ch,
                        osc: i as u8 + 1,
                        wave,
                    }]);
                }
            });
        }
        let p = self.clone();
        prev.connect_clicked(move |_| p.step_preset(-1));
        let p = self.clone();
        next.connect_clicked(move |_| p.step_preset(1));
        let a = self.app.clone();
        preset_btn.connect_clicked(move |_| a.command(UiCommand::ShowSounds));
        let p = self.clone();
        vary_btn.connect_clicked(move |_| p.vary());
        let p = self.clone();
        self.expert.connect_clicked(move |_| {
            if let Some(inst) = p.instance.get() {
                let name = p.plugin_name.text().to_string();
                let r = p.app.session.borrow_mut().registry.show_gui(inst, &name);
                if let Err(e) = r {
                    p.app.toast(&format!("Cannot open the plugin window: {e}"));
                }
            }
        });
    }

    fn set_param(&self, ch: ChannelId, param: SynthParam, value: f64) {
        self.app.edit_resting(
            "Change sound",
            vec![Edit::SetSynthParam {
                channel: ch,
                param,
                value,
            }],
        );
    }

    /// Plays the next or previous built-in sound on this channel.
    fn step_preset(&self, dir: i32) {
        let (Some(ch), Some(cur)) = (self.channel(), self.params()) else {
            return;
        };
        let all = presets::presets();
        let n = all.len() as i32;
        let i = match presets::index_of(&cur) {
            Some(i) => (i as i32 + dir).rem_euclid(n),
            None => {
                if dir > 0 {
                    0
                } else {
                    n - 1
                }
            }
        } as usize;
        self.app.edit(presets::apply_edits(ch, &all[i].params));
        self.pulse(ch);
    }

    /// A small random change within the knobs' ranges: one undo step.
    fn vary(self: &Rc<SoundPage>) {
        let (Some(ch), Some(cur)) = (self.channel(), self.params()) else {
            return;
        };
        let seed = self.seed.get().wrapping_add(glib::monotonic_time() as u64);
        self.seed.set(seed);
        let edits: Vec<Edit> = vary(&cur, seed, 0.1)
            .into_iter()
            .map(|(param, value)| Edit::SetSynthParam {
                channel: ch,
                param,
                value,
            })
            .collect();
        if self.app.edit(edits).is_some() {
            let a = self.app.clone();
            self.app
                .toast_action("Sound varied", "Undo", move || a.undo());
            self.pulse(ch);
        }
    }

    fn pulse(&self, ch: ChannelId) {
        let key = {
            let s = self.app.session.borrow();
            s.document().project.channel(ch).map(|c| c.root_key)
        };
        if let Some(k) = key {
            self.app
                .preview_pulse(ch, k, doc::document::DEFAULT_STEP_VEL, 400);
        }
    }

    /// Writes the channel's values into the widgets.
    fn sync(&self) {
        let Some(ch) = self.channel() else {
            self.stack.set_visible_child_name("none");
            return;
        };
        let (name, slot, instr, channel) = {
            let s = self.app.session.borrow();
            let Some(c) = s.document().project.channel(ch) else {
                drop(s);
                self.stack.set_visible_child_name("none");
                return;
            };
            let slot = crate::palette::channel_slot(&s.document().project, c.id);
            (c.name.clone(), slot, c.instrument.clone(), c.clone())
        };
        self.updating.set(true);
        match instr {
            Instrument::Synth(p) => {
                self.stack.set_visible_child_name("synth");
                self.name.set_text(&name);
                self.bar.set_id(slot);
                self.engine.set_text("Built-in synth");
                let preset = presets::index_of(&p).map(|i| presets::presets()[i].name);
                self.preset_label.set_text(preset.unwrap_or("Custom"));
                for (i, (knob, read)) in self.knobs.iter().enumerate() {
                    let spec = &MACROS[i];
                    let v = p.get(spec.param);
                    knob.set_unit(to_unit(spec, v));
                    let t = format_value(spec.param, v);
                    read.set_text(&t);
                    knob.set_value_text(&t);
                }
                for (param, scale, read) in &self.more {
                    let v = p.get(*param);
                    if (scale.value() - v).abs() > 1e-9 {
                        scale.set_value(v);
                    }
                    read.set_text(&format_value(*param, v));
                }
                for (i, osc) in [p.osc1, p.osc2].iter().enumerate() {
                    let idx = WAVES.iter().position(|w| w.0 == osc.wave).unwrap_or(0);
                    if self.waves[i].selected() as usize != idx {
                        self.waves[i].set_selected(idx as u32);
                    }
                }
            }
            Instrument::Bass808(_) => {
                self.stack.set_visible_child_name("bass808");
                self.bass.sync(&channel, slot);
            }
            Instrument::Sampler(_) => {
                self.stack.set_visible_child_name("sampler");
                self.samp.sync(&channel, slot);
            }
            Instrument::Clap(r) => {
                self.stack.set_visible_child_name("clap");
                let title = {
                    let s = self.app.session.borrow();
                    s.registry
                        .find_desc(&r.plugin_id)
                        .map(|d| d.name.clone())
                        .unwrap_or_else(|| r.plugin_id.clone())
                };
                self.plugin_name.set_text(&format!("{name} - {title}"));
                self.instance.set(Some(r.instance));
                let has_gui = self.app.session.borrow().registry.gui_open(r.instance)
                    || self
                        .app
                        .session
                        .borrow()
                        .registry
                        .phase(r.instance)
                        .is_some();
                self.expert.set_sensitive(has_gui);
                self.no_window.set_visible(!has_gui);
            }
        }
        self.updating.set(false);
    }
}
