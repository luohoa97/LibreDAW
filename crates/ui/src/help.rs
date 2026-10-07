// SPDX-License-Identifier: GPL-3.0-or-later
//! The About dialog and the keyboard shortcuts window.

use adw::prelude::*;

use crate::shortcuts;

pub fn show_about(window: &adw::ApplicationWindow) {
    let d = adw::AboutDialog::builder()
        .application_name("LibreDAW")
        .application_icon("audio-x-generic")
        .developer_name("The LibreDAW contributors")
        .version(env!("CARGO_PKG_VERSION"))
        .comments("A free digital audio workstation for Linux, made for beginners.")
        .copyright("Copyright The LibreDAW contributors")
        .license_type(gtk::License::Gpl30)
        .build();
    d.present(Some(window));
}

/// The keyboard shortcuts window, generated from `shortcuts::SHORTCUTS`.
pub fn show_shortcuts(window: &adw::ApplicationWindow) {
    let b = gtk::Builder::from_string(&shortcuts::shortcuts_window_xml());
    if let Some(w) = b.object::<gtk::ShortcutsWindow>("help") {
        w.set_transient_for(Some(window));
        w.present();
    }
}
