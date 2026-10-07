// SPDX-License-Identifier: GPL-3.0-or-later
//! Preferences (docs/ui-design.md 3.11) as an `AdwPreferencesDialog`.
//! Every change is saved at once; there is no Apply button.

use std::rc::Rc;

use adw::prelude::*;

use crate::app::App;
use crate::engine_adapter::{self, Host};
use crate::settings::{BUFFER_SIZES, ColorScheme};

/// Applies the color scheme setting to the style manager.
pub fn apply_color_scheme(c: ColorScheme) {
    adw::StyleManager::default().set_color_scheme(match c {
        ColorScheme::System => adw::ColorScheme::Default,
        ColorScheme::Light => adw::ColorScheme::ForceLight,
        ColorScheme::Dark => adw::ColorScheme::ForceDark,
    });
}

fn save(app: &App) {
    if let Err(e) = app.settings.borrow().write(&app.dirs) {
        app.toast(&format!("Could not save the preferences: {e}"));
    }
}

fn combo(title: &str, items: &[&str], selected: u32) -> adw::ComboRow {
    let row = adw::ComboRow::new();
    row.set_title(title);
    row.set_model(Some(&gtk::StringList::new(items)));
    row.set_selected(selected);
    row
}

pub fn show(window: &adw::ApplicationWindow, app: &Rc<App>) {
    let dialog = adw::PreferencesDialog::new();
    dialog.set_title("Preferences");
    dialog.set_search_enabled(false);

    // ---- Audio ----
    let audio = adw::PreferencesPage::new();
    audio.set_title("Audio");
    audio.set_icon_name(Some("audio-speakers-symbolic"));

    let out = adw::PreferencesGroup::new();
    out.set_title("Output");
    out.set_description(Some("Changes take effect the next time LibreDAW starts."));
    let mut devices = engine_adapter::devices(Host::PipeWire);
    if devices.is_empty() {
        devices = engine_adapter::devices(Host::Alsa);
    }
    let mut names = vec!["System Default".to_string()];
    names.extend(devices.iter().cloned());
    let current = app.settings.borrow().output_device.clone();
    let sel = current
        .as_ref()
        .and_then(|d| devices.iter().position(|x| x == d))
        .map(|i| i as u32 + 1)
        .unwrap_or(0);
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let device = combo("Output Device", &refs, sel);
    {
        let a = app.clone();
        let devs = devices.clone();
        device.connect_selected_notify(move |r| {
            let i = r.selected() as usize;
            a.settings.borrow_mut().output_device = if i == 0 {
                None
            } else {
                devs.get(i - 1).cloned()
            };
            save(&a);
        });
    }
    out.add(&device);

    let sizes: Vec<String> = std::iter::once("Automatic".to_string())
        .chain(BUFFER_SIZES.iter().map(|n| format!("{n} frames")))
        .collect();
    let refs: Vec<&str> = sizes.iter().map(String::as_str).collect();
    let cur = app.settings.borrow().buffer_frames;
    let sel = BUFFER_SIZES
        .iter()
        .position(|n| *n == cur)
        .map(|i| i as u32 + 1)
        .unwrap_or(0);
    let buffer = combo("Buffer Size", &refs, sel);
    buffer.set_subtitle("Smaller buffers respond faster but need more from your computer");
    {
        let a = app.clone();
        buffer.connect_selected_notify(move |r| {
            let i = r.selected() as usize;
            a.settings.borrow_mut().buffer_frames = if i == 0 { 0 } else { BUFFER_SIZES[i - 1] };
            save(&a);
        });
    }
    out.add(&buffer);

    let rate = adw::ActionRow::new();
    rate.set_title("Sample Rate");
    let hz = app.session.borrow().link.sample_rate().round() as u32;
    rate.set_subtitle(&format!("{hz} Hz"));
    rate.add_css_class("property");
    out.add(&rate);
    audio.add(&out);

    let editing = adw::PreferencesGroup::new();
    editing.set_title("Editing");
    let preview = adw::SwitchRow::new();
    preview.set_title("Preview Notes");
    preview.set_subtitle("Hear a sound when you click a step, a piano key, or a note");
    preview.set_active(app.settings.borrow().preview_notes);
    {
        let a = app.clone();
        preview.connect_active_notify(move |r| {
            a.settings.borrow_mut().preview_notes = r.is_active();
            if !r.is_active() {
                a.preview_off();
            }
            save(&a);
        });
    }
    editing.add(&preview);
    audio.add(&editing);
    dialog.add(&audio);

    // ---- Appearance ----
    let look = adw::PreferencesPage::new();
    look.set_title("Appearance");
    look.set_icon_name(Some("applications-graphics-symbolic"));
    let g = adw::PreferencesGroup::new();
    let labels: Vec<&str> = ColorScheme::ALL.iter().map(|c| c.label()).collect();
    let cur = app.settings.borrow().color_scheme;
    let scheme = combo(
        "Color Scheme",
        &labels,
        ColorScheme::ALL.iter().position(|c| *c == cur).unwrap_or(0) as u32,
    );
    {
        let a = app.clone();
        scheme.connect_selected_notify(move |r| {
            let c = ColorScheme::ALL[(r.selected() as usize).min(2)];
            a.settings.borrow_mut().color_scheme = c;
            apply_color_scheme(c);
            save(&a);
        });
    }
    g.add(&scheme);
    look.add(&g);
    dialog.add(&look);

    dialog.present(Some(window));
}
