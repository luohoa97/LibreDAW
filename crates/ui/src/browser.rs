// SPDX-License-Identifier: GPL-3.0-or-later
//! The left utility pane: the sound browser (docs/ui-design.md 3.8). It
//! lists the built-in sounds with a search box and role filters. Activating
//! a row adds the sound as a new channel; the button on the row puts it on
//! the selected channel instead. Auditioning without changing the project
//! arrives with the preview slot (SPEC 17.2).

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;

use protocol::model::Instrument;

use crate::app::App;
use crate::channels::{self, NewChannel};
use doc::presets::{self, Preset};

/// The label of a role in the filter row and the subtitle.
pub fn role_label(role: &str) -> &str {
    match role {
        "Drum" => "Drums",
        "Blank" => "Empty",
        other => other,
    }
}

/// Whether a sound passes the search text and the role filter. The search
/// is a case-insensitive substring over name, role, and "Built-in".
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
    [p.name, role_label(p.role), "built-in"]
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
    out
}

struct State {
    search: String,
    role: Option<&'static str>,
}

pub fn build(app: &Rc<App>) -> gtk::Widget {
    let all = presets::presets();
    let state = Rc::new(RefCell::new(State {
        search: String::new(),
        role: None,
    }));

    let search = gtk::SearchEntry::new();
    search.set_placeholder_text(Some("Search sounds"));
    search.update_property(&[gtk::accessible::Property::Label("Search sounds")]);
    search.set_hexpand(true);
    let search_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    search_box.add_css_class("toolbar");
    search_box.append(&search);

    // Role filters act as radio buttons, "All" first.
    let filters = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    filters.add_css_class("toolbar");
    let all_btn = gtk::ToggleButton::with_label("All");
    all_btn.set_active(true);
    all_btn.add_css_class("flat");
    filters.append(&all_btn);
    let mut buttons = vec![(None, all_btn.clone())];
    for r in roles(&all) {
        let b = gtk::ToggleButton::with_label(role_label(r));
        b.add_css_class("flat");
        b.set_group(Some(&all_btn));
        filters.append(&b);
        buttons.push((Some(r), b));
    }
    let filter_scroller = gtk::ScrolledWindow::new();
    filter_scroller.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Never);
    filter_scroller.set_child(Some(&filters));

    // The list.
    let list = gtk::ListBox::new();
    list.add_css_class("boxed-list");
    list.set_selection_mode(gtk::SelectionMode::None);
    list.set_margin_start(12);
    list.set_margin_end(12);
    list.set_margin_top(6);
    list.set_margin_bottom(12);
    for p in &all {
        let row = adw::ActionRow::new();
        row.set_title(p.name);
        row.set_subtitle(&format!("{} - Built-in", role_label(p.role)));
        row.set_activatable(true);
        row.set_tooltip_text(Some("Add as a New Channel"));
        let use_btn = gtk::Button::from_icon_name("emblem-synchronizing-symbolic");
        use_btn.add_css_class("flat");
        use_btn.set_valign(gtk::Align::Center);
        use_btn.set_tooltip_text(Some("Use on the Selected Channel"));
        use_btn.update_property(&[gtk::accessible::Property::Label(&format!(
            "Use {} on the selected channel",
            p.name
        ))]);
        row.add_suffix(&use_btn);
        let (a, name) = (app.clone(), p.name.to_string());
        row.connect_activated(move |_| {
            if channels::add(&a, NewChannel::Preset(name.clone())).is_some() {
                let a2 = a.clone();
                a.toast_action(&format!("Added {name}"), "Undo", move || a2.undo());
            }
        });
        let (a, params) = (app.clone(), p.params);
        use_btn.connect_clicked(move |_| {
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
                    a.edit(presets::apply_edits(c, &params));
                }
                _ => a.toast("Select a channel with a built-in sound first"),
            }
        });
        list.append(&row);
    }

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
        let (state, list, stack, all) = (
            state.clone(),
            list.clone(),
            stack.clone(),
            presets::presets(),
        );
        move || {
            let s = state.borrow();
            let mut shown = 0;
            let mut i = 0;
            while let Some(row) = list.row_at_index(i) {
                let ok = all
                    .get(i as usize)
                    .map(|p| matches(p, &s.search, s.role))
                    .unwrap_or(false);
                row.set_visible(ok);
                shown += ok as usize;
                i += 1;
            }
            stack.set_visible_child_name(if shown == 0 { "empty" } else { "results" });
        }
    });
    {
        let (st, ap) = (state.clone(), apply.clone());
        search.connect_search_changed(move |e| {
            st.borrow_mut().search = e.text().to_string();
            ap();
        });
        for (role, b) in &buttons {
            let (state, apply, role) = (state.clone(), apply.clone(), *role);
            b.connect_toggled(move |b| {
                if b.is_active() {
                    state.borrow_mut().role = role;
                    apply();
                }
            });
        }
        let (search, all_btn, apply, state) = (
            search.clone(),
            all_btn.clone(),
            apply.clone(),
            state.clone(),
        );
        clear.connect_clicked(move |_| {
            search.set_text("");
            all_btn.set_active(true);
            let mut s = state.borrow_mut();
            s.search.clear();
            s.role = None;
            drop(s);
            apply();
        });
    }

    let view = adw::ToolbarView::new();
    view.add_top_bar(&search_box);
    view.add_top_bar(&filter_scroller);
    view.set_content(Some(&stack));
    view.upcast()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_and_role_filter() {
        let all = presets::presets();
        let kick = all.iter().find(|p| p.name == "Kick").unwrap();
        assert!(matches(kick, "", None));
        assert!(matches(kick, "kic", None));
        assert!(matches(kick, "  KICK ", None));
        assert!(matches(kick, "drums", None), "role is searchable");
        assert!(matches(kick, "built-in", None));
        assert!(!matches(kick, "bass", None));
        assert!(matches(kick, "", Some("Drum")));
        assert!(!matches(kick, "", Some("Bass")));
        assert!(!matches(kick, "kick", Some("Bass")));
    }

    #[test]
    fn roles_are_listed_once_in_order() {
        let r = roles(&presets::presets());
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
        let all = presets::presets();
        for role in roles(&all) {
            assert!(all.iter().any(|p| matches(p, "", Some(role))), "{role}");
        }
    }
}
