// SPDX-License-Identifier: GPL-3.0-or-later
//! The Home page (SPEC 19.1): what Oto shows at launch and after closing a
//! project. Unsaved work first, then recent projects, newest first. The
//! list itself is `home_logic`; this file draws it and acts on clicks.
//! Nothing here deletes: Discard and Move to Trash use the desktop trash.

use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, UNIX_EPOCH};

use adw::prelude::*;
use gtk::gio;

use crate::app::App;
use crate::files;
use crate::home_logic::{self, HomeItem, Kind};

/// A search box appears above the recent projects past this many.
const SEARCH_ABOVE: usize = 8;

pub struct Home {
    pub widget: adw::ToolbarView,
    pub toasts: adw::ToastOverlay,
    /// Reads the folders again and redraws.
    pub refresh: Rc<dyn Fn()>,
}

pub fn build(app: &Rc<App>) -> Home {
    let content = gtk::Box::new(gtk::Orientation::Vertical, 24);
    content.set_margin_top(24);
    content.set_margin_bottom(24);
    content.set_margin_start(12);
    content.set_margin_end(12);
    let clamp = adw::Clamp::builder()
        .maximum_size(640)
        .child(&content)
        .build();
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&clamp)
        .build();

    let empty = adw::StatusPage::builder()
        .icon_name("audio-x-generic-symbolic")
        .title("Make Your First Beat")
        .description("Start a project, click the grid, and press Space to hear it.")
        .vexpand(true)
        .build();
    let empty_box = gtk::Box::new(gtk::Orientation::Vertical, 12);
    empty_box.set_halign(gtk::Align::Center);
    empty_box.append(&new_button(app));
    empty_box.append(&open_button(app));
    empty.set_child(Some(&empty_box));

    let pages = gtk::Stack::new();
    pages.add_named(&scroll, Some("list"));
    pages.add_named(&empty, Some("empty"));
    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&pages));

    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&adw::WindowTitle::new("Oto", "")));
    let menu = gtk::MenuButton::new();
    menu.set_icon_name("open-menu-symbolic");
    menu.set_menu_model(Some(&crate::menus::main_menu()));
    menu.set_primary(true);
    menu.add_css_class("flat");
    menu.set_tooltip_text(Some("Main Menu"));
    menu.update_property(&[gtk::accessible::Property::Label("Main menu")]);
    header.pack_end(&menu);

    let widget = adw::ToolbarView::new();
    widget.add_top_bar(&header);
    widget.set_content(Some(&toasts));

    let refresh: Rc<std::cell::RefCell<Option<Rc<dyn Fn()>>>> = Rc::default();
    let (a, c, p, r) = (app.clone(), content.clone(), pages.clone(), refresh.clone());
    let redraw: Rc<dyn Fn()> = Rc::new(move || {
        let again: Rc<dyn Fn()> = r.borrow().clone().expect("set below");
        draw(&a, &c, &p, again);
    });
    *refresh.borrow_mut() = Some(redraw.clone());
    Home {
        widget,
        toasts,
        refresh: redraw,
    }
}

fn new_button(app: &Rc<App>) -> gtk::Button {
    let b = gtk::Button::with_label("New Project");
    b.add_css_class("suggested-action");
    b.add_css_class("pill");
    let a = app.clone();
    b.connect_clicked(move |_| files::new_project(&a));
    b
}

fn open_button(app: &Rc<App>) -> gtk::Button {
    let b = gtk::Button::with_label("Open Other…");
    b.add_css_class("pill");
    let a = app.clone();
    b.connect_clicked(move |b| files::open(b, &a));
    b
}

