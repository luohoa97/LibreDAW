// SPDX-License-Identifier: GPL-3.0-or-later
//! Export Audio (docs/ui-design.md 2.3, 3.6): a small dialog over the
//! offline render and the WAV writer. The render runs on a worker thread
//! (SPEC 3.1); the dialog shows progress and a Cancel button.

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

use adw::prelude::*;
use gtk::{gio, glib};

use protocol::control::WavFormat;
use protocol::model::Instrument;

use crate::app::App;
use crate::engine_adapter::{self, RenderJob};
use crate::files;

pub const FORMATS: [(&str, WavFormat); 3] = [
    ("16-bit Integer", WavFormat::Pcm16),
    ("24-bit Integer", WavFormat::Pcm24),
    ("32-bit Float", WavFormat::Float32),
];

/// Suggested file name for an export of the project.
pub fn export_name(project_name: &str) -> String {
    let base: String = project_name
        .chars()
        .map(|c| if c == '/' || c == '\\' { '-' } else { c })
        .collect();
    let base = base.trim();
    format!("{}.wav", if base.is_empty() { "Export" } else { base })
}

/// Whether any channel or insert is a CLAP plugin (not part of exports yet).
pub fn has_plugins(p: &protocol::model::Project) -> bool {
    p.channels
        .iter()
        .any(|c| matches!(c.instrument, Instrument::Clap(_)))
        || p.tracks.iter().any(|t| {
            t.inserts
                .iter()
                .any(|i| matches!(i, protocol::model::Insert::Clap(_)))
        })
}

pub fn show(window: &adw::ApplicationWindow, app: &Rc<App>) {
    let Some(pattern) = app.current_pattern() else {
        app.toast("Add a pattern before exporting");
        return;
    };
    let plugins = has_plugins(&app.session.borrow().document().project);

    let dialog = adw::Dialog::builder()
        .title("Export Audio")
        .content_width(400)
        .build();
    let view = adw::ToolbarView::new();
    view.add_top_bar(&adw::HeaderBar::new());
    let body = gtk::Box::new(gtk::Orientation::Vertical, 18);
    body.set_margin_top(12);
    body.set_margin_bottom(24);
    body.set_margin_start(12);
    body.set_margin_end(12);

    let group = adw::PreferencesGroup::new();
    if plugins {
        group.set_description(Some(
            "Plugin instruments and effects are not included in exports yet.",
        ));
    }
    let names: Vec<&str> = FORMATS.iter().map(|f| f.0).collect();
    let format = adw::ComboRow::new();
    format.set_title("Format");
    format.set_subtitle("WAV file");
    format.set_model(Some(&gtk::StringList::new(&names)));
    let repeats = adw::SpinRow::with_range(1.0, 16.0, 1.0);
    repeats.set_title("Repeats");
    repeats.set_subtitle("How many times the pattern plays");
    group.add(&format);
    group.add(&repeats);
    body.append(&group);

    let progress = gtk::ProgressBar::new();
    progress.set_visible(false);
    progress.update_property(&[gtk::accessible::Property::Label("Export progress")]);
    body.append(&progress);

    let go = gtk::Button::with_label("Export");
    go.add_css_class("suggested-action");
    go.add_css_class("pill");
    go.set_halign(gtk::Align::Center);
    body.append(&go);
    view.set_content(Some(&body));
    dialog.set_child(Some(&view));
    dialog.set_default_widget(Some(&go));

    let cancel = Arc::new(AtomicBool::new(false));
    let running = Rc::new(std::cell::Cell::new(false));
    {
        let (cancel, running) = (cancel.clone(), running.clone());
        dialog.connect_closed(move |_| {
            if running.get() {
                cancel.store(true, Ordering::Relaxed);
            }
        });
    }

    let (app2, win, dlg) = (app.clone(), window.clone(), dialog.clone());
    go.connect_clicked(move |btn| {
        if running.get() {
            // The button is "Cancel" while running.
            cancel.store(true, Ordering::Relaxed);
            return;
        }
        let fmt = FORMATS[(format.selected() as usize).min(FORMATS.len() - 1)].1;
        let loops = repeats.value() as u32;
        let name = files::display_name(&app2.ui.borrow().path);
        let dialog = gtk::FileDialog::builder()
            .title("Export Audio")
            .initial_name(export_name(&name))
            .build();
        let (app3, dlg2, btn2, progress2, cancel2, running2, format2, repeats2) = (
            app2.clone(),
            dlg.clone(),
            btn.clone(),
            progress.clone(),
            cancel.clone(),
            running.clone(),
            format.clone(),
            repeats.clone(),
        );
        dialog.save(Some(&win), gio::Cancellable::NONE, move |res| {
            let Ok(file) = res else { return };
            let Some(path) = file.path() else { return };
            start(
                &app3, &dlg2, &btn2, &progress2, &cancel2, &running2, &format2, &repeats2, pattern,
                loops, fmt, path,
            );
        });
    });
    dialog.present(Some(window));
}

