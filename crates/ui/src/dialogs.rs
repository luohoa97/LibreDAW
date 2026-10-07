// SPDX-License-Identifier: GPL-3.0-or-later
//! Dialogs: the plugin chooser (SPEC 13.1 item 8) and a name prompt.

use std::rc::Rc;

use adw::prelude::*;

use crate::app::App;
use crate::plugin_adapter::PluginDesc;

/// What the chooser lists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PluginKind {
    Instrument,
    Effect,
    Any,
}

fn matches(d: &PluginDesc, k: PluginKind) -> bool {
    match k {
        PluginKind::Instrument => d.instrument,
        PluginKind::Effect => d.effect,
        PluginKind::Any => true,
    }
}

pub fn kind_label(d: &PluginDesc) -> &'static str {
    match (d.instrument, d.effect) {
        (true, true) => "instrument and effect",
        (true, false) => "instrument",
        (false, true) => "effect",
        _ => "plugin",
    }
}

/// Lists the plugins found by the scan; `on_pick` runs with the choice.
pub fn choose_plugin(
    parent: &impl IsA<gtk::Widget>,
    app: &Rc<App>,
    kind: PluginKind,
    title: &str,
    on_pick: impl Fn(PluginDesc) + 'static,
) {
    let on_pick = Rc::new(on_pick);
    let list = gtk::ListBox::new();
    list.add_css_class("boxed-list");
    list.set_selection_mode(gtk::SelectionMode::None);
    let scroller = gtk::ScrolledWindow::builder()
        .child(&list)
        .vexpand(true)
        .min_content_height(260)
        .propagate_natural_height(true)
        .build();

    let dialog = adw::Dialog::builder()
        .title(title)
        .content_width(460)
        .content_height(420)
        .build();

    let fill = {
        let list = list.clone();
        let app = app.clone();
        let dialog = dialog.clone();
        let on_pick = on_pick.clone();
        Rc::new(move |plugins: Vec<PluginDesc>| {
            while let Some(c) = list.first_child() {
                list.remove(&c);
            }
            let shown: Vec<_> = plugins.into_iter().filter(|d| matches(d, kind)).collect();
            if shown.is_empty() {
                let row = adw::ActionRow::builder()
                    .title("No plugins found")
                    .subtitle("Looked in CLAP_PATH, ~/.clap, /usr/lib/clap, /usr/lib64/clap")
                    .build();
                list.append(&row);
            }
            // Plugins with sounds in our list come first; the rest sit behind
            // "Other Plugins", out of the way of beginners.
            let (main, other): (Vec<_>, Vec<_>) = shown.into_iter().partition(|d| {
                plugin_host::sounds::plugins()
                    .iter()
                    .any(|p| p.clap_id == d.id)
            });
            let other_rows = adw::ExpanderRow::builder()
                .title("Other Plugins")
                .subtitle("For advanced users")
                .build();
            let have_main = !main.is_empty();
            let any_other = !other.is_empty();
            for (is_main, d) in main
                .into_iter()
                .map(|d| (true, d))
                .chain(other.into_iter().map(|d| (false, d)))
            {
                let row = adw::ActionRow::builder()
                    .title(glib_escape(&d.name))
                    .subtitle(glib_escape(&format!(
                        "{} · {} · {}",
                        d.vendor,
                        d.version,
                        kind_label(&d)
                    )))
                    .activatable(true)
                    .build();
                let (dialog, on_pick, _app) = (dialog.clone(), on_pick.clone(), app.clone());
                row.connect_activated(move |_| {
                    dialog.close();
                    on_pick(d.clone());
                });
                if is_main {
                    list.append(&row);
                } else {
                    other_rows.add_row(&row);
                }
            }
            if any_other {
                other_rows.set_expanded(!have_main);
                list.append(&other_rows);
            }
        })
    };
    fill(app.session.borrow().registry.catalog().to_vec());

    let rescan = gtk::Button::with_label("Scan again");
    {
        let (app, fill) = (app.clone(), fill.clone());
        rescan.connect_clicked(move |b| {
            b.set_sensitive(false);
            let found = crate::plugin_adapter::scan();
            app.session.borrow_mut().registry.set_catalog(found.clone());
            fill(found);
            b.set_sensitive(true);
        });
    }
    let bx = gtk::Box::new(gtk::Orientation::Vertical, 12);
    bx.set_margin_top(12);
    bx.set_margin_bottom(12);
    bx.set_margin_start(12);
    bx.set_margin_end(12);
    bx.append(&scroller);
    bx.append(&rescan);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&bx));
    dialog.set_child(Some(&toolbar));
    dialog.present(Some(parent));
}

/// Escapes text for Adw rows, which read titles as Pango markup.
fn glib_escape(s: &str) -> String {
    gtk::glib::markup_escape_text(s).to_string()
}

/// Asks for a name; `on_ok` runs with the entered text if it is not empty.
pub fn ask_name(
    parent: &impl IsA<gtk::Widget>,
    title: &str,
    initial: &str,
    on_ok: impl Fn(String) + 'static,
) {
    let entry = gtk::Entry::builder()
        .text(initial)
        .activates_default(true)
        .build();
    let dialog = adw::AlertDialog::builder()
        .heading(title)
        .close_response("cancel")
        .default_response("ok")
        .build();
    dialog.add_responses(&[("cancel", "Cancel"), ("ok", "OK")]);
    dialog.set_response_appearance("ok", adw::ResponseAppearance::Suggested);
    dialog.set_extra_child(Some(&entry));
    let e = entry.clone();
    dialog.connect_response(None, move |_, r| {
        if r == "ok" {
            let t = e.text().to_string();
            if !t.trim().is_empty() {
                on_ok(t.trim().to_string());
            }
        }
    });
    dialog.present(Some(parent));
}

/// A yes or no question with a destructive or suggested confirm button.
pub fn confirm(
    parent: &impl IsA<gtk::Widget>,
    heading: &str,
    body: &str,
    confirm_label: &str,
    destructive: bool,
    on_yes: impl Fn() + 'static,
) {
    let dialog = adw::AlertDialog::builder()
        .heading(heading)
        .body(body)
        .close_response("cancel")
        .build();
    dialog.add_responses(&[("cancel", "Cancel"), ("yes", confirm_label)]);
    dialog.set_response_appearance(
        "yes",
        if destructive {
            adw::ResponseAppearance::Destructive
        } else {
            adw::ResponseAppearance::Suggested
        },
    );
    dialog.connect_response(None, move |_, r| {
        if r == "yes" {
            on_yes();
        }
    });
    dialog.present(Some(parent));
}