fn draw(app: &Rc<App>, content: &gtk::Box, pages: &gtk::Stack, refresh: Rc<dyn Fn()>) {
    while let Some(c) = content.first_child() {
        content.remove(&c);
    }
    let mut exclude = vec![app.dirs.recovery_bundle(&app.session_id)];
    exclude.extend(app.stale_recovery.borrow().clone());
    let list = home_logic::scan(&app.dirs, &exclude);
    if list.is_empty() {
        pages.set_visible_child_name("empty");
        return;
    }
    pages.set_visible_child_name("list");
    let now = home_logic::now_secs();

    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    actions.set_halign(gtk::Align::Center);
    actions.append(&new_button(app));
    actions.append(&open_button(app));
    content.append(&actions);

    if !list.unsaved.is_empty() {
        content.append(&section("Unsaved Work"));
        let rows = boxed_list();
        for item in &list.unsaved {
            rows.append(&unsaved_row(app, item, now, refresh.clone()));
        }
        content.append(&rows);
    }

    if !list.recent.is_empty() {
        content.append(&section("Recent Projects"));
        let rows = boxed_list();
        if list.recent.len() > SEARCH_ABOVE {
            let search = gtk::SearchEntry::new();
            search.set_placeholder_text(Some("Search projects"));
            content.append(&search);
            let s = search.clone();
            rows.set_filter_func(move |row| {
                let q = s.text().to_lowercase();
                q.is_empty()
                    || row
                        .downcast_ref::<adw::PreferencesRow>()
                        .is_some_and(|r| r.title().to_lowercase().contains(&q))
            });
            let r = rows.clone();
            search.connect_search_changed(move |_| r.invalidate_filter());
        }
        for item in &list.recent {
            rows.append(&recent_row(app, item, now, refresh.clone()));
        }
        content.append(&rows);
    }
}

fn section(title: &str) -> gtk::Label {
    let l = gtk::Label::new(Some(title));
    l.add_css_class("heading");
    l.set_halign(gtk::Align::Start);
    l.set_accessible_role(gtk::AccessibleRole::Heading);
    l
}

fn boxed_list() -> gtk::ListBox {
    let l = gtk::ListBox::new();
    l.add_css_class("boxed-list");
    l.set_selection_mode(gtk::SelectionMode::None);
    l
}

fn row(title: &str, subtitle: &str) -> adw::ActionRow {
    let r = adw::ActionRow::new();
    r.set_use_markup(false);
    r.set_title(title);
    r.set_subtitle(subtitle);
    r
}

fn modified_at(item: &HomeItem) -> std::time::SystemTime {
    UNIX_EPOCH + Duration::from_secs(item.modified)
}

/// Opens a saved project, saving the one that is open first.
fn open_item(app: &Rc<App>, path: PathBuf) {
    let a = app.clone();
    files::save_then(app, move || files::open_path(&a, path));
}

fn unsaved_row(app: &Rc<App>, item: &HomeItem, now: u64, refresh: Rc<dyn Fn()>) -> adw::ActionRow {
    let age = home_logic::age_text(now, item.modified);
    let sub = match item.kind {
        Kind::Autosave => format!("Unsaved changes · {age}"),
        _ => age,
    };
    let r = row(&item.name, &sub);

    let restore = gtk::Button::with_label("Restore");
    restore.add_css_class("suggested-action");
    restore.set_valign(gtk::Align::Center);
    let (a, it) = (app.clone(), item.clone());
    restore.connect_clicked(move |_| {
        let (a, it) = (a.clone(), it.clone());
        let a2 = a.clone();
        files::save_then(&a2, move || match it.kind {
            Kind::Recovery => files::open_recovery_bundle(&a, it.path.clone(), modified_at(&it)),
            _ => files::open_path(&a, it.path.clone()),
        });
    });

    let discard = gtk::Button::with_label("Discard");
    discard.add_css_class("flat");
    discard.set_valign(gtk::Align::Center);
    let (a, it) = (app.clone(), item.clone());
    discard.connect_clicked(move |b| {
        move_to_trash(&a, b, &it, &refresh);
    });
    r.add_suffix(&discard);
    r.add_suffix(&restore);
    r
}

