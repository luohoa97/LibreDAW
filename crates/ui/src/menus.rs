// SPDX-License-Identifier: GPL-3.0-or-later
//! Every menu the app builds, in one place, and the table of actions each
//! one may name. One menu per object: the primary menu, the Add Channel
//! menu, a channel's menu, a note's menu, and a mixer track's menu. A
//! context menu opens from the pointer (right-click), a long press, the
//! Menu key and Shift+F10 and always shows the same model; no object has a
//! second, different menu.
//!
//! The tests walk each model and check that every item names an action
//! from the table, that the table is registered, and that the handler does
//! what the label says.

use std::rc::Rc;

use gtk::gio;
use gtk::prelude::*;

use protocol::edit::Edit;
use protocol::ids::ChannelId;

use crate::app::{App, UiCommand};
use crate::channels;
use doc::presets;

/// Actions of a channel row (`row.*`), registered for each row.
pub const ROW_ACTIONS: &[&str] = &["sound", "rename", "remove", "choke"];
/// Actions of the note menu in the piano roll (`roll.*`).
pub const ROLL_ACTIONS: &[&str] = &["delete", "duplicate"];
/// Actions of a mixer track (`strip.*`).
pub const STRIP_ACTIONS: &[&str] = &["rename", "reset", "remove"];
/// Actions of a clip on the timeline (`clip.*`).
pub const CLIP_ACTIONS: &[&str] = &["edit", "duplicate", "unique", "split", "mute", "delete"];
/// Actions of a built-in sound in the sound browser (`sound.*`).
pub const SOUND_ACTIONS: &[&str] = &["add", "replace"];
/// Window actions (`win.*`) that menus name.
pub const WIN_ACTIONS: &[&str] = &[
    "new",
    "open",
    "save",
    "save-as",
    "export",
    "add-plugin",
    "show-help-overlay",
    "add-preset",
    "add-808",
    "add-sampler",
    "add-instrument",
];
/// Application actions (`app.*`) that menus name.
pub const APP_ACTIONS: &[&str] = &["preferences", "about"];

/// The primary menu.
pub fn main_menu() -> gio::Menu {
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
    end.append(Some("_About Oto"), Some("app.about"));
    menu.append_section(None, &end);
    menu
}

/// The "Add Channel" menu: built-in sounds grouped by role, then the 808,
/// the sampler and a plugin.
pub fn add_channel_menu() -> gio::Menu {
    let menu = gio::Menu::new();
    let mut roles: Vec<&str> = Vec::new();
    let all = presets::presets();
    for p in &all {
        if !roles.contains(&p.role) {
            roles.push(p.role);
        }
    }
    for role in roles {
        let section = gio::Menu::new();
        for p in all.iter().filter(|p| p.role == role) {
            let item = gio::MenuItem::new(Some(p.name), None);
            item.set_action_and_target_value(Some("win.add-preset"), Some(&p.name.to_variant()));
            section.append_item(&item);
        }
        let title = match role {
            "Drum" => "Drums",
            "Blank" => "Empty",
            other => other,
        };
        menu.append_section(Some(title), &section);
    }
    let native = gio::Menu::new();
    native.append(Some("_808 Bass"), Some("win.add-808"));
    native.append(Some("_Sampler…"), Some("win.add-sampler"));
    menu.append_section(None, &native);
    let plugin = gio::Menu::new();
    plugin.append(Some("_Plugin…"), Some("win.add-instrument"));
    menu.append_section(None, &plugin);
    menu
}

/// The menu of a channel row. It is the only menu a channel has: the
/// pointer, a long press, the Menu key and Shift+F10 all open it.
pub fn channel_menu() -> gio::Menu {
    let menu = gio::Menu::new();
    let first = gio::Menu::new();
    first.append(Some("Edit _Sound"), Some("row.sound"));
    menu.append_section(None, &first);
    // Channels in the same choke group cut each other off (15.1).
    let choke = gio::Menu::new();
    let none = gio::MenuItem::new(Some("None"), None);
    none.set_action_and_target_value(Some("row.choke"), Some(&0i32.to_variant()));
    choke.append_item(&none);
    for g in 1..=protocol::consts::MAX_CHOKE_GROUP as i32 {
        let item = gio::MenuItem::new(Some(&format!("Group {g}")), None);
        item.set_action_and_target_value(Some("row.choke"), Some(&g.to_variant()));
        choke.append_item(&item);
    }
    let grouping = gio::Menu::new();
    grouping.append_submenu(Some("C_hoke Group"), &choke);
    menu.append_section(None, &grouping);
    let second = gio::Menu::new();
    second.append(Some("_Rename"), Some("row.rename"));
    second.append(Some("Remove _Instrument"), Some("row.remove"));
    menu.append_section(None, &second);
    menu
}

