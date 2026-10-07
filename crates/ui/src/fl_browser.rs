// SPDX-License-Identifier: GPL-3.0-or-later
//! The "FL Studio" section of the Sounds pane: the offer to use the
//! owner's FL Studio sounds, the scan, and the kits, instruments and sounds
//! it finds. The files are read in place and stay on this computer.
//!
//! The pane builds a few rows at a time from metadata; audio is decoded
//! when a sound is added or previewed (SPEC 23).

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use library::index::SoundEntry;
use library::{FlInstall, Kit};

use crate::app::App;
use crate::channels::{self, NewChannel};
use crate::fl_library::{self as fl, Loaded, Status};
use crate::samples_ui::{self, ImportItem};

/// Rows shown with nothing typed, and with a search or filter.
const BROWSE_SOUNDS: usize = 30;
const FOUND_SOUNDS: usize = 150;
const FOUND_INSTRUMENTS: usize = 60;
const BROWSE_INSTRUMENTS: usize = 12;

thread_local! {
    static LISTENER: RefCell<Option<Rc<dyn Fn()>>> = const { RefCell::new(None) };
    static STARTED: Cell<bool> = const { Cell::new(false) };
    static DETECTED: RefCell<Option<Vec<FlInstall>>> = const { RefCell::new(None) };
}

/// Called when the scan changes what the pane should show.
pub fn set_listener(f: Rc<dyn Fn()>) {
    LISTENER.with(|l| *l.borrow_mut() = Some(f));
}

fn changed() {
    let f = LISTENER.with(|l| l.borrow().clone());
    if let Some(f) = f {
        f();
    }
}

fn esc(s: &str) -> String {
    gtk::glib::markup_escape_text(s).to_string()
}

/// The installs on this computer, looked up once (a few folder listings).
fn detected() -> Vec<FlInstall> {
    DETECTED.with(|d| {
        d.borrow_mut()
            .get_or_insert_with(library::detect_installs)
            .clone()
    })
}

fn cache_dir(app: &App) -> std::path::PathBuf {
    library::default_cache_dir().unwrap_or_else(|| app.dirs.data.join("cache"))
}

/// Reads the folder: headers first so the list shows quickly, then the
/// keys of numbered multisamples in the background.
fn start_scan(app: &Rc<App>, install: FlInstall) {
    fl::set_status(Status::Scanning);
    let cache = cache_dir(app);
    let (a, i2, c2) = (app.clone(), install.clone(), cache.clone());
    app.tasks.spawn(
        "fl-scan-fast",
        {
            let (i, c) = (install, cache);
            move || fl::scan(&i, &c, false)
        },
        move |fast| {
            match fast {
                Some(l) => fl::set_status(Status::Ready(Arc::new(l))),
                None => {
                    fl::set_status(Status::Failed);
                    changed();
                    return;
                }
            }
            changed();
            a.tasks.spawn(
                "fl-scan-full",
                move || fl::scan(&i2, &c2, true),
                move |full| {
                    if let Some(l) = full {
                        fl::set_status(Status::Ready(Arc::new(l)));
                        changed();
                    }
                },
            );
        },
    );
}

/// Picks up the choice remembered from an earlier run, once.
pub fn resume(app: &Rc<App>) {
    if STARTED.with(|s| s.replace(true)) {
        return;
    }
    if let Some(install) = fl::load(&app.dirs.config) {
        start_scan(app, install);
    }
}

fn adopt(app: &Rc<App>, install: FlInstall) {
    if let Err(e) = fl::save(&app.dirs.config, &install) {
        app.toast(&format!("Could not remember your choice: {e}"));
    }
    start_scan(app, install);
    changed();
}

/// "Check FL Studio's license" notice, then the scan.
fn confirm(app: &Rc<App>, parent: &gtk::Widget, install: FlInstall) {
    let alert = adw::AlertDialog::new(
        Some("Use Your FL Studio Sounds?"),
        Some(
            "These sounds stay on this computer. Oto reads them from your FL Studio folder \
             and never shares them. Check FL Studio's license for how you may use them.",
        ),
    );
    alert.add_responses(&[("cancel", "_Cancel"), ("add", "_Add")]);
    alert.set_response_appearance("add", adw::ResponseAppearance::Suggested);
    alert.set_default_response(Some("add"));
    alert.set_close_response("cancel");
    let app = app.clone();
    alert.connect_response(None, move |_, r| {
        if r == "add" {
            adopt(&app, install.clone());
        }
    });
    alert.present(Some(parent));
}

