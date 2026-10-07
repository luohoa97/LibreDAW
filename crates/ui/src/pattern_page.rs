// SPDX-License-Identifier: GPL-3.0-or-later
//! The Pattern page (docs/ui-design.md 3.2 to 3.4): a pattern strip, then
//! steps over notes in one vertical paned. The steps section is a channel
//! header column beside one step grid; the notes section is the piano roll
//! with its toolbar. At narrow widths one section shows at a time.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::gio;
use gtk::glib;

use protocol::consts::MAX_STEPS;
use protocol::edit::Edit;
use protocol::ids::{ChannelId, PatternId};

use crate::app::{App, UiCommand};
use crate::channel_list::ChannelList;
use crate::lane_logic::Lane;
use crate::shortcuts;
use crate::size_class::{PatternFocus, split_position};
use crate::view_math::SNAPS;
use crate::widgets::lane_editor::LaneEditor;
use crate::widgets::piano_roll::PianoRoll;
use crate::widgets::step_grid::StepGrid;
use doc::presets;

pub struct PatternPage {
    pub widget: gtk::Widget,
    pub roll: PianoRoll,
    pub snap: gtk::DropDown,
    pub paned: gtk::Paned,
    pub channels: Rc<ChannelList>,
    /// Share of the height for steps, applied once the paned has a size.
    pub pending_split: Rc<Cell<Option<f64>>>,
}

/// A toolbar row that never forces the window wider: it scrolls sideways
/// when it does not fit.
fn toolbar_scroller(content: &impl IsA<gtk::Widget>) -> gtk::ScrolledWindow {
    let sw = gtk::ScrolledWindow::new();
    sw.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Never);
    sw.set_propagate_natural_height(true);
    sw.set_min_content_width(0);
    sw.set_child(Some(content));
    sw
}

fn flat_button(icon: &str, tip: &str) -> gtk::Button {
    let b = gtk::Button::from_icon_name(icon);
    b.add_css_class("flat");
    b.set_tooltip_text(Some(tip));
    b.update_property(&[gtk::accessible::Property::Label(tip)]);
    b
}

/// The "Add Channel" menu: built-in sounds grouped by role, then a plugin.
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

pub fn build(window: &adw::ApplicationWindow, app: &Rc<App>) -> PatternPage {
    let _ = window;
    let channels = ChannelList::new(app);
    let steps_box = build_steps(app, &channels);
    let (roll, notes_box, snap) = build_notes(app);

    let paned = gtk::Paned::new(gtk::Orientation::Vertical);
    paned.set_start_child(Some(&steps_box));
    paned.set_end_child(Some(&notes_box));
    paned.set_resize_start_child(true);
    paned.set_resize_end_child(true);
    paned.set_shrink_start_child(false);
    paned.set_shrink_end_child(false);
    paned.set_wide_handle(true);

    // The pattern strip above the paned.
    let strip = build_strip(app);
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.append(&strip);
    root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    paned.set_vexpand(true);
    root.append(&paned);

    // Which sections show: the focus and the size class decide.
    let apply_focus = {
        let (a, s, n) = (app.clone(), steps_box.clone(), notes_box.clone());
        move || {
            let f = a.effective_focus();
            s.set_visible(f.shows_steps());
            n.set_visible(f.shows_notes());
        }
    };
    apply_focus();
    app.on_view_change(apply_focus);

    // The divider starts at the size class's share once it has a height.
    let pending = Rc::new(Cell::new(Some(app.size_class().default_split())));
    {
        let (p, pend) = (paned.clone(), pending.clone());
        paned.add_tick_callback(move |_, _| {
            let total = p.height();
            if total > 0
                && let Some(f) = pend.take()
            {
                p.set_position(split_position(total, f, 120));
            }
            glib::ControlFlow::Continue
        });
    }

    // "Edit Notes" from a channel menu.
    {
        let (a, r) = (app.clone(), roll.clone());
        app.on_command(move |c| {
            if c == UiCommand::EditNotes {
                if a.size_class().can_show_both() {
                    if a.pattern_focus() == PatternFocus::Steps {
                        a.set_pattern_focus(PatternFocus::Both);
                    }
                    r.grab_focus();
                } else {
                    a.set_pattern_focus(PatternFocus::Notes);
                }
            }
        });
    }

    PatternPage {
        widget: root.upcast(),
        roll,
        snap,
        paned,
        channels,
        pending_split: pending,
    }
}

