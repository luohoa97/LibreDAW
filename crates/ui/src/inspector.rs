// SPDX-License-Identifier: GPL-3.0-or-later
//! The right utility pane (docs/ui-design.md 3.7, 3.9, 3.10): Sound, Agent,
//! and History pages behind a switcher bar.

use std::rc::Rc;

use adw::prelude::*;

use crate::app::App;

pub struct Inspector {
    pub widget: gtk::Widget,
    pub stack: adw::ViewStack,
}

fn status(icon: &str, title: &str, description: &str) -> adw::StatusPage {
    let p = adw::StatusPage::new();
    p.set_icon_name(Some(icon));
    p.set_title(title);
    p.set_description(Some(description));
    p.add_css_class("compact");
    p
}

pub fn build(_app: &Rc<App>) -> Inspector {
    let stack = adw::ViewStack::new();
    stack.add_titled_with_icon(
        &status(
            "audio-x-generic-symbolic",
            "No Sound Selected",
            "Choose a channel to change its sound.",
        ),
        Some("sound"),
        "Sound",
        "audio-x-generic-symbolic",
    );
    stack.add_titled_with_icon(
        &status(
            "network-workgroup-symbolic",
            "No Agent Connected",
            "Connect an agent with libredaw-mcp, then allow it here.",
        ),
        Some("agent"),
        "Agent",
        "network-workgroup-symbolic",
    );
    stack.add_titled_with_icon(
        &status(
            "document-open-recent-symbolic",
            "No Saved Versions",
            "Save a version before you try something big. You can always go back.",
        ),
        Some("history"),
        "History",
        "document-open-recent-symbolic",
    );
    let bar = adw::ViewSwitcherBar::new();
    bar.set_stack(Some(&stack));
    bar.set_reveal(true);
    let view = adw::ToolbarView::new();
    view.set_content(Some(&stack));
    view.add_bottom_bar(&bar);
    Inspector {
        widget: view.upcast(),
        stack,
    }
}
