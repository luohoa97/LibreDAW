// SPDX-License-Identifier: GPL-3.0-or-later
//! The left utility pane: the sound browser (docs/ui-design.md 3.8). It
//! lists the built-in sounds, then the kits of installed sound packs and the
//! folders the user added, with a search box and role filters.
//!
//! Activating a built-in row, or a piece of a kit, adds it as a new channel;
//! the button on a built-in row puts it on the selected channel instead. A
//! kit's "Add Kit" button adds all of its pieces. Pack sounds are copied
//! into the project (after the pack index hash is checked); sounds from a
//! folder the user added stay where they are (`local_only`, 17.2).

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;

use protocol::model::Instrument;

use crate::app::App;
use crate::channels::{self, NewChannel};
use crate::samples_ui;
use crate::sound_picker;
use crate::soundlib::{self, Kit, Piece, Source};
use doc::presets::{self, Preset};
use plugin_host::sounds::{self, Sound};

/// The label of a role in the filter row and the subtitle.
pub fn role_label(role: &str) -> &str {
    match role {
        "Drum" => "Drums",
        other => sound_picker::role_text(other).0,
    }
}

/// Whether a sound passes the search text and the role filter. The search
/// is a case-insensitive substring over name and role.
pub fn matches(p: &Preset, search: &str, role: Option<&str>) -> bool {
    if let Some(r) = role
        && p.role != r
    {
        return false;
    }
    let q = search.trim().to_lowercase();
    if q.is_empty() {
        return true;
    }
    [p.name, role_label(p.role)]
        .iter()
        .any(|f| f.to_lowercase().contains(&q))
}

/// The same test for a piece of a kit: name, role tag, kit, and "pack" or
/// "your folder" are searchable; the filter compares the role group.
pub fn matches_piece(kit: &Kit, piece: &Piece, search: &str, role: Option<&str>) -> bool {
    if let Some(r) = role
        && soundlib::role_group(&piece.role) != r
    {
        return false;
    }
    let q = search.trim().to_lowercase();
    if q.is_empty() {
        return true;
    }
    let source = match kit.source {
        Source::Pack => "pack",
        Source::UserFolder => "your folder",
    };
    [
        piece.name.as_str(),
        &soundlib::role_title(&piece.role),
        &kit.title,
        source,
    ]
    .iter()
    .any(|f| f.to_lowercase().contains(&q))
}

/// The same test for a factory sound of a plugin: name, role and plugin name.
pub fn matches_sound(s: &Sound, search: &str, role: Option<&str>) -> bool {
    if let Some(r) = role
        && s.role != r
    {
        return false;
    }
    let q = search.trim().to_lowercase();
    if q.is_empty() {
        return true;
    }
    [
        s.label(),
        role_label(&s.role).to_string(),
        sound_picker::plugin_name(s),
    ]
    .iter()
    .any(|f| f.to_lowercase().contains(&q))
}

/// The roles that have a sound, in list order.
pub fn roles(all: &[Preset]) -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::new();
    for p in all {
        if !out.contains(&p.role) {
            out.push(p.role);
        }
    }
    for r in sounds::roles() {
        if !out.contains(&r) {
            out.push(r);
        }
    }
    out
}

struct State {
    search: String,
    role: Option<&'static str>,
}

/// What `apply` toggles: a plain row, or a kit with its piece rows.
enum Entry {
    Preset {
        row: adw::ActionRow,
        index: usize,
    },
    Sound {
        row: adw::ActionRow,
        index: usize,
    },
    Kit {
        expander: adw::ExpanderRow,
        kit: Kit,
        rows: Vec<adw::ActionRow>,
    },
}

type Entries = Rc<RefCell<Vec<Entry>>>;

