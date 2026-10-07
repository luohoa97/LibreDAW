// SPDX-License-Identifier: GPL-3.0-or-later
//! The instrument picker (SPEC 20.6, Amendment 23): sounds by role, each a
//! factory sound of a free CLAP plugin. Choosing one adds an instrument that
//! already plays it. When the plugin is not installed the sounds are greyed
//! out and "Install Sounds…" opens the software store.

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
fn installed(app: &App) -> bool {
    sounds::plugins().first().is_some_and(|p| {
        app.session
            .borrow()
            .registry
            .find_desc(&p.clap_id)
            .is_some()
    })
}

thread_local! {
    /// Redraws the open picker after a rescan, if one is open.
    static REFRESH: RefCell<Option<Rc<dyn Fn()>>> = const { RefCell::new(None) };
}

/// Opens the software store on the sounds' page.
fn open_store(parent: &gtk::Widget) {
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

/// Shows the picker over `parent`.
pub fn show(parent: &impl IsA<gtk::Widget>, app: &Rc<App>) {
    let dialog = adw::Dialog::builder()
        .title("Add Instrument")
        .content_width(420)
        .content_height(560)
        .build();
    let page = gtk::Box::new(gtk::Orientation::Vertical, 18);
    page.set_margin_top(12);
    page.set_margin_bottom(18);
    page.set_margin_start(12);
    page.set_margin_end(12);
    let scroller = gtk::ScrolledWindow::builder()
        .child(&page)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .build();
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&scroller));
    dialog.set_child(Some(&toolbar));

    let fill: Rc<dyn Fn()> = {
        let (app, page, dialog) = (app.clone(), page.clone(), dialog.clone());
        Rc::new(move || {
            while let Some(c) = page.first_child() {
                page.remove(&c);
            }
            let have = installed(&app);
            if !have {
                page.append(&install_group(&dialog));
            }
            for role in sounds::roles() {
                page.append(&role_group(&app, &dialog, role, have));
            }
        })
    };
    fill();
    REFRESH.with(|r| *r.borrow_mut() = Some(fill));
    dialog.connect_closed(|_| REFRESH.with(|r| *r.borrow_mut() = None));
    dialog.present(Some(parent));
}

fn install_group(dialog: &adw::Dialog) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::new();
    let row = adw::ActionRow::builder()
        .title("Install Sounds…")
        .subtitle("Get free sounds for bass, keys, pads and more")
        .activatable(true)
        .build();
    row.add_prefix(&gtk::Image::from_icon_name("folder-download-symbolic"));
    row.set_tooltip_text(Some("Opens the software store to get the free sounds"));
    let d = dialog.clone();
    row.connect_activated(move |_| open_store(d.upcast_ref()));
    group.add(&row);
    group
}

fn role_group(
    app: &Rc<App>,
    dialog: &adw::Dialog,
    role: &str,
    have: bool,
) -> adw::PreferencesGroup {
    let (title, blurb) = role_text(role);
    let group = adw::PreferencesGroup::builder().title(title).build();
    group.set_sensitive(have);
    for s in sounds::sounds().iter().filter(|s| s.role == role) {
        let label = sound_label(s);
        let row = adw::ActionRow::builder()
            .title(gtk::glib::markup_escape_text(&label))
            .activatable(have)
            .build();
        row.set_tooltip_text(Some(blurb));
        let (app, dialog, s) = (app.clone(), dialog.clone(), s.clone());
        row.connect_activated(move |_| {
            dialog.close();
            if let Some(what) = new_channel(&s)
                && channels::add(&app, what).is_some()
            {
                let a2 = app.clone();
                app.toast_action(&format!("Added {}", sound_label(&s)), "Undo", move || {
                    a2.undo()
                });
            }
        });
        group.add(&row);
    }
    group
}