#[allow(clippy::too_many_arguments)]
fn start(
    app: &Rc<App>,
    dialog: &adw::Dialog,
    button: &gtk::Button,
    progress_bar: &gtk::ProgressBar,
    cancel: &Arc<AtomicBool>,
    running: &Rc<std::cell::Cell<bool>>,
    format: &adw::ComboRow,
    repeats: &adw::SpinRow,
    pattern: protocol::ids::PatternId,
    loops: u32,
    fmt: WavFormat,
    path: PathBuf,
) {
    cancel.store(false, Ordering::Relaxed);
    running.set(true);
    button.set_label("Cancel");
    button.remove_css_class("suggested-action");
    format.set_sensitive(false);
    repeats.set_sensitive(false);
    progress_bar.set_visible(true);
    progress_bar.set_fraction(0.0);

    let (project, slots, rate, store) = {
        let s = app.session.borrow();
        (
            s.document().project.clone(),
            s.slots.clone(),
            s.link.sample_rate().round() as u32,
            s.store.clone(),
        )
    };
    let progress = Arc::new(AtomicU32::new(0));
    let (p2, c2, out) = (progress.clone(), cancel.clone(), path.clone());
    let done = Rc::new(std::cell::Cell::new(false));

    // Progress ticks until the task reports.
    {
        let (bar, done, p) = (progress_bar.clone(), done.clone(), progress.clone());
        glib::timeout_add_local(Duration::from_millis(100), move || {
            if done.get() {
                return glib::ControlFlow::Break;
            }
            bar.set_fraction(p.load(Ordering::Relaxed).min(100) as f64 / 100.0);
            glib::ControlFlow::Continue
        });
    }

    let (app2, dialog2, running2) = (app.clone(), dialog.clone(), running.clone());
    app.tasks.spawn(
        "export",
        move || -> Result<Vec<String>, String> {
            let frames = engine_adapter::render(
                RenderJob {
                    project,
                    pattern,
                    loops,
                    sample_rate: rate,
                    song_tail: None,
                    store: Some(store),
                },
                &slots,
                &[],
                &p2,
                &c2,
            )
            .map_err(|e| e.to_string())?;
            let warnings = frames.warnings;
            let frames = frames.audio;
            if c2.load(Ordering::Relaxed) {
                return Err("cancelled".into());
            }
            engine_adapter::write_wav(&out, &frames, rate, fmt).map_err(|e| e.to_string())?;
            Ok(warnings)
        },
        move |r| {
            done.set(true);
            running2.set(false);
            dialog2.close();
            match r {
                Ok(warnings) => {
                    let title = if warnings.is_empty() {
                        "Export finished".to_string()
                    } else {
                        format!("Export finished. {}", warnings.join(" "))
                    };
                    let target = path.clone();
                    app2.toast_action(&title, "Show in Files", move || {
                        let file = gio::File::for_path(&target);
                        gtk::FileLauncher::new(Some(&file)).open_containing_folder(
                            None::<&gtk::Window>,
                            gio::Cancellable::NONE,
                            |_| {},
                        );
                    });
                }
                Err(e) if e == "cancelled" => app2.toast("Export cancelled"),
                Err(e) => app2.toast(&format!("Could not export: {e}")),
            }
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_names() {
        assert_eq!(export_name("My Beat"), "My Beat.wav");
        assert_eq!(export_name("  "), "Export.wav");
        assert_eq!(export_name("a/b"), "a-b.wav");
    }

    #[test]
    fn formats_are_listed_once() {
        assert_eq!(FORMATS.len(), 3);
        assert_ne!(FORMATS[0].1, FORMATS[1].1);
    }
}