// ---- the pattern strip ----

fn build_strip(app: &Rc<App>) -> gtk::ScrolledWindow {
    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    bar.add_css_class("toolbar");

    let pattern_btn = gtk::MenuButton::new();
    pattern_btn.set_tooltip_text(Some("Choose Pattern"));
    pattern_btn.update_property(&[gtk::accessible::Property::Label("Choose pattern")]);
    let pat_list = gtk::ListBox::new();
    pat_list.add_css_class("navigation-sidebar");
    pat_list.set_selection_mode(gtk::SelectionMode::None);
    let pat_scroll = gtk::ScrolledWindow::builder()
        .child(&pat_list)
        .propagate_natural_height(true)
        .max_content_height(320)
        .min_content_width(200)
        .build();
    let pop = gtk::Popover::new();
    pop.set_child(Some(&pat_scroll));
    pattern_btn.set_popover(Some(&pop));

    let add = flat_button("list-add-symbolic", "New Pattern");
    let remove = flat_button("user-trash-symbolic", "Remove Pattern");
    let bars_label = gtk::Label::new(Some("Bars"));
    bars_label.add_css_class("dim-label");
    let bars = gtk::SpinButton::with_range(1.0, 16.0, 1.0);
    bars.set_tooltip_text(Some("Pattern Length in Bars"));
    bars.update_property(&[gtk::accessible::Property::Label("Pattern length in bars")]);
    // Swing: percent of a step by which odd steps are delayed (15.4).
    let swing_label = gtk::Label::new(Some("Swing"));
    swing_label.add_css_class("dim-label");
    let swing = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 75.0, 1.0);
    swing.set_width_request(140);
    swing.set_draw_value(true);
    swing.set_value_pos(gtk::PositionType::Right);
    swing.set_format_value_func(|_, v| format!("{}%", v.round() as i32));
    swing.add_mark(0.0, gtk::PositionType::Bottom, None);
    swing.add_mark(50.0, gtk::PositionType::Bottom, None);
    swing.set_tooltip_text(Some("Swing"));
    swing.update_property(&[gtk::accessible::Property::Label("Swing")]);
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    let focus = gtk::Button::from_icon_name("view-dual-symbolic");
    focus.add_css_class("flat");
    focus.set_action_name(Some("win.focus-pattern"));
    focus.set_tooltip_text(Some(&shortcuts::tooltip(
        "Focus Steps or Notes",
        "win.focus-pattern",
    )));
    focus.update_property(&[gtk::accessible::Property::Label("Focus steps or notes")]);

    for w in [
        pattern_btn.upcast_ref::<gtk::Widget>(),
        add.upcast_ref(),
        remove.upcast_ref(),
        bars.upcast_ref(),
        bars_label.upcast_ref(),
        swing_label.upcast_ref(),
        swing.upcast_ref(),
        spacer.upcast_ref(),
        focus.upcast_ref(),
    ] {
        bar.append(w);
    }

    let updating = Rc::new(Cell::new(false));
    let ids: Rc<RefCell<Vec<PatternId>>> = Rc::new(RefCell::new(Vec::new()));

    {
        let a = app.clone();
        add.connect_clicked(move |_| {
            let n = a.session.borrow().document().project.patterns.len() + 1;
            if let Some(r) = a.edit(vec![Edit::AddPattern {
                name: format!("Pattern {n}"),
                length_steps: 16,
            }]) {
                a.select_pattern(PatternId(r.created[0]));
            }
        });
        let a = app.clone();
        remove.connect_clicked(move |_| {
            if let Some(p) = a.current_pattern() {
                a.edit(vec![Edit::RemovePattern { pattern: p }]);
                let a2 = a.clone();
                a.toast_action("Pattern removed", "Undo", move || a2.undo());
            }
        });
        let (a, up) = (app.clone(), updating.clone());
        bars.connect_value_changed(move |s| {
            if up.get() {
                return;
            }
            let Some(p) = a.current_pattern() else { return };
            let per_bar = steps_per_bar(&a);
            let want = (s.value() as u32 * per_bar).min(MAX_STEPS as u32) as u8;
            let cur = a
                .session
                .borrow()
                .document()
                .project
                .pattern(p)
                .map(|x| x.length_steps);
            if cur != Some(want) {
                a.edit(vec![Edit::SetPatternLength {
                    pattern: p,
                    length_steps: want,
                }]);
            }
        });
    }

    {
        let (a, up) = (app.clone(), updating.clone());
        swing.connect_value_changed(move |s| {
            if up.get() {
                return;
            }
            let Some(p) = a.current_pattern() else { return };
            let want = (s.value().round() as u16 * 10).min(protocol::consts::MAX_SWING);
            let cur = a
                .session
                .borrow()
                .document()
                .project
                .pattern(p)
                .map(|x| x.swing);
            if cur != Some(want) {
                a.edit_resting(
                    "Swing",
                    vec![Edit::SetSwing {
                        pattern: p,
                        swing: want,
                    }],
                );
            }
        });
    }

    let sync = {
        let (a, ids, up, btn, list, bars, pop, swing) = (
            app.clone(),
            ids.clone(),
            updating.clone(),
            pattern_btn.clone(),
            pat_list.clone(),
            bars.clone(),
            pop.clone(),
            swing.clone(),
        );
        move || {
            up.set(true);
            let cur_swing = a
                .current_pattern()
                .and_then(|id| {
                    a.session
                        .borrow()
                        .document()
                        .project
                        .pattern(id)
                        .map(|p| p.swing)
                })
                .unwrap_or(0);
            let pct = cur_swing as f64 / 10.0;
            if (swing.value() - pct).abs() > 0.5 {
                swing.set_value(pct);
            }
            swing.set_sensitive(a.current_pattern().is_some());
            let (names, new_ids) = {
                let s = a.session.borrow();
                let p = &s.document().project;
                (
                    p.patterns
                        .iter()
                        .map(|x| x.name.clone())
                        .collect::<Vec<_>>(),
                    p.patterns.iter().map(|x| x.id).collect::<Vec<_>>(),
                )
            };
            let cur_name = a
                .current_pattern()
                .and_then(|id| new_ids.iter().position(|x| *x == id))
                .map(|i| names[i].clone());
            let content = adw::ButtonContent::builder()
                .label(cur_name.as_deref().unwrap_or("No Pattern"))
                .icon_name("pan-down-symbolic")
                .build();
            btn.set_child(Some(&content));
            if *ids.borrow() != new_ids || list.first_child().is_none() {
                while let Some(c) = list.first_child() {
                    list.remove(&c);
                }
                for (i, n) in names.iter().enumerate() {
                    let l = gtk::Label::new(Some(n));
                    l.set_xalign(0.0);
                    l.set_margin_top(6);
                    l.set_margin_bottom(6);
                    l.set_margin_start(6);
                    l.set_margin_end(6);
                    let row = gtk::ListBoxRow::new();
                    row.set_child(Some(&l));
                    let (a2, id, pop2) = (a.clone(), new_ids[i], pop.clone());
                    let click = gtk::GestureClick::new();
                    click.connect_released(move |_, _, _, _| {
                        a2.select_pattern(id);
                        pop2.popdown();
                    });
                    row.add_controller(click);
                    row.set_activatable(true);
                    list.append(&row);
                }
                *ids.borrow_mut() = new_ids.clone();
            }
            let per_bar = steps_per_bar(&a);
            let steps = a.current_pattern().and_then(|id| {
                a.session
                    .borrow()
                    .document()
                    .project
                    .pattern(id)
                    .map(|p| p.length_steps)
            });
            if let Some(steps) = steps {
                let max_bars = (MAX_STEPS as u32 / per_bar).max(1);
                bars.set_range(1.0, max_bars as f64);
                let b = (steps as u32).div_ceil(per_bar).max(1) as f64;
                if (bars.value() - b).abs() > 0.5 {
                    bars.set_value(b);
                }
            }
            up.set(false);
        }
    };
    sync();
    app.on_change(sync);
    toolbar_scroller(&bar)
}

