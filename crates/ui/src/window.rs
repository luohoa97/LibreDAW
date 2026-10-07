// SPDX-License-Identifier: GPL-3.0-or-later
//! The application window (docs/ui-design.md 2): `AdwToolbarView` with a
//! header bar (sounds, undo, redo, view switcher, inspector, main menu), the
//! transport bar, and the three pages Pattern, Song, and Mixer between a
//! sound browser on the left and an inspector on the right. Breakpoints fold
//! the layout down to 360 px.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use adw::prelude::*;
use gtk::gdk;
use gtk::gio;
use gtk::glib;

use protocol::edit::Edit;

use crate::app::{App, UiCommand};
use crate::channels::{self, NewChannel};
use crate::dialogs::{self, PluginKind};
use crate::files;
use crate::help;
use crate::mixer::Mixer;
use crate::palette;
use crate::pattern_page::{self, PatternPage};
use crate::prefs;
use crate::shortcuts::{self, SHORTCUTS};
use crate::size_class::{self, SizeClass};
use crate::transport::Transport;
use crate::{browser, export, inspector};
use doc::bundle::{AutosaveDebounce, AutosaveWorker};
use doc::persist::ViewState;

const DEFAULT_WIDTH: i32 = 1360;
const DEFAULT_HEIGHT: i32 = 800;

