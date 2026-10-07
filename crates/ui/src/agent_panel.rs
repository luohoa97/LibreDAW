// SPDX-License-Identifier: GPL-3.0-or-later
//! The Agent page of the inspector (docs/ui-design.md 3.9) and the approval
//! banner. Persistent state is a banner; the human click that SPEC 17.1
//! requires is the Allow button in the row, which the agent cannot press.
//! Text that came from an agent is shown as plain text, shortened.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use crate::app::{App, UiCommand};
use crate::control_bridge::{self, AgentUi};

/// The banner title for the current state, if the banner should show.
pub fn banner_title(ui: &AgentUi) -> Option<String> {
    match ui.pending.len() {
        0 if ui.wants_control && !ui.enabled => Some("An agent wants to control Oto".into()),
        0 => None,
        1 => Some(format!("An agent wants to {}", ui.pending[0].summary)),
        n => Some(format!("{n} agent requests need your approval")),
    }
}

pub struct AgentPanel {
    pub widget: gtk::Widget,
    app: Rc<App>,
    page: adw::PreferencesPage,
    groups: RefCell<Vec<adw::PreferencesGroup>>,
    updating: Cell<bool>,
}

fn clock(unix_s: u64) -> String {
    glib::DateTime::from_unix_local(unix_s as i64)
        .ok()
        .and_then(|d| d.format("%H:%M").ok())
        .map(|s| s.to_string())
        .unwrap_or_default()
}

impl AgentPanel {
    pub fn new(app: &Rc<App>) -> Rc<AgentPanel> {
        let page = adw::PreferencesPage::new();
        let p = Rc::new(AgentPanel {
            widget: page.clone().upcast(),
            app: app.clone(),
            page,
            groups: RefCell::new(Vec::new()),
            updating: Cell::new(false),
        });
        let q = p.clone();
        app.on_command(move |c| {
            if c == UiCommand::AgentChanged {
                q.rebuild();
            }
        });
        p.rebuild();
        p
    }

    fn rebuild(self: &Rc<AgentPanel>) {
        // Not while a button of the old rows is still running.
        let me = self.clone();
        glib::idle_add_local_once(move || me.rebuild_now());
    }