/// Choose Folder... through the file chooser, for an install outside the
/// sandbox or in an unusual place.
fn choose_folder(app: &Rc<App>, parent: &gtk::Widget) {
    let dialog = gtk::FileDialog::builder()
        .title("Choose Your FL Studio Folder")
        .build();
    let win = parent.root().and_then(|r| r.downcast::<gtk::Window>().ok());
    let (app, parent) = (app.clone(), parent.clone());
    dialog.select_folder(win.as_ref(), gtk::gio::Cancellable::NONE, move |res| {
        let Ok(f) = res else { return };
        let Some(dir) = f.path() else { return };
        match library::install_from_folder(&dir) {
            Some(install) => confirm(&app, &parent, install),
            None => app.toast("That folder could not be read"),
        }
    });
}

// ---------------------------------------------------------------------------
// Adding and previewing

fn first_error(app: &App, results: &[Result<protocol::model::SampleRef, String>]) {
    if let Some(Err(e)) = results.iter().find(|r| r.is_err()) {
        app.toast(e);
    }
}

fn item(e: &SoundEntry) -> ImportItem {
    ImportItem {
        path: e.path.clone(),
        local_only: true,
        expect_sha256: None,
    }
}

fn add_sound(app: &Rc<App>, e: &SoundEntry) {
    let (a, e) = (app.clone(), e.clone());
    samples_ui::import(app, vec![item(&e)], move |results| {
        first_error(&a, &results);
        if let Some(Ok(s)) = results.into_iter().next() {
            let name = e.name.clone();
            if channels::add(&a, NewChannel::Sampler(fl::sound_setup(&e, s))).is_some() {
                let a2 = a.clone();
                a.toast_action(&format!("Added {name}"), "Undo", move || a2.undo());
            }
        }
    });
}

