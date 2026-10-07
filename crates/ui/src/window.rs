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
use crate::prefs;
use crate::shortcuts::{self, SHORTCUTS};
use crate::size_class::{self, SizeClass};
use crate::timeline_page::{self, TimelinePage};
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
    timeline: TimelinePage,
    mixer: Rc<Mixer>,
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
        .title("Oto")
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
    menu_button.set_menu_model(Some(&crate::menus::main_menu()));
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

    // ---- the content: header, transport, banners, pages ----
    // Each pane has its own toolbar view and header bar, as in GNOME's own
    // split-view apps; the panes' headers line up with the content header
    // by construction, and libadwaita shows the window buttons only on the
    // outermost headers.
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

    let timeline = timeline_page::build(&app);
    let mixer = Mixer::new(app.clone());
    stack.add_titled_with_icon(
        &timeline.widget,
        Some("timeline"),
        "Timeline",
        "view-continuous-symbolic",
    );
    stack.add_titled_with_icon(
        &mixer.widget(),
        Some("mixer"),
        "Mixer",
        "audio-volume-high-symbolic",
    );

    let transport = Transport::new(&app);
    let banner = adw::Banner::new("");
    banner.set_button_label(Some("Review"));
    banner.set_revealed(false);
    // Audio that could not start stays visible until it works.
    let audio_banner = adw::Banner::new("");
    audio_banner.set_button_label(Some("Retry"));
    audio_banner.set_revealed(false);
    // Toasts sit over the pages, above the bottom view switcher.
    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&stack));
    let switcher_bar = adw::ViewSwitcherBar::new();
    switcher_bar.set_stack(Some(&stack));
    let center = adw::ToolbarView::new();
    center.add_top_bar(&header);
    center.add_top_bar(&transport.bar);
    center.add_top_bar(&audio_banner);
    center.add_top_bar(&banner);
    center.set_content(Some(&toasts));
    center.add_bottom_bar(&switcher_bar);

    // ---- side panes ----
    // Pinned beside the content while there is room, over it when the
    // window is narrow; `pin-sidebar` keeps the user's open or closed
    // choice when that changes.
    let inspector = inspector::build(&app);
    let inspector_split = adw::OverlaySplitView::builder()
        .sidebar_position(gtk::PackType::End)
        .min_sidebar_width(280.0)
        .max_sidebar_width(360.0)
        .sidebar_width_unit(adw::LengthUnit::Sp)
        .sidebar_width_fraction(0.25)
        .pin_sidebar(true)
        .show_sidebar(false)
        .build();
    inspector_split.set_sidebar(Some(&inspector.widget));
    inspector_split.set_content(Some(&center));
    let sounds = adw::ToolbarView::new();
    let sounds_header = adw::HeaderBar::new();
    sounds_header.set_title_widget(Some(&adw::WindowTitle::new("Sounds", "")));
    sounds.add_top_bar(&sounds_header);
    sounds.set_content(Some(&browser::build(&app)));
    let browser_split = adw::OverlaySplitView::builder()
        .sidebar_position(gtk::PackType::Start)
        .min_sidebar_width(260.0)
        .max_sidebar_width(340.0)
        .sidebar_width_unit(adw::LengthUnit::Sp)
        .sidebar_width_fraction(0.22)
        .pin_sidebar(true)
        .show_sidebar(false)
        .build();
    browser_split.set_sidebar(Some(&sounds));
    browser_split.set_content(Some(&inspector_split));

    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&browser_split));
    overlay.add_overlay(&palette::install());
    window.set_content(Some(&overlay));
    {
        // One toast per message: a repeat of a toast that is showing is
        // dropped instead of queued behind it; different ones queue.
        let shown: Rc<RefCell<Vec<String>>> = Rc::default();
        let (t, s) = (toasts.clone(), shown.clone());
        let add = Rc::new(move |toast: adw::Toast| {
            // Messages are plain text (names can hold "&" or "<").
            toast.set_use_markup(false);
            let title = toast.title().map(|t| t.to_string()).unwrap_or_default();
            if s.borrow().contains(&title) {
                return;
            }
            s.borrow_mut().push(title.clone());
            let s2 = s.clone();
            toast.connect_dismissed(move |_| s2.borrow_mut().retain(|x| x != &title));
            t.add_toast(toast);
        });
        let a2 = add.clone();
        app.set_toaster(move |m| a2(adw::Toast::new(m)));
        app.set_action_toaster(move |m, label, cb| {
            let toast = adw::Toast::new(m);
            toast.set_button_label(Some(label));
            toast.connect_button_clicked(move |_| cb());
            add(toast);
        });
    }
    install_audio_banner(&audio_banner, &app);

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
        timeline,
        mixer: mixer.clone(),
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
            UiCommand::RenameChannel(id) => u.timeline.channels.rename(id),
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
    // The keyboard starts in the content (the step grid), not nowhere.
    {
        let u = ui.clone();
        window.connect_map(move |_| {
            let u = u.clone();
            glib::idle_add_local_once(move || {
                if gtk::prelude::GtkWindowExt::focus(&u.window).is_none() {
                    u.timeline.timeline.grab_focus();
                }
            });
        });
    }
    install_debug_shot(gapp, &ui, &app);
    crate::debug_shot::install(ui.window.upcast_ref());
    window
}