fn steps_per_bar(app: &App) -> u32 {
    app.session.borrow().document().project.time_sig_num as u32 * 4
}

// ---- steps ----

fn build_steps(app: &Rc<App>, channels: &Rc<ChannelList>) -> gtk::Widget {
    let stack = gtk::Stack::new();
    stack.set_vexpand(true);
    stack.set_hexpand(true);

    // Empty state.
    let empty = adw::StatusPage::new();
    empty.set_icon_name(Some("audio-x-generic-symbolic"));
    empty.set_title("No Channels Yet");
    empty.set_description(Some(
        "Add a sound to start your beat. You can also drag one in from the sounds list.",
    ));
    empty.add_css_class("compact");
    let add = gtk::MenuButton::new();
    add.set_label("Add Channel");
    add.set_menu_model(Some(&add_channel_menu()));
    add.add_css_class("suggested-action");
    add.add_css_class("pill");
    add.set_halign(gtk::Align::Center);
    add.update_property(&[gtk::accessible::Property::Label("Add channel")]);
    empty.set_child(Some(&add));
    stack.add_named(&empty, Some("empty"));

    // Rows: header column and grid side by side, scrolling together.
    let grid = StepGrid::new(app.clone());
    let lane = LaneEditor::new(app.clone());
    let grid_col = gtk::Box::new(gtk::Orientation::Vertical, 0);
    grid_col.append(&grid);
    grid_col.append(&lane);
    let hscroll = gtk::ScrolledWindow::new();
    hscroll.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Never);
    hscroll.set_hexpand(true);
    hscroll.set_min_content_width(0);
    hscroll.set_child(Some(&grid_col));
    // Under the channel names: the lane selector, level with the lane.
    let selector = lane_selector(&lane);
    let left = gtk::Box::new(gtk::Orientation::Vertical, 0);
    left.append(&channels.widget);
    left.append(&selector);
    let both = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    both.append(&left);
    both.append(&gtk::Separator::new(gtk::Orientation::Vertical));
    both.append(&hscroll);
    let add_row = gtk::MenuButton::new();
    add_row.set_child(Some(
        &adw::ButtonContent::builder()
            .icon_name("list-add-symbolic")
            .label("Add Channel")
            .build(),
    ));
    add_row.set_menu_model(Some(&add_channel_menu()));
    add_row.add_css_class("flat");
    add_row.set_halign(gtk::Align::Start);
    add_row.set_margin_start(6);
    add_row.set_margin_top(4);
    add_row.set_margin_bottom(6);
    add_row.set_tooltip_text(Some("Add Channel"));
    let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
    column.append(&both);
    column.append(&add_row);
    let vscroll = gtk::ScrolledWindow::new();
    vscroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    vscroll.set_vexpand(true);
    vscroll.set_child(Some(&column));
    stack.add_named(&vscroll, Some("rows"));
    // WAV files dropped from the file manager become sampler channels.
    crate::samples_ui::install_drop_target(&stack, app);

    let sync = {
        let (a, st) = (app.clone(), stack.clone());
        move || {
            let none = a.session.borrow().document().project.channels.is_empty();
            st.set_visible_child_name(if none { "empty" } else { "rows" });
        }
    };
    sync();
    app.on_change(sync);
    stack.upcast()
}