pub fn build(app: &Rc<App>) -> gtk::Widget {
    let all = drum_presets();
    let state = Rc::new(RefCell::new(State {
        search: String::new(),
        role: None,
    }));
    let entries: Entries = Rc::new(RefCell::new(Vec::new()));

    let search = gtk::SearchEntry::new();
    search.set_placeholder_text(Some("Search sounds"));
    search.update_property(&[gtk::accessible::Property::Label("Search sounds")]);
    search.set_hexpand(true);
    search.set_widget_name("sound-search");
    let search_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    search_box.add_css_class("toolbar");
    search_box.append(&search);

    // The role filter is one dropdown beside the search (chips would clip
    // at any width; libadwaita 1.5 has no wrap box).
    let role_list: Vec<Option<&'static str>> = std::iter::once(None)
        .chain(roles(&all).into_iter().map(Some))
        .collect();
    let labels: Vec<String> = role_list
        .iter()
        .map(|r| match r {
            None => "All Sounds".to_string(),
            Some(r) => role_label(r).to_string(),
        })
        .collect();
    let label_refs: Vec<&str> = labels.iter().map(String::as_str).collect();
    let role_drop = gtk::DropDown::from_strings(&label_refs);
    role_drop.set_tooltip_text(Some("Show One Kind of Sound"));
    role_drop.update_property(&[gtk::accessible::Property::Label("Kind of sound")]);
    search_box.append(&role_drop);

    let list = gtk::ListBox::new();
    list.add_css_class("boxed-list");
    list.set_selection_mode(gtk::SelectionMode::None);
    list.set_margin_start(12);
    list.set_margin_end(12);
    list.set_margin_top(6);
    list.set_margin_bottom(12);

    // Empty results.
    let none = adw::StatusPage::new();
    none.set_icon_name(Some("edit-find-symbolic"));
    none.set_title("No Sounds Found");
    none.set_description(Some("Try another word or clear the filters."));
    none.add_css_class("compact");
    let clear = gtk::Button::with_label("Clear Filters");
    clear.add_css_class("pill");
    clear.set_halign(gtk::Align::Center);
    none.set_child(Some(&clear));

    let scroller = gtk::ScrolledWindow::builder()
        .child(&list)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .build();
    let stack = gtk::Stack::new();
    stack.add_named(&scroller, Some("results"));
    stack.add_named(&none, Some("empty"));

    let apply: Rc<dyn Fn()> = Rc::new({
        let (state, entries, stack) = (state.clone(), entries.clone(), stack.clone());
        move || {
            let s = state.borrow();
            let searching = !s.search.trim().is_empty();
            let all = drum_presets();
            let mut shown = 0;
            for e in entries.borrow().iter() {
                match e {
                    Entry::Preset { row, index } => {
                        let ok = all
                            .get(*index)
                            .map(|p| matches(p, &s.search, s.role))
                            .unwrap_or(false);
                        row.set_visible(ok);
                        shown += ok as usize;
                    }
                    Entry::Sound { row, index } => {
                        let ok = sounds::sounds()
                            .get(*index)
                            .is_some_and(|x| matches_sound(x, &s.search, s.role));
                        row.set_visible(ok);
                        shown += ok as usize;
                    }
                    Entry::Kit {
                        expander,
                        kit,
                        rows,
                    } => {
                        let mut any = false;
                        for (row, piece) in rows.iter().zip(&kit.pieces) {
                            let ok = matches_piece(kit, piece, &s.search, s.role);
                            row.set_visible(ok);
                            any |= ok;
                        }
                        expander.set_visible(any);
                        if searching && any {
                            expander.set_expanded(true);
                        }
                        shown += any as usize;
                    }
                }
            }
            stack.set_visible_child_name(if shown == 0 { "empty" } else { "results" });
        }
    });

    // Builds the rows: built-in sounds, then the kits.
    let rebuild: Rc<dyn Fn()> = Rc::new({
        let (app, list, entries, apply) =
            (app.clone(), list.clone(), entries.clone(), apply.clone());
        move || {
            while let Some(c) = list.first_child() {
                list.remove(&c);
            }
            let mut es = Vec::new();
            if !sound_picker::installed(&app) {
                list.append(&install_row());
            }
            for (i, p) in drum_presets().iter().enumerate() {
                let row = preset_row(&app, p);
                list.append(&row);
                es.push(Entry::Preset { row, index: i });
            }
            let have = sound_picker::installed(&app);
            for (i, s) in sounds::sounds().iter().enumerate() {
                let row = sound_row(&app, s, have);
                list.append(&row);
                es.push(Entry::Sound { row, index: i });
            }
            for kit in samples_ui::library(&app) {
                let (expander, rows) = kit_rows(&app, &kit);
                list.append(&expander);
                es.push(Entry::Kit {
                    expander,
                    kit,
                    rows,
                });
            }
            // Advanced: any other instrument on this computer.
            list.append(&more_row());
            *entries.borrow_mut() = es;
            apply();
        }
    });
    rebuild();
    sound_picker::set_refresh(rebuild.clone());

    {
        let (st, ap) = (state.clone(), apply.clone());
        search.connect_search_changed(move |e| {
            st.borrow_mut().search = e.text().to_string();
            ap();
        });
        {
            let (state, apply) = (state.clone(), apply.clone());
            role_drop.connect_selected_notify(move |d| {
                state.borrow_mut().role = role_list.get(d.selected() as usize).copied().flatten();
                apply();
            });
        }
        let (search, role_drop, apply, state) = (
            search.clone(),
            role_drop.clone(),
            apply.clone(),
            state.clone(),
        );
        clear.connect_clicked(move |_| {
            search.set_text("");
            role_drop.set_selected(0);
            let mut s = state.borrow_mut();
            s.search.clear();
            s.role = None;
            drop(s);
            apply();
        });
    }

    // Bottom bar: Add Sound Folder...
    let add_folder = gtk::Button::new();
    add_folder.set_child(Some(
        &adw::ButtonContent::builder()
            .icon_name("folder-new-symbolic")
            .label("Add Sound Folder…")
            .build(),
    ));
    add_folder.add_css_class("flat");
    add_folder.set_tooltip_text(Some("Add a Folder of Your Own WAV Sounds"));
    {
        let (app, rebuild) = (app.clone(), rebuild.clone());
        add_folder.connect_clicked(move |b| add_sound_folder(b, &app, rebuild.clone()));
    }
    let bottom = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    bottom.add_css_class("toolbar");
    bottom.append(&add_folder);

    let view = adw::ToolbarView::new();
    view.add_top_bar(&search_box);
    view.set_content(Some(&stack));
    view.add_bottom_bar(&bottom);
    view.upcast()
}