/// Debug aids for screenshots without a screenshot tool:
/// `LIBREDAW_PAGE=timeline|mixer` picks the page,
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
                let text = match (t.is_active(), name) {
                    (true, n) => format!("Hide {n}"),
                    (false, "Sounds") => "Show Sounds – pick a sound to add an instrument".into(),
                    (false, n) => format!("Show {n}"),
                };
                t.set_tooltip_text(Some(&shortcuts::tooltip(&text, action)));
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
    // Both panes overlay below 1100 sp: the content would get less than
    // about 600 sp beside the sounds pane. Only setters, no size class.
    let medium = adw::Breakpoint::new(max_w(1100));
    medium.add_setter(&ui.browser_split, "collapsed", Some(&true.to_value()));
    medium.add_setter(&ui.inspector_split, "collapsed", Some(&true.to_value()));
    let narrow = adw::Breakpoint::new(max_w(size_class::NARROW_MAX_SP));
    let landscape = adw::Breakpoint::new(Cond::new_and(
        max_w(size_class::NARROW_MAX_SP),
        max_h(size_class::LANDSCAPE_MAX_PX),
    ));
    // The panes sit beside the content while it keeps about 600 sp; below
    // that they overlay it. Only whether they overlap changes, never
    // whether they are open.
    // Below 1400 sp the inspector overlays (both panes would leave the
    // content less than about 600 sp); below 900 sp the sounds pane too.
    for bp in [&regular, &compact, &narrow, &landscape] {
        bp.add_setter(&ui.inspector_split, "collapsed", Some(&on));
    }
    for bp in [&compact, &narrow, &landscape] {
        bp.add_setter(&ui.browser_split, "collapsed", Some(&on));
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
        if i == 0 {
            // Between regular and compact (the last matching breakpoint wins).
            ui.window.add_breakpoint(medium.clone());
        }
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

/// "Audio is off: <reason>" with Retry, shown while there is no audio
/// stream (a toast would be gone before the user read it).
fn install_audio_banner(banner: &adw::Banner, app: &Rc<App>) {
    banner.set_use_markup(false);
    let sync = {
        let (b, a) = (banner.clone(), app.clone());
        move || {
            let err = a.ui.borrow().audio_error.clone();
            match err {
                Some(e) => {
                    let title = format!("Audio is off: {e}");
                    if b.title() != title {
                        b.set_title(&title);
                    }
                    b.set_revealed(true);
                }
                None => b.set_revealed(false),
            }
        }
    };
    sync();
    app.on_change(sync.clone());
    let a = app.clone();
    banner.connect_button_clicked(move |_| {
        let configs = crate::run::audio_configs(&a.settings.borrow());
        let r = a.session.borrow_mut().start_audio(&configs);
        match r {
            Ok(()) => {
                a.ui.borrow_mut().audio_error = None;
                a.toast("Audio is on");
            }
            Err(e) => {
                a.ui.borrow_mut().audio_error = Some(e);
                a.toast("Audio is still off");
            }
        }
        a.notify();
        sync();
    });
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
        .unwrap_or_else(|| "timeline".into())
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
    ui.window.set_title(Some(&format!("{shown} - Oto")));
    let sub = match app.ui.borrow().audio_error.clone() {
        Some(_) => "Audio is off".to_string(),
        None => String::new(),
    };
    ui.narrow_title.set_subtitle(&sub);
    ui.transport.sync();
}

fn go_to_page(ui: &Ui, page: &str) {
    // Older view files name the "pattern" page: the timeline replaced it.
    let page = if ui.stack.child_by_name(page).is_some() {
        page
    } else {
        "timeline"
    };
    ui.stack.set_visible_child_name(page);
}

/// Ctrl++ / Ctrl+- / Ctrl+0: the piano roll when it has the keyboard,
/// else the timeline. `factor == 0` resets.
fn zoom(ui: &Ui, factor: f64) {
    let roll = &ui.timeline.editor.roll;
    let in_roll = roll.has_focus();
    match (in_roll, factor == 0.0) {
        (true, true) => roll.reset_zoom(),
        (true, false) => roll.zoom_x(factor),
        (false, true) => ui.timeline.timeline.reset_zoom(),
        (false, false) => ui.timeline.timeline.zoom_x(factor),
    }
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
    for (name, page) in [("view-timeline", "timeline"), ("view-mixer", "mixer")] {
        let u = ui.clone();
        add(name, Box::new(move || go_to_page(&u, page)));
    }
    let u = ui.clone();
    add("zoom-in", Box::new(move || zoom(&u, 1.3)));
    let u = ui.clone();
    add("zoom-out", Box::new(move || zoom(&u, 1.0 / 1.3)));
    let u = ui.clone();
    add("zoom-reset", Box::new(move || zoom(&u, 0.0)));
    let a = app.clone();
    add("edit-notes", Box::new(move || a.edit_notes(None)));

    // Channels and plugins.
    let u = ui.clone();
    add(
        "rename",
        Box::new(move || {
            // F2 renames what the page in front has selected.
            if page_name(&u) == "mixer" {
                u.mixer.rename_selected();
            } else {
                u.timeline.channels.rename_selected();
            }
        }),
    );
    crate::sound_picker::rescan_on_focus(window, app);
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
    let u = ui.clone();
    add(
        "add-sound",
        Box::new(move || {
            u.browser_split.set_show_sidebar(true);
            crate::browser::focus_search(u.browser_split.upcast_ref());
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
            let (px_per_tick, row_h, scroll_x, scroll_y, snap) =
                u1.timeline.editor.roll.view_params();
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
            u2.timeline.editor.roll.set_view_params(
                v.px_per_tick,
                v.row_h,
                v.scroll_x,
                v.scroll_y,
                v.snap as usize,
            );
            u2.timeline
                .editor
                .snap
                .set_selected(u2.timeline.editor.roll.snap_index() as u32);
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
    let play_sync = RefCell::new(crate::transport_logic::PlayingSync::default());
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
        let shown = a.ui.borrow().playing;
        if a.session.borrow().link.is_live()
            && let Some(now_playing) = play_sync.borrow_mut().step(shown, playing, now)
        {
            a.ui.borrow_mut().playing = now_playing;
            a.notify();
        }
        glib::ControlFlow::Continue
    });
}
