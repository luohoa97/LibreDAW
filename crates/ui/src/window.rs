// SPDX-License-Identifier: GPL-3.0-or-later
//! The application window (SPEC 11): header bar with transport, a channel
//! rack with step rows, the piano roll, and the mixer. The layout is our own.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use adw::prelude::*;
use gtk::gio;
use gtk::glib;

use protocol::consts::{MAX_STEPS, MAX_TEMPO_BPM, MIN_STEPS, MIN_TEMPO_BPM};
use protocol::edit::{Edit, NewInstrument};
use protocol::model::SynthParams;

use crate::app::App;
use crate::bundle::{AutosaveDebounce, AutosaveWorker};
use crate::dialogs::{self, PluginKind};
use crate::files;
use crate::mixer::Mixer;
use crate::persist::ViewState;
use crate::view_math::SNAPS;
use crate::widgets::piano_roll::PianoRoll;
use crate::widgets::step_grid::StepGrid;

/// Widgets of the header that mirror document state.
struct Header {
    title: adw::WindowTitle,
    play: gtk::Button,
    tempo: gtk::SpinButton,
    metro: gtk::ToggleButton,
    undo: gtk::Button,
    redo: gtk::Button,
    updating: Cell<bool>,
}

pub fn build(gapp: &adw::Application, app: Rc<App>) -> adw::ApplicationWindow {
    let window = adw::ApplicationWindow::builder()
        .application(gapp)
        .default_width(1280)
        .default_height(800)
        .title("LibreDAW")
        .build();
    let toasts = adw::ToastOverlay::new();
    {
        let t = toasts.clone();
        app.set_toaster(move |m| {
            t.add_toast(adw::Toast::new(m));
        });
    }

    let header = build_header(&window, &app);
    let rack = build_rack(&window, &app);
    let (roll, roll_widget, snap_box) = build_roll(&app);
    let mixer = Mixer::new(app.clone());

    // Left: rack over roll. Right: mixer.
    let left = gtk::Paned::new(gtk::Orientation::Vertical);
    left.set_start_child(Some(&rack));
    left.set_end_child(Some(&roll_widget));
    left.set_resize_start_child(false);
    left.set_shrink_start_child(false);
    left.set_position(250);
    let main = gtk::Paned::new(gtk::Orientation::Horizontal);
    main.set_start_child(Some(&left));
    main.set_end_child(Some(&mixer.widget()));
    main.set_resize_end_child(false);
    main.set_shrink_end_child(false);
    main.set_position(820);
    toasts.set_child(Some(&main));

    let view = adw::ToolbarView::new();
    view.add_top_bar(&header.0);
    view.set_content(Some(&toasts));
    window.set_content(Some(&view));

    install_actions(gapp, &window, &app);
    install_tick(&app);

    {
        let (r, l, m) = (roll.clone(), left.clone(), main.clone());
        let (r2, l2, m2, s2) = (roll.clone(), left.clone(), main.clone(), snap_box.clone());
        app.set_view_hooks(
            move || {
                let (px_per_tick, row_h, scroll_x, scroll_y, snap) = r.view_params();
                ViewState {
                    px_per_tick,
                    row_h,
                    scroll_x,
                    scroll_y,
                    snap: snap as u32,
                    split_rack: l.position(),
                    split_mixer: m.position(),
                    ..ViewState::default()
                }
            },
            move |v| {
                r2.set_view_params(
                    v.px_per_tick,
                    v.row_h,
                    v.scroll_x,
                    v.scroll_y,
                    v.snap as usize,
                );
                s2.set_selected(r2.snap_index() as u32);
                l2.set_position(v.split_rack.clamp(80, 2000));
                m2.set_position(v.split_mixer.clamp(300, 4000));
            },
        );
    }
    install_close(&window, &app);
    window
}