/// The drums made by Oto itself. The built-in synth and 808 are not offered any
/// more (Amendment 23); projects that hold them still play.
fn drum_presets() -> Vec<Preset> {
    presets::presets()
        .into_iter()
        .filter(|p| p.role == "Drum")
        .collect()
}

/// Focuses the search box of the Sounds pane under `root`.
pub fn focus_search(root: &gtk::Widget) {
    fn find(w: &gtk::Widget) -> Option<gtk::Widget> {
        if w.widget_name() == "sound-search" {
            return Some(w.clone());
        }
        let mut c = w.first_child();
        while let Some(child) = c {
            if let Some(f) = find(&child) {
                return Some(f);
            }
            c = child.next_sibling();
        }
        None
    }
    if let Some(s) = find(root) {
        s.grab_focus();
    }
}

/// Shown while the sounds are not installed.
fn install_row() -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title("Install Sounds…")
        .subtitle("Get free sounds for bass, keys, pads and more")
        .activatable(true)
        .build();
    row.add_prefix(&gtk::Image::from_icon_name("folder-download-symbolic"));
    row.set_tooltip_text(Some("Opens the software store to get the free sounds"));
    row.connect_activated(|r| sound_picker::open_store(r.upcast_ref()));
    row
}

/// A factory sound of a plugin. Greyed out until the plugin is installed.
fn sound_row(app: &Rc<App>, s: &Sound, have: bool) -> adw::ActionRow {
    let label = sound_picker::sound_label(s);
    let row = adw::ActionRow::new();
    row.set_title(&gtk::glib::markup_escape_text(&label));
    row.set_subtitle(&format!(
        "{} · {}",
        role_label(&s.role),
        sound_picker::plugin_name(s)
    ));
    let tip = sound_picker::role_text(&s.role).1;
    row.set_tooltip_text(Some(tip));
    row.set_sensitive(have);
    row.set_activatable(have);
    let add_btn = gtk::Button::from_icon_name("list-add-symbolic");
    add_btn.add_css_class("flat");
    add_btn.set_valign(gtk::Align::Center);
    add_btn.set_tooltip_text(Some("Add to Project"));
    add_btn.update_property(&[gtk::accessible::Property::Label(&format!(
        "Add {label} to the project"
    ))]);
    row.add_suffix(&add_btn);
    let (a, s2) = (app.clone(), s.clone());
    add_btn.connect_clicked(move |_| sound_picker::add_sound(&a, &s2));
    let (a, s2) = (app.clone(), s.clone());
    row.connect_activated(move |_| sound_picker::add_sound(&a, &s2));
    row
}

/// The last row: the full list of instruments on this computer.
fn more_row() -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title("More Instruments…")
        .subtitle("Choose from other instruments installed on this computer")
        .activatable(true)
        .build();
    row.set_tooltip_text(Some("Shows every instrument installed on this computer"));
    row.connect_activated(|r| {
        let _ = r.activate_action("win.add-instrument", None);
    });
    row
}

