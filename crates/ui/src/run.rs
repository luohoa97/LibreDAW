// SPDX-License-Identifier: GPL-3.0-or-later
//! Program start: engine, plugin scan, session, window.

use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use crate::app::App;
use crate::engine_adapter::{self, EngineConfig, EngineLink, Host};
use crate::plugin_adapter;
use crate::registry::Registry;
use crate::session::Session;
use crate::slots::SlotAllocator;
use crate::window;
use doc::document::Document;

pub const APP_ID: &str = "io.github.luohoa97.Oto";

/// Prints start-up stages when `LIBREDAW_DEBUG` is set.
fn trace(what: &str) {
    if std::env::var_os("LIBREDAW_DEBUG").is_some() {
        eprintln!("libredaw: {what}");
    }
}

/// The audio hosts to try, in order, with the user's settings.
pub fn audio_configs(settings: &crate::settings::Settings) -> Vec<EngineConfig> {
    [Host::PipeWire, Host::Alsa, Host::Jack]
        .into_iter()
        .map(|host| EngineConfig {
            host,
            device: settings.output_device.clone(),
            buffer_frames: settings.buffer_frames,
            sample_rate: None,
        })
        .collect()
}

/// Tries the audio hosts in order. Returns the link and, if none opened, why.
fn start_audio(settings: &crate::settings::Settings) -> (EngineLink, Option<String>) {
    let empty = Document::new();
    let mut slots = SlotAllocator::new();
    let _ = slots.sync(&empty.project);
    let mut last_err = String::from("no audio host available");
    for cfg in audio_configs(settings) {
        let first = engine_adapter::compile(&crate::compiler::CompileJob {
            revision: 0,
            project: empty.project.clone(),
            slots: slots.clone(),
            sample_rate: 48000.0,
            store: None,
        });
        match EngineLink::start(&cfg, first) {
            Ok(l) => return (l, None),
            Err(e) => last_err = e.to_string(),
        }
    }
    (EngineLink::stub(48000.0), Some(last_err))
}

/// The `.ldaw` projects among the files the app was asked to open (a
/// project is a folder; the desktop file passes `%F`).
pub fn projects_in(files: &[gtk::gio::File]) -> Vec<std::path::PathBuf> {
    files
        .iter()
        .filter_map(|f| f.path())
        .filter(|p| doc::persist::is_bundle(p))
        .collect()
}

pub fn run() -> glib::ExitCode {
    let gapp = adw::Application::builder()
        .application_id(APP_ID)
        .flags(gtk::gio::ApplicationFlags::HANDLES_OPEN)
        .build();
    // `libredaw Song.ldaw` or opening a project from Files: that project,
    // instead of the last one.
    gapp.connect_open(|gapp, files, _| {
        let paths = projects_in(files);
        let app = match start(gapp) {
            Started::New(app) => app,
            Started::Running(app) => app,
        };
        match paths.into_iter().next() {
            Some(p) => crate::files::open_path(&app, p),
            None => app.toast("That is not an Oto project"),
        }
    });
    gapp.add_main_option(
        "agent-request",
        glib::Char::from(0),
        glib::OptionFlags::NONE,
        glib::OptionArg::None,
        "Ask to allow agent control at start-up",
        None,
    );
    gapp.connect_activate(|gapp| match start(gapp) {
        Started::New(app) => crate::files::restore_last_session(&app),
        Started::Running(_) => {
            if let Some(w) = gapp.active_window() {
                w.present();
            }
        }
    });
    gapp.run()
}

/// Whether `start` made the window or found it.
enum Started {
    New(Rc<App>),
    Running(Rc<App>),
}

thread_local! {
    static RUNNING: std::cell::RefCell<Option<Rc<App>>> = const { std::cell::RefCell::new(None) };
}

/// Starts audio, the session, the control socket and the window once;
/// later calls return the running app.
fn start(gapp: &adw::Application) -> Started {
    if let Some(app) = RUNNING.with(|r| r.borrow().clone()) {
        return Started::Running(app);
    }
    trace("starting audio");
    let settings = crate::settings::Settings::read(&crate::files::real_dirs());
    let (link, audio_err) = start_audio(&settings);
    trace(&format!(
        "audio: {}",
        audio_err.as_deref().unwrap_or("running")
    ));
    let rate = link.sample_rate();
    let catalog = plugin_adapter::scan();
    trace(&format!("{} plugins found", catalog.len()));
    let session = Session::new(Document::new(), false, link, Registry::new(catalog, rate));
    let app: Rc<App> = App::new(session);
    crate::files::point_samples_at(&app, None);
    // Audio that is off shows in a banner with Retry (window.rs).
    app.ui.borrow_mut().audio_error = audio_err;
    // The control socket for scripts and agents. Agents stay off until
    // the user allows them (Preferences or the banner).
    let agent_request = std::env::args().any(|a| a == "--agent-request");
    let (bridge, bridge_err) = crate::control_bridge::start(agent_request);
    if let Some(e) = &bridge_err {
        trace(e);
    }
    *app.bridge.borrow_mut() = bridge;
    trace("building window");
    let win = window::build(gapp, app.clone());
    win.present();
    trace("window presented");
    // SIGTERM, SIGHUP, SIGINT: save, then exit (Amendment 10). The
    // window's close handler does the saving.
    use crate::signals::{SIGHUP, SIGINT, SIGTERM, signal_source};
    for sig in [SIGTERM, SIGHUP, SIGINT] {
        let w = win.clone();
        // The sources live as long as the main context.
        std::mem::forget(signal_source(sig, move || {
            trace("signal: closing");
            w.close()
        }));
    }
    RUNNING.with(|r| *r.borrow_mut() = Some(app.clone()));
    Started::New(app)
}