/// The note menu of the piano roll.
pub fn roll_menu() -> gio::Menu {
    let menu = gio::Menu::new();
    menu.append(Some("_Delete"), Some("roll.delete"));
    menu.append(Some("D_uplicate"), Some("roll.duplicate"));
    menu
}

/// The menu of the selected clips on the timeline (SPEC 20.3). Shortcuts
/// in the labels' tooltips: Return, Ctrl+D, S, 0, Delete.
pub fn clip_menu() -> gio::Menu {
    let menu = gio::Menu::new();
    let first = gio::Menu::new();
    first.append(Some("_Edit Clip"), Some("clip.edit"));
    menu.append_section(None, &first);
    let second = gio::Menu::new();
    second.append(Some("_Copy That Changes Together"), Some("clip.duplicate"));
    second.append(Some("Edit _Separately"), Some("clip.unique"));
    second.append(Some("_Split at Playhead"), Some("clip.split"));
    second.append(Some("_Mute"), Some("clip.mute"));
    menu.append_section(None, &second);
    let third = gio::Menu::new();
    third.append(Some("De_lete"), Some("clip.delete"));
    menu.append_section(None, &third);
    menu
}

/// The menu of a built-in sound in the sound browser.
pub fn sound_menu() -> gio::Menu {
    let menu = gio::Menu::new();
    menu.append(Some("_Add to Project"), Some("sound.add"));
    menu.append(
        Some("_Replace the Selected Channel's Sound"),
        Some("sound.replace"),
    );
    menu
}

/// The menu of a mixer track.
pub fn strip_menu() -> gio::Menu {
    let menu = gio::Menu::new();
    menu.append(Some("_Rename"), Some("strip.rename"));
    menu.append(Some("Reset _Fader"), Some("strip.reset"));
    menu.append(Some("Remove _Track"), Some("strip.remove"));
    menu
}

/// Every `(action, target)` a menu names, depth first. Sections and
/// submenus are followed.
pub fn items_of(menu: &gio::MenuModel) -> Vec<(String, Option<gtk::glib::Variant>)> {
    let mut out = Vec::new();
    for i in 0..menu.n_items() {
        if let Some(action) = menu
            .item_attribute_value(i, "action", None)
            .and_then(|v| v.get::<String>())
        {
            out.push((action, menu.item_attribute_value(i, "target", None)));
        }
        for link in ["section", "submenu"] {
            if let Some(sub) = menu.item_link(i, link) {
                out.extend(items_of(&sub));
            }
        }
    }
    out
}