/// The three linked lane buttons (docs/ui-design.md 3.3). Only one lane is
/// open; pressing the open one closes the lane.
fn lane_selector(lane: &LaneEditor) -> gtk::Widget {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    row.add_css_class("linked");
    row.set_valign(gtk::Align::Start);
    row.set_halign(gtk::Align::Start);
    row.set_margin_start(6);
    row.set_margin_top(8);
    row.set_margin_bottom(6);
    row.update_property(&[gtk::accessible::Property::Label("Step lanes")]);
    let buttons: Vec<(Lane, gtk::ToggleButton)> = Lane::ALL
        .iter()
        .map(|l| {
            let b = gtk::ToggleButton::with_label(l.label());
            b.set_tooltip_text(Some(l.tooltip()));
            b.add_css_class("caption");
            row.append(&b);
            (*l, b)
        })
        .collect();
    let guard = Rc::new(Cell::new(false));
    for (l, b) in &buttons {
        let (editor, all, guard, l) = (lane.clone(), buttons.clone(), guard.clone(), *l);
        b.connect_toggled(move |me| {
            if guard.get() {
                return;
            }
            guard.set(true);
            if me.is_active() {
                for (other, ob) in &all {
                    if *other != l {
                        ob.set_active(false);
                    }
                }
                editor.set_lane(Some(l));
            } else {
                editor.set_lane(None);
            }
            guard.set(false);
        });
    }
    // Screenshot aid: `LIBREDAW_LANE=velocity|pitch|ratchet` opens a lane.
    if let Ok(want) = std::env::var("LIBREDAW_LANE") {
        let buttons = buttons.clone();
        glib::timeout_add_local_once(std::time::Duration::from_millis(900), move || {
            if let Some((_, b)) = buttons
                .iter()
                .find(|(l, _)| l.label().eq_ignore_ascii_case(&want))
            {
                b.set_active(true);
            }
        });
    }
    let scroller = toolbar_scroller(&row);
    scroller.set_valign(gtk::Align::Start);
    scroller.upcast()
}