fn preset_row(app: &Rc<App>, p: &Preset) -> adw::ActionRow {
    let row = adw::ActionRow::new();
    row.set_title(p.name);
    row.set_subtitle(&format!("{} · Oto Kit", role_label(p.role)));
    row.set_activatable(true);
    row.set_tooltip_text(Some("Add to Project"));
    let add_btn = gtk::Button::from_icon_name("list-add-symbolic");
    add_btn.add_css_class("flat");
    add_btn.set_valign(gtk::Align::Center);
    add_btn.set_tooltip_text(Some("Add to Project"));
    add_btn.update_property(&[gtk::accessible::Property::Label(&format!(
        "Add {} to the project",
        p.name
    ))]);
    row.add_suffix(&add_btn);
    // The row's menu (menus::sound_menu): add, or replace the sound of the
    // selected channel.
    let group = gtk::gio::SimpleActionGroup::new();
    for n in crate::menus::SOUND_ACTIONS {
        let act = gtk::gio::SimpleAction::new(n, None);
        let (a, name, params, n) = (app.clone(), p.name.to_string(), p.params, *n);
        act.connect_activate(move |_, _| match n {
            "add" => add_preset(&a, &name),
            "replace" => replace_preset(&a, &params),
            _ => {}
        });
        group.add_action(&act);
    }
    row.insert_action_group("sound", Some(&group));
    add_btn.set_action_name(Some("sound.add"));
    let a = app.clone();
    let name = p.name.to_string();
    row.connect_activated(move |_| add_preset(&a, &name));
    crate::context_menu::attach(&row, &crate::menus::sound_menu(), |_| true);
    row
}

/// A built-in sound as a new channel, with Undo in the toast.
fn add_preset(a: &Rc<App>, name: &str) {
    if channels::add(a, NewChannel::Preset(name.to_string())).is_some() {
        let a2 = a.clone();
        a.toast_action(&format!("Added {name}"), "Undo", move || a2.undo());
    }
}

/// Gives the selected channel this built-in sound (built-in synth
/// channels only).
fn replace_preset(a: &Rc<App>, params: &protocol::model::SynthParams) {
    let ch = a.current_channel();
    let is_synth = ch
        .and_then(|c| {
            a.session
                .borrow()
                .document()
                .project
                .channel(c)
                .map(|c| matches!(c.instrument, Instrument::Synth(_)))
        })
        .unwrap_or(false);
    match ch {
        Some(c) if is_synth => {
            a.edit(presets::apply_edits(c, params));
        }
        _ => a.toast("Choose an instrument with a built-in sound first"),
    }
}

/// The expander row of a kit and its piece rows.
fn kit_rows(app: &Rc<App>, kit: &Kit) -> (adw::ExpanderRow, Vec<adw::ActionRow>) {
    let expander = adw::ExpanderRow::new();
    expander.set_title(&kit.title);
    let kind = match kit.source {
        Source::Pack => "Sound pack",
        Source::UserFolder => "Your folder",
    };
    expander.set_subtitle(&format!("{} - {} sounds", kind, kit.pieces.len()));
    if kit.source == Source::UserFolder {
        let icon = gtk::Image::from_icon_name("computer-symbolic");
        icon.set_tooltip_text(Some("On this computer only (not saved in shared projects)"));
        expander.add_prefix(&icon);
    }
    let add_all = gtk::Button::with_label("Add Kit");
    add_all.add_css_class("flat");
    add_all.set_valign(gtk::Align::Center);
    add_all.set_tooltip_text(Some("Add every sound of this kit as a new instrument"));
    add_all.update_property(&[gtk::accessible::Property::Label(&format!(
        "Add all sounds of {}",
        kit.title
    ))]);
    {
        let (a, k) = (app.clone(), kit.clone());
        add_all.connect_clicked(move |_| samples_ui::add_kit(&a, &k));
    }
    expander.add_suffix(&add_all);
    let mut rows = Vec::new();
    for piece in &kit.pieces {
        let row = adw::ActionRow::new();
        row.set_title(&piece.name);
        row.set_subtitle(&format!(
            "{} - {}",
            soundlib::role_title(&piece.role),
            kit.title
        ));
        row.set_activatable(true);
        row.set_tooltip_text(Some("Add this sound as a new instrument"));
        let (a, k, p) = (app.clone(), kit.clone(), piece.clone());
        row.connect_activated(move |_| samples_ui::add_piece(&a, &k, &p));
        expander.add_row(&row);
        rows.push(row);
    }
    (expander, rows)
}

