// SPDX-License-Identifier: GPL-3.0-or-later
//! The Timeline page (SPEC 20.3): the instrument names beside the timeline
//! lanes, and the clip editor docked below when a clip is selected. One
//! model: rows are instruments, music lives in clips.

use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use crate::app::{App, UiCommand};
use crate::channel_list::ChannelList;
use crate::clip_editor::ClipEditor;
use crate::shortcuts;
use crate::widgets::timeline::Timeline;

pub struct TimelinePage {
    pub widget: gtk::Widget,
    pub timeline: Timeline,
    pub channels: Rc<ChannelList>,
    pub editor: Rc<ClipEditor>,
}

fn flat_button(icon: &str, tip: &str) -> gtk::Button {
    let b = gtk::Button::from_icon_name(icon);
    b.add_css_class("flat");
    b.set_tooltip_text(Some(tip));
    b.update_property(&[gtk::accessible::Property::Label(tip)]);
    b
}

/// Opens the Sounds pane, the one place to choose a sound.
fn add_instrument_button(pill: bool) -> gtk::Button {
    let b = gtk::Button::new();
    b.set_action_name(Some("win.add-sound"));
    b.set_tooltip_text(Some("Pick a Sound for a New Instrument"));
    if pill {
        b.set_label("Add Instrument");
        b.add_css_class("suggested-action");
        b.add_css_class("pill");
        b.set_halign(gtk::Align::Center);
    } else {
        b.set_child(Some(
            &adw::ButtonContent::builder()
                .icon_name("list-add-symbolic")
                .label("Add Instrument")
                .build(),
        ));
        b.add_css_class("flat");
    }
    b
}

pub fn build(app: &Rc<App>) -> TimelinePage {
    let timeline = Timeline::new(app.clone());
    let channels = ChannelList::new(app, &timeline.vadj());
    let editor = ClipEditor::new(app);

    // ---- toolbar over the lanes ----
    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    bar.add_css_class("toolbar");
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    let zoom_out = flat_button(
        "zoom-out-symbolic",
        &shortcuts::tooltip("Zoom Out", "win.zoom-out"),
    );
    let zoom_in = flat_button(
        "zoom-in-symbolic",
        &shortcuts::tooltip("Zoom In", "win.zoom-in"),
    );
    bar.append(&add_instrument_button(false));
    bar.append(&spacer);
    bar.append(&zoom_out);
    bar.append(&zoom_in);
    {
        let t = timeline.clone();
        zoom_in.connect_clicked(move |_| t.zoom_x(1.3));
        let t = timeline.clone();
        zoom_out.connect_clicked(move |_| t.zoom_x(1.0 / 1.3));
    }

    // ---- names beside lanes ----
    let lanes = gtk::Grid::new();
    let vs = gtk::Scrollbar::new(gtk::Orientation::Vertical, Some(&timeline.vadj()));
    let hs = gtk::Scrollbar::new(gtk::Orientation::Horizontal, Some(&timeline.hadj()));
    vs.set_margin_top(crate::timeline_logic::RULER_H as i32);
    lanes.attach(&timeline, 0, 0, 1, 1);
    lanes.attach(&vs, 1, 0, 1, 1);
    lanes.attach(&hs, 0, 1, 1, 1);
    let body = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    channels.widget.set_margin_end(6);
    body.append(&channels.widget);
    // The first thing to do, until there is a clip (SPEC 20.6).
    let first_hint = gtk::Label::new(Some("Click an empty spot in a row to add a clip"));
    first_hint.add_css_class("dim-label");
    first_hint.add_css_class("title-4");
    first_hint.set_can_target(false);
    first_hint.set_halign(gtk::Align::Center);
    first_hint.set_valign(gtk::Align::Center);
    let lanes_overlay = gtk::Overlay::new();
    lanes_overlay.set_child(Some(&lanes));
    lanes_overlay.add_overlay(&first_hint);
    body.append(&lanes_overlay);
    crate::samples_ui::install_drop_target(&body, app);

    // Empty state: no instruments yet.
    let empty = adw::StatusPage::new();
    empty.set_icon_name(Some("audio-x-generic-symbolic"));
    empty.set_title("No Instruments Yet");
    empty.set_description(Some(
        "Add an instrument, then click its row to add a clip. You can also drag a sound in from the sounds list.",
    ));
    empty.add_css_class("compact");
    empty.set_child(Some(&add_instrument_button(true)));
    let stack = gtk::Stack::new();
    stack.add_named(&body, Some("lanes"));
    stack.add_named(&empty, Some("empty"));
    stack.add_css_class("view");
    stack.set_vexpand(true);

    let top = adw::ToolbarView::new();
    top.add_top_bar(&bar);
    top.set_content(Some(&stack));

    // ---- the editor under the lanes ----
    let paned = gtk::Paned::new(gtk::Orientation::Vertical);
    paned.set_start_child(Some(&top));
    paned.set_end_child(Some(&editor.widget));
    paned.set_shrink_start_child(false);
    paned.set_shrink_end_child(false);
    paned.set_resize_start_child(true);
    paned.set_resize_end_child(true);
    editor.widget.set_visible(false);
    paned.set_vexpand(true);
    paned.set_hexpand(true);

    // The editor takes about half the page the first time it opens.
    let placed = Rc::new(Cell::new(false));
    let sync = {
        let first_hint = first_hint.clone();
        let (a, st, ed, p, placed) = (
            app.clone(),
            stack.clone(),
            editor.clone(),
            paned.clone(),
            placed.clone(),
        );
        move || {
            let (instruments, clip) = {
                let s = a.session.borrow();
                (
                    !s.document().project.channels.is_empty(),
                    a.current_clip().is_some(),
                )
            };
            st.set_visible_child_name(if instruments { "lanes" } else { "empty" });
            let no_clips = a.session.borrow().document().project.clips.is_empty();
            first_hint.set_visible(instruments && no_clips);
            if ed.widget.is_visible() != clip {
                ed.widget.set_visible(clip);
                if clip && !placed.replace(true) {
                    let p2 = p.clone();
                    glib::idle_add_local_once(move || {
                        let h = p2.height();
                        if h > 0 {
                            p2.set_position(h * 45 / 100);
                        }
                    });
                }
            }
        }
    };
    sync();
    app.on_change(sync);

    {
        let ed = editor.clone();
        app.on_command(move |c| {
            if c == UiCommand::EditNotes {
                ed.focus();
            }
        });
    }
    install_escape(app, &paned, &channels);

    TimelinePage {
        widget: paned.upcast(),
        timeline,
        channels,
        editor,
    }
}

/// Escape closes the clip editor first, then clears the instrument
/// selection (a field being edited or a menu takes Escape before both).
fn install_escape(app: &Rc<App>, page: &gtk::Paned, channels: &Rc<ChannelList>) {
    let keys = gtk::EventControllerKey::new();
    let (a, ch) = (app.clone(), channels.clone());
    keys.connect_key_pressed(move |c, key, _, state| {
        if key != gtk::gdk::Key::Escape || !crate::keys::plain(state) || ch.is_editing() {
            return glib::Propagation::Proceed;
        }
        let in_text = c
            .widget()
            .and_then(|w| w.root())
            .and_then(|r| r.focus())
            .is_some_and(|f| f.is::<gtk::Text>() || f.is::<gtk::Editable>());
        if in_text {
            return glib::Propagation::Proceed;
        }
        if a.deselect_clip() || a.deselect_channel() {
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
    });
    page.add_controller(keys);
}