// ---- notes ----

fn build_notes(app: &Rc<App>) -> (PianoRoll, gtk::Widget, gtk::DropDown) {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);

    let tools = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    tools.add_css_class("toolbar");
    let back = gtk::Button::new();
    back.set_child(Some(
        &adw::ButtonContent::builder()
            .icon_name("go-previous-symbolic")
            .label("Steps")
            .build(),
    ));
    back.add_css_class("flat");
    back.set_tooltip_text(Some("Back to Steps"));
    back.set_visible(false);
    let channel = gtk::DropDown::from_strings(&[]);
    channel.set_tooltip_text(Some("Channel to Edit"));
    channel.update_property(&[gtk::accessible::Property::Label("Channel to edit")]);
    let labels: Vec<&str> = SNAPS.iter().map(|s| s.0).collect();
    let snap = gtk::DropDown::from_strings(&labels);
    snap.set_tooltip_text(Some("Snap to Grid"));
    snap.update_property(&[gtk::accessible::Property::Label("Snap to grid")]);
    let zoom_out = flat_button(
        "zoom-out-symbolic",
        &shortcuts::tooltip("Zoom Out", "win.zoom-out"),
    );
    let zoom_in = flat_button(
        "zoom-in-symbolic",
        &shortcuts::tooltip("Zoom In", "win.zoom-in"),
    );
    let vel = gtk::ToggleButton::new();
    vel.set_icon_name("view-list-symbolic");
    vel.add_css_class("flat");
    vel.set_active(true);
    vel.set_tooltip_text(Some("Show Velocity Lane"));
    vel.update_property(&[gtk::accessible::Property::Label("Show velocity lane")]);
    for w in [
        back.upcast_ref::<gtk::Widget>(),
        channel.upcast_ref(),
        snap.upcast_ref(),
        zoom_out.upcast_ref(),
        zoom_in.upcast_ref(),
        vel.upcast_ref(),
    ] {
        tools.append(w);
    }
    root.append(&toolbar_scroller(&tools));
    root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

    let roll = PianoRoll::new(app.clone());
    let grid = gtk::Grid::new();
    let vs = gtk::Scrollbar::new(gtk::Orientation::Vertical, Some(&roll.vadj()));
    let hs = gtk::Scrollbar::new(gtk::Orientation::Horizontal, Some(&roll.hadj()));
    grid.attach(&roll, 0, 0, 1, 1);
    grid.attach(&vs, 1, 0, 1, 1);
    grid.attach(&hs, 0, 1, 1, 1);
    let hint = gtk::Label::new(Some("Click the grid to add a note."));
    hint.add_css_class("dim-label");
    hint.set_can_target(false);
    hint.set_halign(gtk::Align::Center);
    hint.set_valign(gtk::Align::Center);
    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&grid));
    overlay.add_overlay(&hint);
    overlay.set_vexpand(true);

    let none = adw::StatusPage::new();
    none.set_icon_name(Some("document-edit-symbolic"));
    none.set_title("Pick a Channel");
    none.set_description(Some("Choose a channel in Steps to draw its notes here."));
    none.add_css_class("compact");
    let stack = gtk::Stack::new();
    stack.add_named(&overlay, Some("roll"));
    stack.add_named(&none, Some("none"));
    stack.set_vexpand(true);
    root.append(&stack);

    let updating = Rc::new(Cell::new(false));
    let ids: Rc<RefCell<Vec<ChannelId>>> = Rc::new(RefCell::new(Vec::new()));
    {
        let r = roll.clone();
        snap.connect_selected_notify(move |d| r.set_snap_index(d.selected() as usize));
        let r = roll.clone();
        zoom_in.connect_clicked(move |_| r.zoom_x(1.3));
        let r = roll.clone();
        zoom_out.connect_clicked(move |_| r.zoom_x(1.0 / 1.3));
        let r = roll.clone();
        vel.connect_toggled(move |b| r.set_velocity_lane(b.is_active()));
        let a = app.clone();
        back.connect_clicked(move |_| a.set_pattern_focus(PatternFocus::Steps));
        let (a, ids2, up) = (app.clone(), ids.clone(), updating.clone());
        channel.connect_selected_notify(move |d| {
            if up.get() {
                return;
            }
            if let Some(id) = ids2.borrow().get(d.selected() as usize).copied() {
                a.select_channel(id);
            }
        });
    }
    {
        let (a, b) = (app.clone(), back.clone());
        let f = move || b.set_visible(!a.size_class().can_show_both());
        f();
        app.on_view_change(f);
    }

    let sync = {
        let (a, stack, hint, channel, ids, up) = (
            app.clone(),
            stack.clone(),
            hint.clone(),
            channel.clone(),
            ids.clone(),
            updating.clone(),
        );
        move || {
            up.set(true);
            let (names, new_ids, has_notes, has_channel) = {
                let s = a.session.borrow();
                let p = &s.document().project;
                let cur = a.current_channel();
                let pat = a.current_pattern().and_then(|id| p.pattern(id));
                let has_notes = match (pat, cur) {
                    (Some(pt), Some(c)) => !pt.notes_of(c).is_empty(),
                    _ => false,
                };
                (
                    p.channels
                        .iter()
                        .map(|c| c.name.clone())
                        .collect::<Vec<_>>(),
                    p.channels.iter().map(|c| c.id).collect::<Vec<_>>(),
                    has_notes,
                    cur.is_some() && pat.is_some(),
                )
            };
            stack.set_visible_child_name(if has_channel { "roll" } else { "none" });
            hint.set_visible(has_channel && !has_notes);
            let current_names: Vec<String> = channel
                .model()
                .and_then(|m| m.downcast::<gtk::StringList>().ok())
                .map(|l| {
                    (0..l.n_items())
                        .filter_map(|i| l.string(i).map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();
            if current_names != names {
                let refs: Vec<&str> = names.iter().map(String::as_str).collect();
                channel.set_model(Some(&gtk::StringList::new(&refs)));
            }
            *ids.borrow_mut() = new_ids.clone();
            if let Some(i) = a
                .current_channel()
                .and_then(|id| new_ids.iter().position(|x| *x == id))
                && channel.selected() as usize != i
            {
                channel.set_selected(i as u32);
            }
            up.set(false);
        }
    };
    sync();
    app.on_change(sync);
    (roll, root.upcast(), snap)
}