/// Asks for a folder, explains what adding it means, then lists it.
fn add_sound_folder(parent: &gtk::Button, app: &Rc<App>, rebuild: Rc<dyn Fn()>) {
    let dialog = gtk::FileDialog::builder().title("Add Sound Folder").build();
    let win = parent.root().and_then(|r| r.downcast::<gtk::Window>().ok());
    let (app, parent) = (app.clone(), parent.clone());
    dialog.select_folder(win.as_ref(), gtk::gio::Cancellable::NONE, move |res| {
        let Ok(f) = res else { return };
        let Some(dir) = f.path() else { return };
        if soundlib::scan_folder(&dir).is_none() {
            app.toast("That folder has no WAV sounds");
            return;
        }
        let alert = adw::AlertDialog::new(
            Some("Use These Sounds on This Computer?"),
            Some(
                "Oto only reads the files in this folder. They stay where they are and \
                 are not copied into your projects or shared. Whether you may use them is up \
                 to the license they came with.",
            ),
        );
        alert.add_responses(&[("cancel", "_Cancel"), ("add", "_Add Folder")]);
        alert.set_response_appearance("add", adw::ResponseAppearance::Suggested);
        alert.set_default_response(Some("add"));
        alert.set_close_response("cancel");
        let (app2, rebuild) = (app.clone(), rebuild.clone());
        alert.connect_response(None, move |_, r| {
            if r == "add" {
                remember_folder(&app2, dir.clone());
                rebuild();
            }
        });
        alert.present(Some(&parent));
    });
}

fn remember_folder(app: &App, dir: PathBuf) {
    let mut folders = samples_ui::load_folders(app);
    if !folders.contains(&dir) {
        folders.push(dir);
    }
    if let Err(e) = samples_ui::save_folders(app, &folders) {
        app.toast(&format!("Could not remember the folder: {e}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::soundlib::parse_kit;
    use std::collections::HashMap;
    use std::path::Path;

    #[test]
    fn search_and_role_filter() {
        let all = presets::presets();
        let kick = all.iter().find(|p| p.name == "Kick").unwrap();
        assert!(matches(kick, "", None));
        assert!(matches(kick, "kic", None));
        assert!(matches(kick, "  KICK ", None));
        assert!(matches(kick, "drums", None), "role is searchable");
        assert!(!matches(kick, "bass", None));
        assert!(matches(kick, "", Some("Drum")));
        assert!(!matches(kick, "", Some("Bass")));
        assert!(!matches(kick, "kick", Some("Bass")));
    }

    #[test]
    fn roles_are_listed_once_in_order() {
        let r = roles(&drum_presets());
        assert_eq!(r.first(), Some(&"Drum"));
        let mut d = r.clone();
        d.sort();
        d.dedup();
        assert_eq!(d.len(), r.len());
        assert_eq!(role_label("Drum"), "Drums");
        assert_eq!(role_label("Bass"), "Bass");
    }

    #[test]
    fn some_sound_matches_every_role_filter() {
        let all = drum_presets();
        for role in roles(&all) {
            assert!(
                all.iter().any(|p| matches(p, "", Some(role)))
                    || sounds::sounds()
                        .iter()
                        .any(|s| matches_sound(s, "", Some(role))),
                "{role}"
            );
        }
    }

    #[test]
    fn kit_pieces_filter_by_group_and_search_by_name_role_and_kit() {
        let text = "name = \"phonk\"\n[[piece]]\nfile = \"kick_deep.wav\"\nrole = \"kick\"\n[[piece]]\nfile = \"808_sub.wav\"\nrole = \"808\"\n";
        let kit = parse_kit(text, Path::new("/x/phonk"), Source::Pack, &HashMap::new()).unwrap();
        let (kick, bass) = (&kit.pieces[0], &kit.pieces[1]);
        assert!(matches_piece(&kit, kick, "", Some("Drum")));
        assert!(!matches_piece(&kit, kick, "", Some("808")));
        assert!(matches_piece(&kit, bass, "", Some("808")));
        assert!(matches_piece(&kit, kick, "phonk", None), "kit name");
        assert!(matches_piece(&kit, kick, "KICK", None), "role tag");
        assert!(matches_piece(&kit, kick, "deep", None), "name");
        assert!(matches_piece(&kit, kick, "pack", None));
        assert!(!matches_piece(&kit, kick, "snare", None));
    }
}
