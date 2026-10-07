// SPDX-License-Identifier: GPL-3.0-or-later
//! The left utility pane: the sound browser (docs/ui-design.md 3.8).

use std::rc::Rc;

use adw::prelude::*;

use crate::app::App;

pub fn build(_app: &Rc<App>) -> gtk::Widget {
    let p = adw::StatusPage::new();
    p.set_icon_name(Some("folder-music-symbolic"));
    p.set_title("No Sounds Yet");
    p.set_description(Some(
        "Add a folder with your samples, or install a sound pack.",
    ));
    p.add_css_class("compact");
    p.upcast()
}