/// Loads `style.css` for the whole display.
#[allow(deprecated)]
pub fn load_css() {
    let Some(display) = gdk::Display::default() else {
        return;
    };
    let p = gtk::CssProvider::new();
    p.load_from_data(include_str!("../data/style.css"));
    gtk::style_context_add_provider_for_display(
        &display,
        &p,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}

/// `LIBREDAW_SIZE=360x640` sets the first window size (screenshots).
fn size_from_env() -> (i32, i32) {
    std::env::var("LIBREDAW_SIZE")
        .ok()
        .and_then(|s| {
            let (w, h) = s.split_once('x')?;
            Some((w.parse().ok()?, h.parse().ok()?))
        })
        .unwrap_or((DEFAULT_WIDTH, DEFAULT_HEIGHT))
}

/// Everything the window builds and later needs to reach.
struct Ui {
    window: adw::ApplicationWindow,
    stack: adw::ViewStack,
    browser_split: adw::OverlaySplitView,
    inspector_split: adw::OverlaySplitView,
    sounds_toggle: gtk::ToggleButton,
    inspector_toggle: gtk::ToggleButton,
    banner: adw::Banner,
    agent_btn: gtk::Button,
    undo: gtk::Button,
    redo: gtk::Button,
    narrow_title: adw::WindowTitle,
    transport: Rc<Transport>,
    pattern: PatternPage,
}

fn flat_toggle(icon: &str, label: &str) -> gtk::ToggleButton {
    let b = gtk::ToggleButton::new();
    b.set_icon_name(icon);
    b.add_css_class("flat");
    b.update_property(&[gtk::accessible::Property::Label(label)]);
    b
}

fn flat_button(icon: &str, label: &str, action: &str) -> gtk::Button {
    let b = gtk::Button::from_icon_name(icon);
    b.add_css_class("flat");
    b.set_action_name(Some(action));
    b.update_property(&[gtk::accessible::Property::Label(label)]);
    b
}

fn main_menu() -> gio::Menu {
    let menu = gio::Menu::new();
    let project = gio::Menu::new();
    project.append(Some("_New Project"), Some("win.new"));
    project.append(Some("_Open Project…"), Some("win.open"));
    project.append(Some("_Save"), Some("win.save"));
    project.append(Some("Save _As…"), Some("win.save-as"));
    menu.append_section(None, &project);
    let output = gio::Menu::new();
    output.append(Some("_Export Audio…"), Some("win.export"));
    menu.append_section(None, &output);
    let plugins = gio::Menu::new();
    plugins.append(Some("Add _Plugin…"), Some("win.add-plugin"));
    menu.append_section(None, &plugins);
    let end = gio::Menu::new();
    end.append(Some("_Preferences"), Some("app.preferences"));
    end.append(Some("_Keyboard Shortcuts"), Some("win.show-help-overlay"));
    end.append(Some("_About LibreDAW"), Some("app.about"));
    menu.append_section(None, &end);
    menu
}

pub fn build(gapp: &adw::Application, app: Rc<App>) -> adw::ApplicationWindow {
    load_css();
    prefs::apply_color_scheme(app.settings.borrow().color_scheme);
    let (w, h) = size_from_env();
    let window = adw::ApplicationWindow::builder()
        .application(gapp)
        .default_width(w)
        .default_height(h)
        .width_request(360)
        .height_request(294)
        .title("LibreDAW")
        .build();

    // ---- header bar ----
    let sounds_toggle = flat_toggle("sidebar-show-symbolic", "Sounds");
    let undo = flat_button("edit-undo-symbolic", "Undo", "win.undo");
    let redo = flat_button("edit-redo-symbolic", "Redo", "win.redo");
    undo.set_tooltip_text(Some(&shortcuts::tooltip("Undo", "win.undo")));
    redo.set_tooltip_text(Some(&shortcuts::tooltip("Redo", "win.redo")));
    let inspector_toggle = flat_toggle("sidebar-show-right-symbolic", "Inspector");
    let menu_button = gtk::MenuButton::new();
    menu_button.set_icon_name("open-menu-symbolic");
    menu_button.set_menu_model(Some(&main_menu()));
    menu_button.set_primary(true);
    menu_button.add_css_class("flat");
    menu_button.set_tooltip_text(Some("Main Menu"));
    menu_button.update_property(&[gtk::accessible::Property::Label("Main menu")]);

    let stack = adw::ViewStack::new();
    let switcher = adw::ViewSwitcher::builder()
        .policy(adw::ViewSwitcherPolicy::Wide)
        .stack(&stack)
        .build();
    let narrow_title = adw::WindowTitle::new("Untitled", "");
    narrow_title.set_visible(false);
    let title_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    title_box.append(&switcher);
    title_box.append(&narrow_title);

    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&title_box));
    header.pack_start(&sounds_toggle);
    header.pack_start(&undo);
    header.pack_start(&redo);
    let agent_btn = gtk::Button::from_icon_name("network-workgroup-symbolic");
    agent_btn.add_css_class("flat");
    agent_btn.set_tooltip_text(Some("An agent is connected"));
    agent_btn.update_property(&[gtk::accessible::Property::Label("An agent is connected")]);
    agent_btn.set_visible(false);
    header.pack_end(&menu_button);
    header.pack_end(&inspector_toggle);
    header.pack_end(&agent_btn);

    // ---- pages ----
    let pattern = pattern_page::build(&window, &app);
    let mixer = Mixer::new(app.clone());
    let song = adw::StatusPage::new();
    song.set_icon_name(Some("view-continuous-symbolic"));
    song.set_title("Song Arrangement");
    song.set_description(Some("Arranging patterns into a song is coming next."));
    stack.add_titled_with_icon(
        &pattern.widget,
        Some("pattern"),
        "Pattern",
        "view-list-symbolic",
    );
    stack.add_titled_with_icon(&song, Some("song"), "Song", "view-continuous-symbolic");
    stack.add_titled_with_icon(
        &mixer.widget(),
        Some("mixer"),
        "Mixer",
        "audio-volume-high-symbolic",
    );

    let switcher_bar = adw::ViewSwitcherBar::new();
    switcher_bar.set_stack(Some(&stack));
    let center = adw::ToolbarView::new();
    center.set_content(Some(&stack));
    center.add_bottom_bar(&switcher_bar);

    // ---- side panes ----
    let inspector = inspector::build(&app);
    let inspector_split = adw::OverlaySplitView::builder()
        .sidebar_position(gtk::PackType::End)
        .min_sidebar_width(300.0)
        .max_sidebar_width(420.0)
        .sidebar_width_fraction(0.28)
        .show_sidebar(false)
        .build();
    inspector_split.set_sidebar(Some(&inspector.widget));
    inspector_split.set_content(Some(&center));
    let browser_split = adw::OverlaySplitView::builder()
        .sidebar_position(gtk::PackType::Start)
        .min_sidebar_width(260.0)
        .max_sidebar_width(360.0)
        .sidebar_width_fraction(0.2)
        .show_sidebar(false)
        .build();
    browser_split.set_sidebar(Some(&browser::build(&app)));
    browser_split.set_content(Some(&inspector_split));

    // ---- transport and the toolbar view ----
    let transport = Transport::new(&app);
    let view = adw::ToolbarView::new();
    let banner = adw::Banner::new("");
    banner.set_button_label(Some("Review"));
    banner.set_revealed(false);
    view.add_top_bar(&header);
    view.add_top_bar(&transport.bar);
    view.add_top_bar(&banner);
    view.set_content(Some(&browser_split));

    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&view));
    overlay.add_overlay(&palette::install());
    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&overlay));
    window.set_content(Some(&toasts));
    {
        let t = toasts.clone();
        app.set_toaster(move |m| {
            t.add_toast(adw::Toast::new(m));
        });
        let t = toasts.clone();
        app.set_action_toaster(move |m, label, cb| {
            let toast = adw::Toast::new(m);
            toast.set_button_label(Some(label));
            toast.connect_button_clicked(move |_| cb());
            t.add_toast(toast);
        });
    }

    let ui = Rc::new(Ui {
        window: window.clone(),
        stack: stack.clone(),
        browser_split: browser_split.clone(),
        inspector_split: inspector_split.clone(),
        sounds_toggle: sounds_toggle.clone(),
        inspector_toggle: inspector_toggle.clone(),
        undo,
        redo,
        narrow_title: narrow_title.clone(),
        transport: transport.clone(),
        pattern,
        banner: banner.clone(),
        agent_btn: agent_btn.clone(),
    });

    install_toggles(&ui);
    install_breakpoints(
        &ui,
        &app,
        &switcher,
        &switcher_bar,
        &narrow_title,
        &transport,
    );
    install_actions(gapp, &ui, &app);
    install_accels(gapp, &window, &ui);
    install_tick(&app);
    install_view_hooks(&ui, &app);
    install_close(&window, &app);
    {
        let (u, stack, a2) = (ui.clone(), inspector.stack.clone(), app.clone());
        app.on_command(move |c| match c {
            UiCommand::ShowSound => {
                stack.set_visible_child_name("sound");
                u.inspector_split.set_show_sidebar(true);
            }
            UiCommand::ShowSounds => u.browser_split.set_show_sidebar(true),
            UiCommand::ShowAgent => {
                stack.set_visible_child_name("agent");
                u.inspector_split.set_show_sidebar(true);
            }
            UiCommand::AgentChanged => update_agent_ui(&u, &a2),
            UiCommand::EditNotes => {}
            UiCommand::RenameChannel(id) => u.pattern.channels.rename(id),
        });
    }
    {
        let (u, a) = (ui.clone(), app.clone());
        banner.connect_button_clicked({
            let a = a.clone();
            move |_| a.command(UiCommand::ShowAgent)
        });
        agent_btn.connect_clicked({
            let a = app.clone();
            move |_| a.command(UiCommand::ShowAgent)
        });
        update_agent_ui(&u, &a);
    }

    {
        let (u, a) = (ui.clone(), app.clone());
        app.on_change(move || sync_header(&u, &a));
    }
    sync_header(&ui, &app);
    install_debug_shot(gapp, &ui, &app);
    window
}