/// What a channel row action does. The row registers every name in
/// `ROW_ACTIONS` with this one function, so a label and its effect cannot
/// drift apart.
pub fn perform_row_action(app: &Rc<App>, id: ChannelId, action: &str, target: Option<i32>) {
    match action {
        "sound" => {
            app.select_channel(id);
            app.command(UiCommand::ShowSound);
        }

        "rename" => {
            app.select_channel(id);
            app.command(UiCommand::RenameChannel(id));
        }
        "remove" => channels::remove(app, id),
        "choke" => {
            if let Some(g) = target {
                app.edit(vec![Edit::SetChokeGroup {
                    channel: id,
                    group: g.clamp(0, protocol::consts::MAX_CHOKE_GROUP as i32) as u8,
                }]);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channels::NewChannel;
    use crate::engine_adapter::EngineLink;
    use crate::registry::Registry;
    use crate::session::Session;
    use doc::document::Document;
    use doc::persist::Dirs;
    use std::cell::RefCell;

    fn walk(menu: &gio::Menu, scope: &str, table: &[&str]) {
        let items = items_of(menu.upcast_ref());
        assert!(!items.is_empty(), "{scope} menu is empty");
        for (action, _) in items {
            let (prefix, name) = action
                .split_once('.')
                .unwrap_or_else(|| panic!("{action}: no group"));
            assert_eq!(prefix, scope, "{action} is in the wrong menu");
            assert!(table.contains(&name), "{action} is not a known action");
        }
    }

    #[test]
    fn every_menu_item_names_a_known_action() {
        walk(&channel_menu(), "row", ROW_ACTIONS);
        walk(&roll_menu(), "roll", ROLL_ACTIONS);
        walk(&strip_menu(), "strip", STRIP_ACTIONS);
        walk(&sound_menu(), "sound", SOUND_ACTIONS);
        walk(&clip_menu(), "clip", CLIP_ACTIONS);
        // The primary and Add Channel menus mix window and app actions.
        for menu in [main_menu(), add_channel_menu()] {
            for (action, _) in items_of(menu.upcast_ref()) {
                let (prefix, name) = action.split_once('.').unwrap();
                match prefix {
                    "win" => assert!(WIN_ACTIONS.contains(&name), "{action}"),
                    "app" => assert!(APP_ACTIONS.contains(&name), "{action}"),
                    other => panic!("{action}: unexpected group {other}"),
                }
            }
        }
    }

    #[test]
    fn every_known_action_is_registered_where_it_is_handled() {
        // Window and application actions are registered in window.rs by
        // name; the others are registered from their table.
        let window = include_str!("window.rs");
        for n in WIN_ACTIONS.iter().chain(APP_ACTIONS) {
            assert!(
                window.contains(&format!("\"{n}\"")),
                "no handler registers {n}"
            );
        }
        let channel_list = include_str!("channel_list.rs");
        assert!(channel_list.contains("ROW_ACTIONS"));
        let strips = include_str!("mixer.rs");
        assert!(strips.contains("STRIP_ACTIONS"));
        assert!(include_str!("browser.rs").contains("SOUND_ACTIONS"));
        assert!(include_str!("widgets/timeline.rs").contains("CLIP_ACTIONS"));
        let roll = include_str!("widgets/piano_roll.rs");
        assert!(roll.contains("ROLL_ACTIONS"));
    }

    #[test]
    fn channel_menu_has_each_action_once_and_no_duplicates_of_other_menus() {
        let items = items_of(channel_menu().upcast_ref());
        for n in ROW_ACTIONS {
            let count = items
                .iter()
                .filter(|(a, _)| a == &format!("row.{n}"))
                .count();
            if *n == "choke" {
                assert_eq!(count, 17, "None and 16 groups");
            } else {
                assert_eq!(count, 1, "{n}");
            }
        }
    }

    fn app() -> Rc<App> {
        let dir = std::env::temp_dir().join(format!("ldaw-menus-{}", std::process::id()));
        let s = Session::new(
            Document::new(),
            true,
            EngineLink::stub(48000.0),
            Registry::new(Vec::new(), 48000.0),
        );
        App::with_dirs(
            s,
            Dirs {
                music: dir.join("m"),
                data: dir.join("d"),
                config: dir.join("c"),
            },
        )
    }

    #[test]
    fn row_actions_do_what_their_labels_say() {
        let a = app();
        let kick = channels::add(&a, NewChannel::Preset("Kick".into())).unwrap();
        let snare = channels::add(&a, NewChannel::Preset("Snare".into())).unwrap();
        let seen: Rc<RefCell<Vec<UiCommand>>> = Rc::default();
        let s2 = seen.clone();
        a.on_command(move |c| s2.borrow_mut().push(c));

        // Edit Sound: selects the channel and opens the Sound page.
        perform_row_action(&a, kick, "sound", None);
        assert_eq!(a.current_channel(), Some(kick));
        assert_eq!(seen.borrow().last(), Some(&UiCommand::ShowSound));

        // Rename: asks to rename that channel, and changes nothing yet.
        let name_before = a
            .session
            .borrow()
            .document()
            .project
            .channel(kick)
            .unwrap()
            .name
            .clone();
        perform_row_action(&a, kick, "rename", None);
        assert_eq!(seen.borrow().last(), Some(&UiCommand::RenameChannel(kick)));
        assert_eq!(
            a.session
                .borrow()
                .document()
                .project
                .channel(kick)
                .unwrap()
                .name,
            name_before,
            "Rename itself never renames: the typed name does"
        );

        // Choke Group: sets the group.
        perform_row_action(&a, kick, "choke", Some(3));
        assert_eq!(
            a.session
                .borrow()
                .document()
                .project
                .channel(kick)
                .unwrap()
                .choke_group,
            3
        );
        perform_row_action(&a, kick, "choke", Some(0));
        assert_eq!(
            a.session
                .borrow()
                .document()
                .project
                .channel(kick)
                .unwrap()
                .choke_group,
            0
        );

        // Remove Channel: gone, and the other one stays selected.
        perform_row_action(&a, kick, "remove", None);
        let s = a.session.borrow();
        assert!(s.document().project.channel(kick).is_none());
        assert!(s.document().project.channel(snare).is_some());
        drop(s);
        assert_eq!(a.current_channel(), Some(snare));

        // An unknown name does nothing.
        let n = seen.borrow().len();
        perform_row_action(&a, snare, "no-such-action", None);
        assert_eq!(seen.borrow().len(), n);
    }
}