/// Closing saves first (Amendment 9): no "save changes?" dialog. If the
/// save fails the window stays open and the error is shown.
fn install_close(window: &adw::ApplicationWindow, app: &Rc<App>) {
    let closing = Rc::new(Cell::new(false));
    let saved_ok = Rc::new(Cell::new(false));
    let a = app.clone();
    window.connect_close_request(move |win| {
        if saved_ok.get() {
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

fn icon_button(icon: &str, tip: &str, action: Option<&str>) -> gtk::Button {
    let b = gtk::Button::from_icon_name(icon);
    b.set_tooltip_text(Some(tip));
    b.update_property(&[gtk::accessible::Property::Label(tip)]);
    if let Some(a) = action {
        b.set_action_name(Some(a));
    }
    b
}

fn build_header(window: &adw::ApplicationWindow, app: &Rc<App>) -> (adw::HeaderBar, Rc<Header>) {
    let bar = adw::HeaderBar::new();
    let title = adw::WindowTitle::new("Untitled", "LibreDAW");
    bar.set_title_widget(Some(&title));

    let open = icon_button(
        "document-open-symbolic",
        "Open project (Ctrl+O)",
        Some("win.open"),
    );
    let save = icon_button(
        "document-save-symbolic",
        "Save project (Ctrl+S)",
        Some("win.save"),
    );
    let undo = icon_button("edit-undo-symbolic", "Undo (Ctrl+Z)", Some("win.undo"));
    let redo = icon_button(
        "edit-redo-symbolic",
        "Redo (Ctrl+Shift+Z)",
        Some("win.redo"),
    );
    bar.pack_start(&open);
    bar.pack_start(&save);
    bar.pack_start(&undo);
    bar.pack_start(&redo);

    // Transport on the left of the title, after the file buttons.
    let transport = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let play = gtk::Button::from_icon_name("media-playback-start-symbolic");
    play.set_tooltip_text(Some("Play or pause (Ctrl+Space)"));
    play.update_property(&[gtk::accessible::Property::Label("Play")]);
    play.set_action_name(Some("win.play"));
    let stop = icon_button(
        "media-playback-stop-symbolic",
        "Stop and rewind",
        Some("win.stop"),
    );
    let tempo = gtk::SpinButton::with_range(MIN_TEMPO_BPM, MAX_TEMPO_BPM, 1.0);
    tempo.set_digits(1);
    tempo.set_width_chars(6);
    tempo.set_tooltip_text(Some("Tempo in beats per minute"));
    tempo.update_property(&[gtk::accessible::Property::Label("Tempo")]);
    let bpm = gtk::Label::new(Some("BPM"));
    bpm.add_css_class("dim-label");
    let metro = gtk::ToggleButton::new();
    metro.set_icon_name("alarm-symbolic");
    metro.set_tooltip_text(Some("Metronome"));
    metro.update_property(&[gtk::accessible::Property::Label("Metronome")]);
    transport.append(&play);
    transport.append(&stop);
    transport.append(&tempo);
    transport.append(&bpm);
    transport.append(&metro);
    bar.pack_start(&transport);

    // Main menu.
    let menu = gio::Menu::new();
    let file = gio::Menu::new();
    file.append(Some("New project"), Some("win.new"));
    file.append(Some("Save as…"), Some("win.save-as"));
    file.append(Some("Export WAV…"), Some("win.export"));
    menu.append_section(None, &file);
    let edit = gio::Menu::new();
    edit.append(Some("Add plugin…"), Some("win.add-plugin"));
    edit.append(Some("Allow agent control"), Some("win.agent"));
    menu.append_section(None, &edit);
    let app_menu = gio::Menu::new();
    app_menu.append(Some("About LibreDAW"), Some("win.about"));
    app_menu.append(Some("Quit"), Some("win.quit"));
    menu.append_section(None, &app_menu);
    let mb = gtk::MenuButton::new();
    mb.set_icon_name("open-menu-symbolic");
    mb.set_menu_model(Some(&menu));
    mb.set_tooltip_text(Some("Main menu"));
    bar.pack_end(&mb);

    let h = Rc::new(Header {
        title,
        play,
        tempo,
        metro,
        undo,
        redo,
        updating: Cell::new(false),
    });

    {
        let (h2, a) = (h.clone(), app.clone());
        h.tempo.connect_value_changed(move |s| {
            if h2.updating.get() {
                return;
            }
            let v = s.value();
            let cur = a.session.borrow().document().project.tempo_bpm;
            if (cur - v).abs() > 1e-9 {
                a.edit(vec![Edit::SetTempo { bpm: v }]);
            }
        });
        let (h2, a) = (h.clone(), app.clone());
        h.metro.connect_toggled(move |b| {
            if h2.updating.get() {
                return;
            }
            let gain = a.session.borrow().document().project.metronome.gain_db;
            a.edit(vec![Edit::SetMetronome {
                enabled: b.is_active(),
                gain_db: gain,
            }]);
        });
    }
    {
        let (h2, a, w) = (h.clone(), app.clone(), window.clone());
        a.on_change({
            let a = a.clone();
            move || sync_header(&h2, &a, &w)
        });
    }
    sync_header(&h, app, window);
    (bar, h)
}

fn sync_header(h: &Header, app: &App, window: &adw::ApplicationWindow) {
    h.updating.set(true);
    let (tempo, metro) = {
        let s = app.session.borrow();
        let p = &s.document().project;
        (p.tempo_bpm, p.metronome.enabled)
    };
    if (h.tempo.value() - tempo).abs() > 1e-9 {
        h.tempo.set_value(tempo);
    }
    h.metro.set_active(metro);
    h.updating.set(false);
    h.undo.set_sensitive(app.can_undo());
    h.redo.set_sensitive(app.can_redo());
    let playing = app.ui.borrow().playing;
    h.play.set_icon_name(if playing {
        "media-playback-pause-symbolic"
    } else {
        "media-playback-start-symbolic"
    });
    let name = files::display_name(&app.ui.borrow().path);
    let dirty = app.is_dirty();
    h.title
        .set_title(&format!("{}{}", if dirty { "• " } else { "" }, name));
    let sub = match app.ui.borrow().audio_error.clone() {
        Some(e) => format!("LibreDAW — audio off: {e}"),
        None => "LibreDAW".to_string(),
    };
    h.title.set_subtitle(&sub);
    window.set_title(Some(&format!("{name} — LibreDAW")));
}

/// Pattern bar, step grid, and the add channel button.
fn build_rack(window: &adw::ApplicationWindow, app: &Rc<App>) -> gtk::Widget {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);

    // Pattern bar.
    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    bar.add_css_class("toolbar");
    let plabel = gtk::Label::new(Some("Pattern"));
    plabel.add_css_class("heading");
    let patterns = gtk::DropDown::from_strings(&[]);
    patterns.set_tooltip_text(Some("Pattern shown in the rack and the piano roll"));
    let add_pattern = icon_button("list-add-symbolic", "New pattern", None);
    let del_pattern = icon_button("user-trash-symbolic", "Remove this pattern", None);
    let steps_label = gtk::Label::new(Some("Steps"));
    steps_label.add_css_class("dim-label");
    let steps = gtk::SpinButton::with_range(MIN_STEPS as f64, MAX_STEPS as f64, 1.0);
    steps.set_tooltip_text(Some("Pattern length in steps"));
    steps.update_property(&[gtk::accessible::Property::Label("Pattern length in steps")]);
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    let add_channel = gtk::MenuButton::new();
    add_channel.set_child(Some(
        &adw::ButtonContent::builder()
            .icon_name("list-add-symbolic")
            .label("Add channel")
            .build(),
    ));
    let cm = gio::Menu::new();
    cm.append(Some("Synth"), Some("win.add-synth"));
    cm.append(Some("Plugin instrument…"), Some("win.add-instrument"));
    add_channel.set_menu_model(Some(&cm));
    let rename = icon_button(
        "document-edit-symbolic",
        "Rename the selected channel",
        None,
    );
    bar.append(&plabel);
    bar.append(&patterns);
    bar.append(&add_pattern);
    bar.append(&del_pattern);
    bar.append(&steps_label);
    bar.append(&steps);
    bar.append(&spacer);
    bar.append(&rename);
    bar.append(&add_channel);
    root.append(&bar);
    root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

    let grid = StepGrid::new(app.clone());
    let sw = gtk::ScrolledWindow::builder()
        .child(&grid)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .build();
    root.append(&sw);

    let updating = Rc::new(Cell::new(false));
    let ids: Rc<RefCell<Vec<protocol::ids::PatternId>>> = Rc::new(RefCell::new(Vec::new()));

    {
        let a = app.clone();
        add_pattern.connect_clicked(move |_| {
            let n = a.session.borrow().document().project.patterns.len() + 1;
            if let Some(r) = a.edit(vec![Edit::AddPattern {
                name: format!("Pattern {n}"),
                length_steps: 16,
            }]) {
                a.select_pattern(protocol::ids::PatternId(r.created[0]));
            }
        });
        let a = app.clone();
        del_pattern.connect_clicked(move |_| {
            if let Some(p) = a.current_pattern() {
                a.edit(vec![Edit::RemovePattern { pattern: p }]);
            }
        });
        let (a, ids2, up) = (app.clone(), ids.clone(), updating.clone());
        patterns.connect_selected_notify(move |d| {
            if up.get() {
                return;
            }
            if let Some(id) = ids2.borrow().get(d.selected() as usize).copied() {
                a.select_pattern(id);
            }
        });
        let (a, up) = (app.clone(), updating.clone());
        steps.connect_value_changed(move |s| {
            if up.get() {
                return;
            }
            if let Some(p) = a.current_pattern() {
                let v = s.value() as u8;
                let cur = a
                    .session
                    .borrow()
                    .document()
                    .project
                    .pattern(p)
                    .map(|x| x.length_steps);
                if cur != Some(v) {
                    a.edit(vec![Edit::SetPatternLength {
                        pattern: p,
                        length_steps: v,
                    }]);
                }
            }
        });
        let (a, w) = (app.clone(), window.clone());
        rename.connect_clicked(move |_| {
            let Some(c) = a.current_channel() else { return };
            let cur = a
                .session
                .borrow()
                .document()
                .project
                .channel(c)
                .map(|x| x.name.clone())
                .unwrap_or_default();
            let a2 = a.clone();
            dialogs::ask_name(&w, "Rename channel", &cur, move |n| {
                a2.edit(vec![Edit::RenameChannel {
                    channel: c,
                    name: n,
                }]);
            });
        });
    }

    let sync = {
        let (a, ids, up, patterns, steps) = (
            app.clone(),
            ids.clone(),
            updating.clone(),
            patterns.clone(),
            steps.clone(),
        );
        move || {
            up.set(true);
            let (names, new_ids, sel_len) = {
                let s = a.session.borrow();
                let p = &s.document().project;
                let names: Vec<String> = p.patterns.iter().map(|x| x.name.clone()).collect();
                let new_ids: Vec<_> = p.patterns.iter().map(|x| x.id).collect();
                let len = a
                    .current_pattern()
                    .and_then(|id| p.pattern(id))
                    .map(|x| x.length_steps as f64);
                (names, new_ids, len)
            };
            let cur_names: Vec<String> = patterns
                .model()
                .and_then(|m| m.downcast::<gtk::StringList>().ok())
                .map(|l| {
                    (0..l.n_items())
                        .filter_map(|i| l.string(i).map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();
            if cur_names != names {
                let refs: Vec<&str> = names.iter().map(String::as_str).collect();
                patterns.set_model(Some(&gtk::StringList::new(&refs)));
            }
            *ids.borrow_mut() = new_ids.clone();
            if let Some(i) = a
                .current_pattern()
                .and_then(|id| new_ids.iter().position(|x| *x == id))
                && patterns.selected() as usize != i
            {
                patterns.set_selected(i as u32);
            }
            if let Some(l) = sel_len
                && (steps.value() - l).abs() > 0.5
            {
                steps.set_value(l);
            }
            up.set(false);
        }
    };
    sync();
    app.on_change(sync);
    root.upcast()
}

/// The piano roll with its toolbar and scrollbars.
fn build_roll(app: &Rc<App>) -> (PianoRoll, gtk::Widget, gtk::DropDown) {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let tools = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    tools.add_css_class("toolbar");
    let title = gtk::Label::new(Some("Piano roll"));
    title.add_css_class("heading");
    let chan = gtk::Label::new(None);
    chan.add_css_class("dim-label");
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    let snap_label = gtk::Label::new(Some("Snap"));
    snap_label.add_css_class("dim-label");
    let labels: Vec<&str> = SNAPS.iter().map(|s| s.0).collect();
    let snap = gtk::DropDown::from_strings(&labels);
    snap.set_tooltip_text(Some("Grid that notes snap to"));
    let zx_out = icon_button("zoom-out-symbolic", "Zoom out in time", None);
    let zx_in = icon_button("zoom-in-symbolic", "Zoom in in time", None);
    let zy_out = icon_button("go-down-symbolic", "Make rows smaller", None);
    let zy_in = icon_button("go-up-symbolic", "Make rows taller", None);
    let tl = gtk::Label::new(Some("Time"));
    tl.add_css_class("dim-label");
    let pl = gtk::Label::new(Some("Pitch"));
    pl.add_css_class("dim-label");
    for w in [
        title.upcast_ref::<gtk::Widget>(),
        chan.upcast_ref(),
        spacer.upcast_ref(),
        snap_label.upcast_ref(),
        snap.upcast_ref(),
        tl.upcast_ref(),
        zx_out.upcast_ref(),
        zx_in.upcast_ref(),
        pl.upcast_ref(),
        zy_out.upcast_ref(),
        zy_in.upcast_ref(),
    ] {
        tools.append(w);
    }
    root.append(&tools);
    root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

    let roll = PianoRoll::new(app.clone());
    let grid = gtk::Grid::new();
    let vs = gtk::Scrollbar::new(gtk::Orientation::Vertical, Some(&roll.vadj()));
    let hs = gtk::Scrollbar::new(gtk::Orientation::Horizontal, Some(&roll.hadj()));
    grid.attach(&roll, 0, 0, 1, 1);
    grid.attach(&vs, 1, 0, 1, 1);
    grid.attach(&hs, 0, 1, 1, 1);
    grid.set_vexpand(true);
    root.append(&grid);

    {
        let r = roll.clone();
        snap.connect_selected_notify(move |d| r.set_snap_index(d.selected() as usize));
        let r = roll.clone();
        zx_in.connect_clicked(move |_| r.zoom_x(1.3));
        let r = roll.clone();
        zx_out.connect_clicked(move |_| r.zoom_x(1.0 / 1.3));
        let r = roll.clone();
        zy_in.connect_clicked(move |_| r.zoom_y(1.2));
        let r = roll.clone();
        zy_out.connect_clicked(move |_| r.zoom_y(1.0 / 1.2));
    }
    let a = app.clone();
    let sync = move || {
        let s = a.session.borrow();
        let p = &s.document().project;
        let name = a
            .current_channel()
            .and_then(|c| p.channel(c))
            .map(|c| c.name.clone());
        chan.set_text(&name.map(|n| format!("— {n}")).unwrap_or_default());
    };
    sync();
    app.on_change(sync);
    (roll, root.upcast(), snap)
}

fn install_actions(gapp: &adw::Application, window: &adw::ApplicationWindow, app: &Rc<App>) {
    let add = |name: &str, f: Box<dyn Fn()>| {
        let a = gio::SimpleAction::new(name, None);
        a.connect_activate(move |_, _| f());
        window.add_action(&a);
    };
    {
        let a = app.clone();
        add("undo", Box::new(move || a.undo()));
        let a = app.clone();
        add("redo", Box::new(move || a.redo()));
        let a = app.clone();
        add("play", Box::new(move || a.toggle_play()));
        let a = app.clone();
        add("stop", Box::new(move || a.stop()));
        let (a, w) = (app.clone(), window.clone());
        add("save", Box::new(move || files::save(&w, &a)));
        let (a, w) = (app.clone(), window.clone());
        add("save-as", Box::new(move || files::save_as(&w, &a)));
        let (a, w) = (app.clone(), window.clone());
        add("open", Box::new(move || files::open(&w, &a)));
        let (a, _w) = (app.clone(), window.clone());
        add("new", Box::new(move || files::new_project(&a)));
        let (a, w) = (app.clone(), window.clone());
        add(
            "add-synth",
            Box::new(move || {
                let n = a.session.borrow().document().project.channels.len() + 1;
                let track = a.ui.borrow().track;
                let _ = &w;
                if let Some(r) = a.edit(vec![Edit::AddChannel {
                    name: format!("Synth {n}"),
                    instrument: NewInstrument::Synth {
                        params: SynthParams::default(),
                    },
                    root_key: 60,
                    track,
                }]) {
                    a.select_channel(protocol::ids::ChannelId(r.created[0]));
                }
            }),
        );
        let (a, w) = (app.clone(), window.clone());
        add(
            "add-instrument",
            Box::new(move || {
                let a2 = a.clone();
                dialogs::choose_plugin(
                    &w,
                    &a,
                    PluginKind::Instrument,
                    "Add instrument",
                    move |d| {
                        let track = a2.ui.borrow().track;
                        if let Some(r) = a2.edit(vec![Edit::AddChannel {
                            name: d.name.clone(),
                            instrument: NewInstrument::Clap {
                                plugin_id: d.id.clone(),
                            },
                            root_key: 60,
                            track,
                        }]) {
                            a2.select_channel(protocol::ids::ChannelId(r.created[0]));
                        }
                    },
                );
            }),
        );
        let (a, w) = (app.clone(), window.clone());
        add(
            "add-plugin",
            Box::new(move || {
                let a2 = a.clone();
                let w2 = w.clone();
                dialogs::choose_plugin(&w, &a, PluginKind::Any, "Add plugin", move |d| {
                    let track = a2.ui.borrow().track;
                    let _ = &w2;
                    if d.instrument {
                        if let Some(r) = a2.edit(vec![Edit::AddChannel {
                            name: d.name.clone(),
                            instrument: NewInstrument::Clap {
                                plugin_id: d.id.clone(),
                            },
                            root_key: 60,
                            track,
                        }]) {
                            a2.select_channel(protocol::ids::ChannelId(r.created[0]));
                        }
                    } else {
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
        let w = window.clone();
        add(
            "about",
            Box::new(move || {
                let d = adw::AboutDialog::builder()
                    .application_name("LibreDAW")
                    .developer_name("The LibreDAW contributors")
                    .license_type(gtk::License::Gpl30)
                    .version(env!("CARGO_PKG_VERSION"))
                    .comments("A free digital audio workstation for Linux.")
                    .build();
                d.present(Some(&w));
            }),
        );
        let w = window.clone();
        add("quit", Box::new(move || w.close()));
    }
    gapp.set_accels_for_action("win.undo", &["<Control>z"]);
    gapp.set_accels_for_action("win.redo", &["<Control><Shift>z", "<Control>y"]);
    gapp.set_accels_for_action("win.save", &["<Control>s"]);
    gapp.set_accels_for_action("win.save-as", &["<Control><Shift>s"]);
    gapp.set_accels_for_action("win.open", &["<Control>o"]);
    gapp.set_accels_for_action("win.new", &["<Control>n"]);
    gapp.set_accels_for_action("win.play", &["<Control>space"]);
    gapp.set_accels_for_action("win.quit", &["<Control>q"]);
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
        for d in report.done {
            if let Err(e) = d.result {
                a.toast(&e.to_string());
            }
        }
        if report.changed {
            a.notify();
        }
        a.tasks.poll();

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