/// Debug aids for screenshots without a screenshot tool:
/// `LIBREDAW_PAGE=pattern|song|mixer` picks the page,
/// `LIBREDAW_THEME=light|dark` the color scheme, and
/// `LIBREDAW_SHOT=/path.png` writes the window to a PNG a moment after it
/// is shown and quits. `LIBREDAW_SIZE=360x640` sets the size, and
/// `LIBREDAW_CHANNEL=Name` selects the channel of that name.
fn install_debug_shot(gapp: &adw::Application, ui: &Rc<Ui>, app: &Rc<App>) {
    if let Ok(p) = std::env::var("LIBREDAW_PAGE") {
        // After the project's saved view has been restored.
        let u = ui.clone();
        glib::timeout_add_local_once(Duration::from_millis(900), move || go_to_page(&u, &p));
    }
    // LIBREDAW_EDIT_NOTES=1 opens the notes page of the selected channel.
    if std::env::var_os("LIBREDAW_EDIT_NOTES").is_some() {
        let a = app.clone();
        glib::timeout_add_local_once(Duration::from_millis(1400), move || a.edit_notes(None));
    }
    if let Ok(name) = std::env::var("LIBREDAW_CHANNEL") {
        let a = app.clone();
        glib::timeout_add_local_once(Duration::from_millis(1000), move || {
            let id = {
                let s = a.session.borrow();
                s.document()
                    .project
                    .channels
                    .iter()
                    .find(|c| c.name == name)
                    .map(|c| c.id)
            };
            if let Some(id) = id {
                a.select_channel(id);
            }
        });
    }
    if let Ok(p) = std::env::var("LIBREDAW_PANES") {
        // "sounds", "inspector", or both separated by a comma.
        let u = ui.clone();
        glib::timeout_add_local_once(Duration::from_millis(900), move || {
            u.browser_split.set_show_sidebar(p.contains("sounds"));
            u.inspector_split.set_show_sidebar(p.contains("inspector"));
        });
    }
    match std::env::var("LIBREDAW_THEME").as_deref() {
        Ok("dark") => adw::StyleManager::default().set_color_scheme(adw::ColorScheme::ForceDark),
        Ok("light") => adw::StyleManager::default().set_color_scheme(adw::ColorScheme::ForceLight),
        _ => {}
    }
    // LIBREDAW_PLAY=1 starts playback after a second (drawing numbers).
    if std::env::var_os("LIBREDAW_PLAY").is_some() {
        let a = ui.window.clone();
        glib::timeout_add_local_once(Duration::from_millis(1000), move || {
            gtk::prelude::ActionGroupExt::activate_action(&a, "play-pause", None);
        });
    }
    let Ok(path) = std::env::var("LIBREDAW_SHOT") else {
        return;
    };
    let delay = std::env::var("LIBREDAW_SHOT_DELAY_MS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(2500);
    let (gapp, window) = (gapp.clone(), ui.window.clone());
    glib::timeout_add_local_once(Duration::from_millis(delay), move || {
        let (w, h) = (window.width(), window.height());
        let paintable = gtk::WidgetPaintable::new(Some(&window));
        let snap = gtk::Snapshot::new();
        paintable.snapshot(&snap, w as f64, h as f64);
        let saved = match (snap.to_node(), window.native().and_then(|n| n.renderer())) {
            (Some(node), Some(r)) => {
                let t = r.render_texture(
                    &node,
                    Some(&gtk::graphene::Rect::new(0.0, 0.0, w as f32, h as f32)),
                );
                let r = t.save_to_png(&path);
                if let Err(e) = &r {
                    eprintln!("libredaw: png: {e}");
                }
                r.is_ok()
            }
            (n, r) => {
                eprintln!(
                    "libredaw: no node {} or renderer {}",
                    n.is_none(),
                    r.is_none()
                );
                false
            }
        };
        eprintln!("libredaw: screenshot {path} {w}x{h}: {saved}");
        gapp.quit();
    });
}

/// Binds the two pane toggles to their split views and sets tooltips.
fn install_toggles(ui: &Rc<Ui>) {
    for (toggle, split, name, action) in [
        (&ui.sounds_toggle, &ui.browser_split, "Sounds", "win.sounds"),
        (
            &ui.inspector_toggle,
            &ui.inspector_split,
            "Inspector",
            "win.inspector",
        ),
    ] {
        toggle
            .bind_property("active", split, "show-sidebar")
            .bidirectional()
            .sync_create()
            .build();
        let update = {
            let (t, name, action) = (toggle.clone(), name, action);
            move || {
                let verb = if t.is_active() { "Hide" } else { "Show" };
                t.set_tooltip_text(Some(&shortcuts::tooltip(&format!("{verb} {name}"), action)));
            }
        };
        update();
        toggle.connect_toggled(move |_| update());
    }
    // Overlaid panes: opening one closes the other (they would cover each
    // other), and Escape closes the open one (libadwaita does that).
    for (this, other) in [
        (&ui.sounds_toggle, &ui.inspector_toggle),
        (&ui.inspector_toggle, &ui.sounds_toggle),
    ] {
        let (this_c, other_c, u) = (this.clone(), other.clone(), ui.clone());
        this.connect_toggled(move |_| {
            if this_c.is_active() && u.browser_split.is_collapsed() {
                other_c.set_active(false);
            }
        });
    }
}

/// `AdwBreakpoint`s with declarative setters for everything that changes
/// the minimum width (so the window can shrink to 360 px), and handlers that
/// publish the size class for the rest.
///
/// libadwaita applies only the last matching breakpoint, so each one lists
/// the setters of the ones before it (regular, then compact, then narrow,
/// then landscape). Height does not change the minimum width, so the short
/// class is not a breakpoint: `install_height_watch` follows the height.
fn install_breakpoints(
    ui: &Rc<Ui>,
    app: &Rc<App>,
    switcher: &adw::ViewSwitcher,
    switcher_bar: &adw::ViewSwitcherBar,
    narrow_title: &adw::WindowTitle,
    transport: &Rc<Transport>,
) {
    use adw::{BreakpointCondition as Cond, BreakpointConditionLengthType as Len, LengthUnit};
    let max_w = |v: u32| Cond::new_length(Len::MaxWidth, v as f64, LengthUnit::Sp);
    let max_h = |v: u32| Cond::new_length(Len::MaxHeight, v as f64, LengthUnit::Px);
    let on = true.to_value();
    let off = false.to_value();

    let regular = adw::Breakpoint::new(max_w(size_class::REGULAR_MAX_SP));
    let compact = adw::Breakpoint::new(max_w(size_class::COMPACT_MAX_SP));
    let narrow = adw::Breakpoint::new(max_w(size_class::NARROW_MAX_SP));
    let landscape = adw::Breakpoint::new(Cond::new_and(
        max_w(size_class::NARROW_MAX_SP),
        max_h(size_class::LANDSCAPE_MAX_PX),
    ));
    for bp in [&regular, &compact, &narrow, &landscape] {
        bp.add_setter(&ui.browser_split, "collapsed", Some(&on));
        bp.add_setter(&ui.inspector_split, "collapsed", Some(&on));
    }
    for bp in [&compact, &narrow, &landscape] {
        transport.add_compact_setters(bp);
    }
    for bp in [&narrow, &landscape] {
        transport.add_narrow_setters(bp);
        bp.add_setter(&ui.inspector_toggle, "visible", Some(&off));
    }
    narrow.add_setter(switcher, "visible", Some(&off));
    narrow.add_setter(narrow_title, "visible", Some(&on));
    narrow.add_setter(switcher_bar, "reveal", Some(&on));
    // A landscape phone keeps the switcher in the header: it has the
    // narrow setters except for the three above.
    landscape.add_setter(switcher, "visible", Some(&on));
    landscape.add_setter(narrow_title, "visible", Some(&off));
    landscape.add_setter(switcher_bar, "reveal", Some(&off));

    // [regular, compact, narrow, landscape]
    let flags = Rc::new(Cell::new([false; 4]));
    let short = Rc::new(Cell::new(false));
    let publish: Rc<dyn Fn()> = {
        let (flags, short, app, ui) = (flags.clone(), short.clone(), app.clone(), ui.clone());
        Rc::new(move || {
            let f = flags.get();
            let c = SizeClass::from_flags(
                f[0] || f[1] || f[2] || f[3],
                f[1] || f[2] || f[3],
                f[2] || f[3],
                short.get(),
            )
            .with_landscape(f[3]);
            if crate::perf::enabled() {
                eprintln!("libredaw: size class {c:?}");
            }
            app.set_size_class(c);
            if c.touch() {
                ui.window.add_css_class("touch");
            } else {
                ui.window.remove_css_class("touch");
            }
            ui.transport.apply_size(c);
        })
    };
    for (i, bp) in [&regular, &compact, &narrow, &landscape]
        .into_iter()
        .enumerate()
    {
        let (fl, p) = (flags.clone(), publish.clone());
        bp.connect_apply(move |_| {
            let mut f = fl.get();
            f[i] = true;
            fl.set(f);
            p();
        });
        let (fl, p) = (flags.clone(), publish.clone());
        bp.connect_unapply(move |_| {
            let mut f = fl.get();
            f[i] = false;
            fl.set(f);
            p();
        });
        ui.window.add_breakpoint(bp.clone());
    }

    // The short class follows the window height.
    {
        let (w, short, p) = (ui.window.clone(), short, publish.clone());
        ui.window.add_tick_callback(move |_, _| {
            let h = w.height();
            if h > 0 {
                let s = h <= size_class::SHORT_MAX_PX as i32;
                if short.replace(s) != s {
                    p();
                }
            }
            glib::ControlFlow::Continue
        });
    }
    publish();
}

/// The approval banner and the agent indicator follow the control state.
fn update_agent_ui(ui: &Ui, app: &App) {
    let state = app
        .bridge
        .borrow()
        .as_ref()
        .map(|b| b.ui.clone())
        .unwrap_or_default();
    match crate::agent_panel::banner_title(&state) {
        Some(t) => {
            ui.banner.set_title(&t);
            ui.banner.set_revealed(true);
        }
        None => ui.banner.set_revealed(false),
    }
    ui.agent_btn
        .set_visible(state.enabled && !state.clients.is_empty());
}

fn page_name(ui: &Ui) -> String {
    ui.stack
        .visible_child_name()
        .map(|s| s.to_string())
        .unwrap_or_else(|| "pattern".into())
}

fn sync_header(ui: &Ui, app: &App) {
    ui.undo.set_sensitive(app.can_undo());
    ui.redo.set_sensitive(app.can_redo());
    let undo_tip = if app.can_undo() {
        "Undo"
    } else {
        "Nothing to Undo"
    };
    let redo_tip = if app.can_redo() {
        "Redo"
    } else {
        "Nothing to Redo"
    };
    ui.undo
        .set_tooltip_text(Some(&shortcuts::tooltip(undo_tip, "win.undo")));
    ui.redo
        .set_tooltip_text(Some(&shortcuts::tooltip(redo_tip, "win.redo")));
    let name = files::display_name(&app.ui.borrow().path);
    let dirty = app.is_dirty();
    let shown = if dirty {
        format!("• {name}")
    } else {
        name.clone()
    };
    ui.narrow_title.set_title(&shown);
    ui.window.set_title(Some(&format!("{shown} - LibreDAW")));
    let sub = match app.ui.borrow().audio_error.clone() {
        Some(_) => "Audio is off".to_string(),
        None => String::new(),
    };
    ui.narrow_title.set_subtitle(&sub);
    ui.transport.sync();
}

fn go_to_page(ui: &Ui, page: &str) {
    ui.stack.set_visible_child_name(page);
}

/// `win.*` and `app.*` actions.
fn install_actions(gapp: &adw::Application, ui: &Rc<Ui>, app: &Rc<App>) {
    let window = &ui.window;
    let add = |name: &str, f: Box<dyn Fn()>| {
        let a = gio::SimpleAction::new(name, None);
        a.connect_activate(move |_, _| f());
        window.add_action(&a);
    };
    let a = app.clone();
    add("undo", Box::new(move || a.undo()));
    let a = app.clone();
    add("redo", Box::new(move || a.redo()));
    let a = app.clone();
    add("play-pause", Box::new(move || a.toggle_play()));
    let a = app.clone();
    add(
        "play-from-start",
        Box::new(move || {
            seek_start(&a);
            a.play();
        }),
    );
    let a = app.clone();
    add("go-start", Box::new(move || seek_start(&a)));
    let a = app.clone();
    add(
        "metronome",
        Box::new(move || {
            let (on, gain) = {
                let s = a.session.borrow();
                let m = &s.document().project.metronome;
                (m.enabled, m.gain_db)
            };
            a.edit(vec![Edit::SetMetronome {
                enabled: !on,
                gain_db: gain,
            }]);
        }),
    );
    let (a, w) = (app.clone(), window.clone());
    add("save", Box::new(move || files::save(&w, &a)));
    let (a, w) = (app.clone(), window.clone());
    add("save-as", Box::new(move || files::save_as(&w, &a)));
    let (a, w) = (app.clone(), window.clone());
    add("open", Box::new(move || files::open(&w, &a)));
    let a = app.clone();
    add("new", Box::new(move || files::new_project(&a)));
    let (a, w) = (app.clone(), window.clone());
    add("export", Box::new(move || export::show(&w, &a)));
    let w = window.clone();
    add(
        "show-help-overlay",
        Box::new(move || help::show_shortcuts(&w)),
    );
    let w = window.clone();
    add("close", Box::new(move || w.close()));

    // View.
    let u = ui.clone();
    add(
        "sounds",
        Box::new(move || u.sounds_toggle.set_active(!u.sounds_toggle.is_active())),
    );
    let u = ui.clone();
    add(
        "inspector",
        Box::new(move || {
            if u.inspector_toggle.is_visible() {
                u.inspector_toggle
                    .set_active(!u.inspector_toggle.is_active());
            }
        }),
    );
    for (name, page) in [
        ("view-pattern", "pattern"),
        ("view-song", "song"),
        ("view-mixer", "mixer"),
    ] {
        let u = ui.clone();
        add(name, Box::new(move || go_to_page(&u, page)));
    }
    let u = ui.clone();
    add("zoom-in", Box::new(move || u.pattern.roll.zoom_x(1.3)));
    let u = ui.clone();
    add(
        "zoom-out",
        Box::new(move || u.pattern.roll.zoom_x(1.0 / 1.3)),
    );
    let u = ui.clone();
    add("zoom-reset", Box::new(move || u.pattern.roll.reset_zoom()));
    let a = app.clone();
    add("edit-notes", Box::new(move || a.edit_notes(None)));

    // Channels and plugins.
    let u = ui.clone();
    add(
        "rename",
        Box::new(move || u.pattern.channels.rename_selected()),
    );
    let a = app.clone();
    let preset = gio::SimpleAction::new("add-preset", Some(glib::VariantTy::STRING));
    preset.connect_activate(move |_, v| {
        if let Some(name) = v.and_then(|v| v.get::<String>()) {
            channels::add(&a, NewChannel::Preset(name));
        }
    });
    window.add_action(&preset);
    let a = app.clone();
    add(
        "add-808",
        Box::new(move || {
            channels::add(&a, NewChannel::Bass808);
        }),
    );
    let (a, w) = (app.clone(), window.clone());
    add(
        "add-sampler",
        Box::new(move || crate::samples_ui::choose_for_new_channels(&w, &a)),
    );
    let (a, w) = (app.clone(), window.clone());
    add(
        "add-instrument",
        Box::new(move || {
            let a2 = a.clone();
            dialogs::choose_plugin(&w, &a, PluginKind::Instrument, "Add Instrument", move |d| {
                channels::add(
                    &a2,
                    NewChannel::Plugin {
                        id: d.id.clone(),
                        name: d.name.clone(),
                    },
                );
            });
        }),
    );
    let (a, w) = (app.clone(), window.clone());
    add(
        "add-plugin",
        Box::new(move || {
            let a2 = a.clone();
            dialogs::choose_plugin(&w, &a, PluginKind::Any, "Add Plugin", move |d| {
                if d.instrument {
                    channels::add(
                        &a2,
                        NewChannel::Plugin {
                            id: d.id.clone(),
                            name: d.name.clone(),
                        },
                    );
                } else {
                    let track = a2.ui.borrow().track;
                    let n = a2
                        .session
                        .borrow()
                        .document()
                        .project
                        .track(track)
                        .map(|t| t.inserts.len())
                        .unwrap_or(0);
                    a2.edit(vec![Edit::AddInsert {
                        track,
                        index: n.min(255) as u8,
                        plugin_id: d.id.clone(),
                    }]);
                }
            });
        }),
    );

    // Application-level actions.
    let add_app = |name: &str, f: Box<dyn Fn()>| {
        let a = gio::SimpleAction::new(name, None);
        a.connect_activate(move |_, _| f());
        gapp.add_action(&a);
    };
    let (a, w) = (app.clone(), window.clone());
    add_app("preferences", Box::new(move || prefs::show(&w, &a)));
    let w = window.clone();
    add_app("about", Box::new(move || help::show_about(&w)));
    let w = window.clone();
    add_app("quit", Box::new(move || w.close()));
}

fn seek_start(app: &App) {
    let _ = app
        .session
        .borrow_mut()
        .link
        .command(protocol::engine::EngineCommand::Seek { tick: 0 });
}

/// Does the focused widget use this key itself?
fn focus_uses_space(focus: &gtk::Widget) -> bool {
    let mut w = Some(focus.clone());
    for _ in 0..4 {
        let Some(x) = w else { return false };
        if x.is::<gtk::Editable>()
            || x.is::<gtk::Button>()
            || x.is::<gtk::CheckButton>()
            || x.is::<gtk::Switch>()
            || x.is::<gtk::DropDown>()
            || x.is::<gtk::MenuButton>()
            || x.is::<gtk::ListBoxRow>()
            || x.is::<gtk::ListView>()
            || x.is::<gtk::GridView>()
            || x.is::<gtk::ColumnView>()
        {
            return true;
        }
        w = x.parent();
    }
    false
}

fn focus_uses_home(focus: &gtk::Widget) -> bool {
    let mut w = Some(focus.clone());
    for _ in 0..4 {
        let Some(x) = w else { return false };
        if x.is::<gtk::Editable>()
            || x.is::<gtk::Scale>()
            || x.is::<gtk::ListBoxRow>()
            || x.is::<gtk::ListView>()
            || x.is::<crate::widgets::step_grid::StepGrid>()
            || x.is::<crate::widgets::piano_roll::PianoRoll>()
        {
            return true;
        }
        w = x.parent();
    }
    false
}

/// Accelerators from the shortcut table. Space, Shift+Space, and Home go
/// through a capture-phase controller that steps aside when the focused
/// widget uses the key (docs/ui-design.md 5.1).
fn install_accels(gapp: &adw::Application, window: &adw::ApplicationWindow, ui: &Rc<Ui>) {
    let _ = ui;
    let conditional = ["win.play-pause", "win.play-from-start", "win.go-start"];
    for sc in SHORTCUTS {
        if sc.action.is_empty() || conditional.contains(&sc.action) {
            continue;
        }
        gapp.set_accels_for_action(sc.action, sc.accels);
    }
    gapp.set_accels_for_action("win.redo", &["<Control><Shift>z", "<Control>y"]);

    let ctl = gtk::ShortcutController::new();
    ctl.set_propagation_phase(gtk::PropagationPhase::Capture);
    for action in conditional {
        let Some(sc) = SHORTCUTS.iter().find(|s| s.action == action) else {
            continue;
        };
        let is_home = action == "win.go-start";
        let win = window.clone();
        let name = action.to_string();
        for accel in sc.accels {
            let Some(trigger) = gtk::ShortcutTrigger::parse_string(accel) else {
                continue;
            };
            let (win, name) = (win.clone(), name.clone());
            let act = gtk::CallbackAction::new(move |_, _| {
                use gtk::prelude::GtkWindowExt;
                if let Some(f) = GtkWindowExt::focus(&win) {
                    let uses = if is_home {
                        focus_uses_home(&f)
                    } else {
                        focus_uses_space(&f)
                    };
                    if uses {
                        return glib::Propagation::Proceed;
                    }
                }
                let n = name.strip_prefix("win.").unwrap_or(&name);
                gtk::prelude::ActionGroupExt::activate_action(&win, n, None);
                glib::Propagation::Stop
            });
            ctl.add_shortcut(gtk::Shortcut::new(Some(trigger), Some(act)));
        }
    }
    window.add_controller(ctl);
}

/// Saves and restores the window view (`.view.toml`).
fn install_view_hooks(ui: &Rc<Ui>, app: &Rc<App>) {
    let (u1, u2) = (ui.clone(), ui.clone());
    app.set_view_hooks(
        move || {
            let (px_per_tick, row_h, scroll_x, scroll_y, snap) = u1.pattern.roll.view_params();
            ViewState {
                px_per_tick,
                row_h,
                scroll_x,
                scroll_y,
                snap: snap as u32,
                page: page_name(&u1),
                sounds_open: u1.sounds_toggle.is_active(),
                inspector_open: u1.inspector_toggle.is_active(),
                ..ViewState::default()
            }
        },
        move |v| {
            u2.pattern.roll.set_view_params(
                v.px_per_tick,
                v.row_h,
                v.scroll_x,
                v.scroll_y,
                v.snap as usize,
            );
            u2.pattern
                .snap
                .set_selected(u2.pattern.roll.snap_index() as u32);
            go_to_page(&u2, &v.page);
            u2.sounds_toggle.set_active(v.sounds_open);
            u2.inspector_toggle.set_active(v.inspector_open);
        },
    );
}

/// Closing saves first (Amendment 9): no "save changes?" dialog. If the
/// save fails the window stays open and the error is shown.
fn install_close(window: &adw::ApplicationWindow, app: &Rc<App>) {
    let closing = Rc::new(Cell::new(false));
    let saved_ok = Rc::new(Cell::new(false));
    let a = app.clone();
    window.connect_close_request(move |win| {
        if saved_ok.get() {
            if let Some(b) = a.bridge.borrow_mut().take() {
                b.shutdown();
            }
            a.session.borrow_mut().link.stop();
            a.session.borrow_mut().shutdown();
            return glib::Propagation::Proceed;
        }
        if closing.replace(true) {
            return glib::Propagation::Stop; // a save is already running
        }
        let (win2, closing2, ok2, a2) = (win.clone(), closing.clone(), saved_ok.clone(), a.clone());
        // Unsaved work is saved (a never-saved project goes to ~/Music); a
        // clean project only gets its view written.
        let needs_save = a.is_dirty();
        let finish = move |r: Result<std::path::PathBuf, String>| {
            closing2.set(false);
            if r.is_ok() {
                ok2.set(true);
                // Not inside the close-request handler: close again from
                // the main loop.
                let w = win2.clone();
                glib::idle_add_local_once(move || w.close());
            }
        };
        if needs_save {
            files::save_current(&a2, finish);
        } else {
            // Nothing to save: just record the view next to the project.
            if let Some(p) = a.ui.borrow().path.clone()
                && let Some(v) = a.collect_view()
            {
                let _ = v.write(&p);
            }
            finish(Ok(std::path::PathBuf::new()));
        }
        glib::Propagation::Stop
    });
}

/// The 10 ms source (4.4) plus autosave (7.6, Amendment 10): 3 s after the
/// last edit and at least every 15 s while edits continue. The autosave
/// writes the document as it is; plugin state is captured by the session on
/// its own schedule (every 60 s when a plugin reported changes, and after
/// plugin gestures).
fn install_tick(app: &Rc<App>) {
    let worker = Rc::new(AutosaveWorker::spawn());
    let debounce = Rc::new(RefCell::new(AutosaveDebounce::standard()));
    let seen_revision = Rc::new(Cell::new(app.session.borrow().document().revision));
    let a = app.clone();
    glib::timeout_add_local(Duration::from_millis(10), move || {
        let now = Instant::now();
        let report = a.session.borrow_mut().tick_at(now);
        crate::control_bridge::on_done(&a, report.done);
        crate::control_bridge::tick(&a);
        if report.changed {
            a.notify();
        }
        a.tasks.poll();
        a.poll_peaks();

        // Autosave: note changes, write when due.
        let (rev, dirty) = {
            let s = a.session.borrow();
            (s.document().revision, s.editor.is_dirty())
        };
        if rev != seen_revision.get() {
            seen_revision.set(rev);
            if dirty {
                debounce.borrow_mut().changed(now);
            }
        }
        if !dirty {
            debounce.borrow_mut().clear();
        }
        if debounce.borrow_mut().due(now) {
            let doc = a.session.borrow().document().clone();
            worker.submit(files::autosave_dir(&a), doc);
        }
        while let Ok(r) = worker.results.try_recv() {
            match r.result {
                Ok(_) => {
                    // Our own recovery bundle exists now: the crashed
                    // session's one is no longer needed.
                    if let Some(old) = a.stale_recovery.borrow_mut().take() {
                        let _ = std::fs::remove_dir_all(old);
                    }
                }
                Err(e) => a.toast(&format!("Autosave failed: {e}")),
            }
        }

        // Keep the transport button in step with the engine.
        let playing = a
            .session
            .borrow()
            .link
            .status
            .playing
            .load(std::sync::atomic::Ordering::Relaxed);
        if a.session.borrow().link.is_live() && a.ui.borrow().playing != playing {
            a.ui.borrow_mut().playing = playing;
            a.notify();
        }
        glib::ControlFlow::Continue
    });
}
