// SPDX-License-Identifier: GPL-3.0-or-later
//! Sounds for the Sounds pane (SPEC 20.6, Amendment 23): factory sounds of a
//! free CLAP plugin, by role. Adding one makes an instrument that already
//! plays it. When the plugin is missing the pane greys them out and
//! "Install Sounds…" opens the software store.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use plugin_host::sounds::{self, Sound};

use crate::app::App;
use crate::channels::{self, NewChannel};

/// The plain name and one-sentence description of a role.
pub fn role_text(role: &str) -> (&'static str, &'static str) {
    match role {
        "Bass" => ("Bass", "Deep, smooth bass for low notes"),
        "808" => ("808", "Booming bass that rings out, for hip hop and trap"),
        "Lead" => ("Lead", "A bright sound that carries the tune"),
        "Pad" => ("Pad", "Soft, slow chords that fill the background"),
        "Keys" => ("Keys", "Piano, organ and electric piano sounds"),
        "Pluck" => ("Pluck", "Short, plucked notes that fade quickly"),
        "Bell" => ("Bell", "Ringing, glassy sounds for sparkle"),
        "Strings" => ("Strings", "Warm violins and cellos for long notes"),
        "Brass" => ("Brass", "Horn sounds with a bold, punchy start"),
        "FX" => ("Sound FX", "Risers, sweeps and strange noises for effect"),
        "Arp" => ("Arp", "Notes that run up and down by themselves"),
        _ => ("More", "More sounds"),
    }
}

/// The sound's name for a person: its file name without the folder and
/// extension ("Sub 1").
pub fn sound_label(s: &Sound) -> String {
    s.label()
}

/// True if the plugin behind `s` was found by the scan.
pub fn available(app: &App, s: &Sound) -> bool {
    sounds::plugin_of(s).is_some_and(|p| {
        app.session
            .borrow()
            .registry
            .find_desc(&p.clap_id)
            .is_some()
    })
}

/// The way to add `s` as a new instrument.
pub fn new_channel(s: &Sound) -> Option<NewChannel> {
    Some(NewChannel::Sound {
        plugin_id: sounds::plugin_of(s)?.clap_id.clone(),
        name: sound_label(s),
        preset: s.preset.clone(),
        note: s.note,
        gain_db: s.gain_db,
    })
}

/// Does the first plugin of the list exist on this machine?
pub fn installed(app: &App) -> bool {
    sounds::plugins().first().is_some_and(|p| {
        app.session
            .borrow()
            .registry
            .find_desc(&p.clap_id)
            .is_some()
    })
}

thread_local! {
    /// Redraws the Sounds pane after a rescan.
    static REFRESH: RefCell<Option<Rc<dyn Fn()>>> = const { RefCell::new(None) };
}

/// Lets the Sounds pane be redrawn when the plugin scan finds something new.
pub fn set_refresh(f: Rc<dyn Fn()>) {
    REFRESH.with(|r| *r.borrow_mut() = Some(f));
}

/// Opens the software store on the sounds' page.
pub fn open_store(parent: &gtk::Widget) {
    let Some(ext) = sounds::plugins().first().map(|p| p.extension.clone()) else {
        return;
    };
    let window = parent.root().and_downcast::<gtk::Window>();
    gtk::UriLauncher::new(&format!("appstream://{ext}")).launch(
        window.as_ref(),
        gtk::gio::Cancellable::NONE,
        |_| {},
    );
}

/// Looks for plugins again whenever the window comes back to the front, so
/// an install made in the store shows up without a restart.
pub fn rescan_on_focus(window: &adw::ApplicationWindow, app: &Rc<App>) {
    let app = app.clone();
    window.connect_is_active_notify(move |w| {
        if !w.is_active() || installed(&app) {
            return;
        }
        let a2 = app.clone();
        app.tasks
            .spawn("plugin-scan", crate::plugin_adapter::scan, move |found| {
                let was = installed(&a2);
                a2.session.borrow_mut().registry.set_catalog(found);
                if installed(&a2) != was {
                    REFRESH.with(|r| {
                        if let Some(f) = r.borrow().clone() {
                            f();
                        }
                    });
                }
            });
    });
}

/// The plugin a sound plays through, as the user knows it ("Surge XT").
pub fn plugin_name(s: &Sound) -> String {
    sounds::plugin_of(s)
        .map(|p| p.name.clone())
        .unwrap_or_default()
}

/// Adds `s` as a new instrument, with Undo in the toast.
pub fn add_sound(app: &Rc<App>, s: &Sound) {
    if let Some(what) = new_channel(s)
        && channels::add(app, what).is_some()
    {
        let a2 = app.clone();
        app.toast_action(&format!("Added {}", sound_label(s)), "Undo", move || {
            a2.undo()
        });
    }
}