fn add_kit(app: &Rc<App>, kit: &Kit, loaded: &Loaded) {
    let sounds: Vec<(&'static str, SoundEntry)> = fl::kit_sounds(kit, &loaded.index)
        .into_iter()
        .map(|(slot, e)| (slot, e.clone()))
        .collect();
    let (a, name) = (app.clone(), kit.name.clone());
    let items = sounds.iter().map(|(_, e)| item(e)).collect();
    samples_ui::import(app, items, move |results| {
        first_error(&a, &results);
        let setups: Vec<_> = sounds
            .iter()
            .zip(results)
            .filter_map(|((slot, e), r)| Some(fl::kit_piece_setup(slot, e, r.ok()?)))
            .collect();
        let n = setups.len();
        if !channels::add_kit(&a, &format!("{name} Kit"), setups).is_empty() {
            let a2 = a.clone();
            a.toast_action(
                &format!("Added {n} sounds from {name}"),
                "Undo",
                move || a2.undo(),
            );
        }
    });
}

/// The sampler plays one recorded note of the instrument, pitched across the
/// keyboard: it has no key zones yet, and the toast says so.
fn add_instrument(app: &Rc<App>, inst: &library::Instrument, loaded: &Loaded) {
    let Some((root, e)) = fl::instrument_root(inst, &loaded.index) else {
        return;
    };
    let (a, name, e) = (app.clone(), inst.name.clone(), e.clone());
    samples_ui::import(app, vec![item(&e)], move |results| {
        first_error(&a, &results);
        if let Some(Ok(s)) = results.into_iter().next() {
            let setup = fl::instrument_setup(root, &name, s);
            if channels::add(&a, NewChannel::Sampler(setup)).is_some() {
                let a2 = a.clone();
                a.toast_action(
                    &format!("Added {name}, played from one recorded note"),
                    "Undo",
                    move || a2.undo(),
                );
            }
        }
    });
}

/// Plays the sound on the preview voice: decoded now, on a worker thread,
/// and only for this click.
fn preview(app: &Rc<App>, e: &SoundEntry) {
    if !app.settings.borrow().preview_notes {
        return;
    }
    let path = e.path.clone();
    let rate = app.session.borrow().store.sample_rate();
    let a = app.clone();
    app.tasks.spawn(
        "fl-preview",
        move || {
            let data = engine::samples::load_sample_file(&path, rate).ok()?;
            Some((fl::preview_key(&path), data))
        },
        move |r| {
            let Some((key, data)) = r else {
                a.toast("That sound could not be played");
                return;
            };
            let mut s = a.session.borrow_mut();
            let store = s.store.clone();
            store.insert(&engine::samples::hash_hex(&key), data);
            let _ = s.link.audition(
                Some(&store),
                protocol::engine::AuditionSource::Sample { hash: key },
                60,
                100,
                true,
            );
        },
    );
}

// ---------------------------------------------------------------------------
// The rows

fn plus_button(label: &str) -> gtk::Button {
    let b = gtk::Button::from_icon_name("list-add-symbolic");
    b.add_css_class("flat");
    b.set_valign(gtk::Align::Center);
    b.set_tooltip_text(Some("Add to Project"));
    b.update_property(&[gtk::accessible::Property::Label(&format!(
        "Add {label} to the project"
    ))]);
    b
}

/// One row: "+" adds, a click on the row previews (when `play` is given).
fn row(
    title: &str,
    subtitle: &str,
    on_add: impl Fn() + 'static,
    on_play: Option<Box<dyn Fn()>>,
) -> adw::ActionRow {
    let r = adw::ActionRow::new();
    r.set_title(&esc(title));
    r.set_subtitle(&esc(subtitle));
    let b = plus_button(title);
    b.connect_clicked(move |_| on_add());
    r.add_suffix(&b);
    if let Some(p) = on_play {
        r.set_activatable(true);
        r.set_tooltip_text(Some("Click to hear it"));
        r.connect_activated(move |_| p());
    }
    r
}

pub struct FlSection {
    app: Rc<App>,
    offer: adw::ActionRow,
    expander: adw::ExpanderRow,
    rows: RefCell<Vec<gtk::Widget>>,
}

/// Builds the section for the pane (one per rebuild).
pub fn section(app: &Rc<App>) -> Rc<FlSection> {
    resume(app);
    let offer = adw::ActionRow::new();
    offer.set_visible(false);
    let expander = adw::ExpanderRow::new();
    expander.set_title("FL Studio");
    expander.add_prefix(&gtk::Image::from_icon_name("computer-symbolic"));
    expander.set_visible(false);
    let sec = Rc::new(FlSection {
        app: app.clone(),
        offer,
        expander,
        rows: RefCell::new(Vec::new()),
    });
    sec.setup_offer();
    sec
}

impl FlSection {
    /// The row at the top of the pane.
    pub fn offer_row(&self) -> &adw::ActionRow {
        &self.offer
    }

    /// The section that lists the sounds.
    pub fn list_row(&self) -> &adw::ExpanderRow {
        &self.expander
    }

    fn setup_offer(self: &Rc<Self>) {
        let offer = &self.offer;
        let status = fl::status();
        let button = |label: &str| {
            let b = gtk::Button::with_label(label);
            b.set_valign(gtk::Align::Center);
            b.add_css_class("suggested-action");
            b
        };
        match status {
            Status::Off => {
                let Some(install) = detected().into_iter().next() else {
                    return;
                };
                offer.set_title("Use Your FL Studio Sounds");
                offer.set_subtitle(&esc(&format!("Found {} on this computer", install.name)));
                offer.add_prefix(&gtk::Image::from_icon_name("computer-symbolic"));
                let b = button("Add");
                let (app, offer2) = (self.app.clone(), offer.clone());
                b.connect_clicked(move |_| confirm(&app, offer2.upcast_ref(), install.clone()));
                offer.add_suffix(&b);
                offer.set_visible(true);
            }
            Status::Failed => {
                offer.set_title("FL Studio Sounds");
                offer.set_subtitle("Oto could not read your FL Studio folder");
                offer.add_prefix(&gtk::Image::from_icon_name("dialog-warning-symbolic"));
                let b = button("Choose Folder…");
                let (app, offer2) = (self.app.clone(), offer.clone());
                b.connect_clicked(move |_| choose_folder(&app, offer2.upcast_ref()));
                offer.add_suffix(&b);
                offer.set_visible(true);
            }
            Status::Scanning | Status::Ready(_) => {}
        }
    }

    /// Fills the section for the current search and filter. Returns 1 when
    /// the section shows something.
    pub fn update(&self, search: &str, role: Option<&str>) -> usize {
        for w in self.rows.borrow_mut().drain(..) {
            self.expander.remove(&w);
        }
        let loaded = match fl::status() {
            Status::Scanning => {
                self.expander.set_subtitle("Reading your FL Studio sounds…");
                self.expander
                    .set_visible(search.trim().is_empty() && role.is_none());
                return 0;
            }
            Status::Ready(l) => l,
            Status::Off | Status::Failed => {
                self.expander.set_visible(false);
                return 0;
            }
        };
        let searching = !search.trim().is_empty() || role.is_some();
        let mut rows: Vec<gtk::Widget> = Vec::new();

        // Kits, the default one first.
        let mut kits: Vec<&Kit> = loaded
            .kits
            .iter()
            .filter(|k| fl::kit_matches(k, search, role))
            .collect();
        if let Some(d) = library::default_kit(&loaded.kits)
            && let Some(i) = kits.iter().position(|k| k.id == d.id)
        {
            let k = kits.remove(i);
            kits.insert(0, k);
        }
        for k in kits.iter().take(FOUND_INSTRUMENTS) {
            let (app, kit, l) = (self.app.clone(), (*k).clone(), loaded.clone());
            let kick = fl::kit_sounds(k, &loaded.index)
                .first()
                .map(|(_, e)| (*e).clone());
            let play = kick.map(|e| {
                let app = self.app.clone();
                Box::new(move || preview(&app, &e)) as Box<dyn Fn()>
            });
            rows.push(
                row(
                    &k.name,
                    &format!("Drum Kit · FL Studio ({})", fl::pack_title(&k.pack)),
                    move || add_kit(&app, &kit, &l),
                    play,
                )
                .upcast(),
            );
        }

        // Instruments played across the keyboard.
        let cap = if searching {
            FOUND_INSTRUMENTS
        } else {
            BROWSE_INSTRUMENTS
        };
        for inst in loaded
            .instruments
            .iter()
            .filter(|i| fl::instrument_matches(i, search, role))
            .take(cap)
        {
            let (app, i2, l) = (self.app.clone(), inst.clone(), loaded.clone());
            let play = fl::instrument_root(inst, &loaded.index).map(|(_, e)| {
                let (app, e) = (self.app.clone(), e.clone());
                Box::new(move || preview(&app, &e)) as Box<dyn Fn()>
            });
            rows.push(
                row(
                    &inst.name,
                    &format!("{} · FL Studio", fl::role_title(&inst.role)),
                    move || add_instrument(&app, &i2, &l),
                    play,
                )
                .upcast(),
            );
        }

        // Single sounds by role.
        let cap = if searching {
            FOUND_SOUNDS
        } else {
            BROWSE_SOUNDS
        };
        let mut total = 0;
        for e in loaded
            .index
            .entries
            .iter()
            .filter(|e| fl::sound_matches(e, search, role))
        {
            total += 1;
            if total > cap {
                continue;
            }
            let (app, e1, e2) = (self.app.clone(), e.clone(), e.clone());
            let app2 = self.app.clone();
            let r = row(
                &e.name,
                &format!("{} · FL Studio", fl::role_title(&e.role)),
                move || add_sound(&app, &e1),
                Some(Box::new(move || preview(&app2, &e2))),
            );
            crate::samples_ui::make_draggable(&r, e.path.clone());
            rows.push(r.upcast());
        }
        if total > cap {
            let more = adw::ActionRow::new();
            more.set_title(&format!("Showing {cap} of {total} sounds"));
            more.set_subtitle("Search or choose a kind of sound to see more");
            more.set_sensitive(false);
            rows.push(more.upcast());
        }

        let shown = !rows.is_empty();
        let count = loaded.index.entries.len();
        self.expander.set_subtitle(&if loaded.complete {
            format!("{count} sounds on this computer only")
        } else {
            format!("{count} sounds, finding instruments…")
        });
        for r in &rows {
            self.expander.add_row(r);
        }
        if searching && shown {
            self.expander.set_expanded(true);
        }
        *self.rows.borrow_mut() = rows;
        self.expander.set_visible(shown);
        shown as usize
    }
}