fn recent_row(app: &Rc<App>, item: &HomeItem, now: u64, refresh: Rc<dyn Fn()>) -> adw::ActionRow {
    let r = row(&item.name, &home_logic::age_text(now, item.modified));
    r.set_activatable(true);
    let (a, p) = (app.clone(), item.path.clone());
    r.connect_activated(move |_| open_item(&a, p.clone()));

    // The same actions from the right-click menu and the "more" button.
    let menu = gio::Menu::new();
    menu.append(Some("Open"), Some("item.open"));
    menu.append(Some("Show in Files"), Some("item.show"));
    menu.append(Some("Rename…"), Some("item.rename"));
    menu.append(Some("Move to Trash"), Some("item.trash"));
    let more = gtk::MenuButton::new();
    more.set_icon_name("view-more-symbolic");
    more.set_menu_model(Some(&menu));
    more.set_valign(gtk::Align::Center);
    more.add_css_class("flat");
    more.set_tooltip_text(Some("Project Actions"));
    more.update_property(&[gtk::accessible::Property::Label("Project actions")]);
    r.add_suffix(&more);
    r.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));

    let group = gio::SimpleActionGroup::new();
    let add = |name: &str, f: Box<dyn Fn()>| {
        let act = gio::SimpleAction::new(name, None);
        act.connect_activate(move |_, _| f());
        group.add_action(&act);
    };
    let (a, p) = (app.clone(), item.path.clone());
    add("open", Box::new(move || open_item(&a, p.clone())));
    let (p, w) = (item.path.clone(), r.clone());
    add(
        "show",
        Box::new(move || {
            let launcher = gtk::FileLauncher::new(Some(&gio::File::for_path(&p)));
            let win = w.root().and_downcast::<gtk::Window>();
            launcher.open_containing_folder(win.as_ref(), gio::Cancellable::NONE, |_| {});
        }),
    );
    let (a, it, w, rf) = (app.clone(), item.clone(), r.clone(), refresh.clone());
    add("rename", Box::new(move || rename(&a, &w, &it, rf.clone())));
    let (a, it, w, rf) = (app.clone(), item.clone(), r.clone(), refresh);
    add("trash", Box::new(move || move_to_trash(&a, &w, &it, &rf)));
    r.insert_action_group("item", Some(&group));

    let click = gtk::GestureClick::builder().button(3).build();
    let m = more.clone();
    click.connect_pressed(move |g, _, _, _| {
        g.set_state(gtk::EventSequenceState::Claimed);
        m.popup();
    });
    r.add_controller(click);
    r
}

fn move_to_trash(
    app: &Rc<App>,
    from: &impl IsA<gtk::Widget>,
    item: &HomeItem,
    refresh: &Rc<dyn Fn()>,
) {
    let target = home_logic::trash_target(item);
    match gio::File::for_path(&target).trash(gio::Cancellable::NONE) {
        Ok(()) => {
            // The open project's folder is gone; it is no longer saved there.
            if item.kind == Kind::Saved && app.ui.borrow().path.as_ref() == Some(&item.path) {
                app.ui.borrow_mut().path = None;
                files::point_samples_at(app, None);
            }
            app.toast("Moved to the trash");
            refresh();
        }
        Err(e) => {
            let _ = from;
            app.toast(&format!("Could not move it to the trash: {e}"));
        }
    }
}

fn rename(app: &Rc<App>, from: &impl IsA<gtk::Widget>, item: &HomeItem, refresh: Rc<dyn Fn()>) {
    let (a, it) = (app.clone(), item.clone());
    crate::dialogs::ask_name(from, "Rename Project", &item.name, move |name| {
        let target = match home_logic::rename_target(&it.path, &name) {
            Ok(t) => t,
            Err(e) => return a.toast(&e),
        };
        if target == it.path {
            return;
        }
        if let Err(e) = std::fs::rename(&it.path, &target) {
            return a.toast(&format!("Could not rename the project: {e}"));
        }
        if a.ui.borrow().path.as_ref() == Some(&it.path) {
            a.ui.borrow_mut().path = Some(target.clone());
            files::point_samples_at(&a, Some(&target));
        }
        let last = doc::persist::LastSession::read(&a.dirs);
        if last.path.as_ref() == Some(&it.path) {
            let _ = doc::persist::LastSession {
                path: Some(target),
                note: last.note,
            }
            .write(&a.dirs);
        }
        refresh();
    });
}