    fn rebuild_now(self: &Rc<AgentPanel>) {
        for g in self.groups.borrow_mut().drain(..) {
            self.page.remove(&g);
        }
        let ui: AgentUi = self
            .app
            .bridge
            .borrow()
            .as_ref()
            .map(|b| b.ui.clone())
            .unwrap_or_default();
        let available = self.app.bridge.borrow().is_some();

        let session = adw::PreferencesGroup::new();
        session.set_title("Agent Session");
        session.set_description(Some("Agents can edit this project while this is on."));
        let who = adw::ActionRow::new();
        who.set_title("Connected agent");
        who.set_subtitle(&if ui.clients.is_empty() {
            "No agent connected".to_string()
        } else {
            ui.clients
                .iter()
                .map(|c| crate::control_bridge::author_of(c).tag())
                .map(|t| t.replace("agent:", ""))
                .collect::<Vec<_>>()
                .join(", ")
        });
        who.add_css_class("property");
        session.add(&who);
        let allow = adw::SwitchRow::new();
        allow.set_title("Allow Agent Control");
        allow.set_subtitle(if available {
            "Until you quit Oto"
        } else {
            "Not available: the control socket could not start"
        });
        allow.set_sensitive(available);
        self.updating.set(true);
        allow.set_active(ui.enabled);
        self.updating.set(false);
        {
            let me = self.clone();
            allow.connect_active_notify(move |r| {
                if me.updating.get() {
                    return;
                }
                if let Some(b) = me.app.bridge.borrow_mut().as_mut() {
                    b.set_enabled(r.is_active());
                }
                me.app.command(UiCommand::AgentChanged);
            });
        }
        session.add(&allow);
        self.page.add(&session);
        self.groups.borrow_mut().push(session);

        // The commands to paste into Claude Code, Codex and the others.
        let connect = crate::prefs::connect_group(&self.app);
        self.page.add(&connect);
        self.groups.borrow_mut().push(connect);

        if !ui.pending.is_empty() {
            let waiting = adw::PreferencesGroup::new();
            waiting.set_title("Waiting for You");
            for a in &ui.pending {
                let row = adw::ActionRow::new();
                row.set_title(&glib::markup_escape_text(&a.summary));
                row.set_title_lines(3);
                row.set_subtitle(&format!(
                    "{} - Waiting - {} s",
                    glib::markup_escape_text(&a.client),
                    a.since.elapsed().as_secs()
                ));
                let deny = gtk::Button::with_label("Deny");
                deny.add_css_class("flat");
                deny.set_valign(gtk::Align::Center);
                let allow = gtk::Button::with_label("Allow");
                allow.add_css_class("suggested-action");
                allow.set_valign(gtk::Align::Center);
                let (app, t) = (self.app.clone(), a.ticket);
                deny.connect_clicked(move |_| control_bridge::decide(&app, t, false));
                let (app, t) = (self.app.clone(), a.ticket);
                allow.connect_clicked(move |_| control_bridge::decide(&app, t, true));
                row.add_suffix(&deny);
                row.add_suffix(&allow);
                waiting.add(&row);
            }
            self.page.add(&waiting);
            self.groups.borrow_mut().push(waiting);
        }

        if !ui.recent.is_empty() {
            let recent = adw::PreferencesGroup::new();
            recent.set_title("Recent Activity");
            let current_author = self
                .app
                .session
                .borrow()
                .editor
                .history()
                .author_of_current()
                .tag();
            for (i, a) in ui.recent.iter().enumerate() {
                let row = adw::ActionRow::new();
                row.set_title(&glib::markup_escape_text(&a.text));
                row.set_title_lines(2);
                row.set_subtitle(&format!(
                    "{} - {}",
                    glib::markup_escape_text(&a.author),
                    clock(a.unix_s)
                ));
                if i == 0 && current_author == a.author {
                    let undo = gtk::Button::with_label("Undo");
                    undo.add_css_class("flat");
                    undo.set_valign(gtk::Align::Center);
                    let app = self.app.clone();
                    undo.connect_clicked(move |_| app.undo());
                    row.add_suffix(&undo);
                }
                if !a.focus.is_empty() {
                    let (hover, foci) = (gtk::EventControllerMotion::new(), a.focus.clone());
                    hover.connect_enter(move |_, _, _| crate::presence_ui::hover(foci.clone()));
                    hover.connect_leave(|_| crate::presence_ui::hover(Vec::new()));
                    row.add_controller(hover);
                }
                recent.add(&row);
            }
            self.page.add(&recent);
            self.groups.borrow_mut().push(recent);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control_bridge::Approval;
    use control::Ticket;
    use std::time::Instant;

    fn approval(summary: &str) -> Approval {
        Approval {
            ticket: Ticket(1),
            summary: summary.into(),
            client: "c".into(),
            since: Instant::now(),
        }
    }

    #[test]
    fn banner_text() {
        let mut ui = AgentUi::default();
        assert_eq!(banner_title(&ui), None);
        ui.wants_control = true;
        assert_eq!(
            banner_title(&ui).as_deref(),
            Some("An agent wants to control Oto")
        );
        ui.enabled = true;
        assert_eq!(banner_title(&ui), None, "nothing to say once it is allowed");
        ui.pending.push(approval("add the plugin Surge XT"));
        assert_eq!(
            banner_title(&ui).as_deref(),
            Some("An agent wants to add the plugin Surge XT")
        );
        ui.pending.push(approval("open another project"));
        ui.pending.push(approval("x"));
        assert_eq!(
            banner_title(&ui).as_deref(),
            Some("3 agent requests need your approval")
        );
    }
}
