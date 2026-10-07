// SPDX-License-Identifier: GPL-3.0-or-later
//! The Pattern page (docs/ui-design.md 3.2 to 3.4): an `AdwNavigationView`
//! with two pages. The root page "Steps" has the pattern strip on top and
//! the channel names beside the step grid. "Edit Notes" (the channel menu,
//! the notes button, a double-click or Return on a channel, Ctrl+Return)
//! pushes a page titled with the channel's name that shows the piano roll,
//! with a back button, Escape and Alt+Left to return. There are no
//! hand-drawn dividers: structure comes from the toolbar views and spacing.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use protocol::consts::MAX_STEPS;
use protocol::edit::Edit;
use protocol::ids::PatternId;

use crate::app::{App, UiCommand};
use crate::channel_list::ChannelList;
use crate::lane_logic::Lane;
use crate::selection::{self, NotesState, StepsState};
use crate::shortcuts;
use crate::view_math::SNAPS;
use crate::widgets::lane_editor::LaneEditor;
use crate::widgets::piano_roll::PianoRoll;
use crate::widgets::step_grid::StepGrid;

pub struct PatternPage {
    pub widget: gtk::Widget,
    pub roll: PianoRoll,
    pub snap: gtk::DropDown,
    pub channels: Rc<ChannelList>,
    pub nav: adw::NavigationView,
}

impl PatternPage {
    /// Whether the notes page is the one showing.
    pub fn showing_notes(&self) -> bool {
        on_notes(&self.nav)
    }

    /// Back to the steps.
    pub fn show_steps(&self) {
        if self.showing_notes() {
            self.nav.pop();
        }
    }
}

/// Whether the notes page is the one showing.
fn on_notes(nav: &adw::NavigationView) -> bool {
    nav.visible_page()
        .and_then(|p| p.tag())
        .is_some_and(|t| t.as_str() == NOTES_TAG)
}

const STEPS_TAG: &str = "steps";
const NOTES_TAG: &str = "notes";

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
pub fn build(window: &adw::ApplicationWindow, app: &Rc<App>) -> PatternPage {
    let _ = window;
    let channels = ChannelList::new(app);
    let steps_content = build_steps(app, &channels);
    let (roll, notes_page, snap) = build_notes(app);

    // Root page: the pattern strip over the steps.
    let strip = build_strip(app);
    let steps_view = adw::ToolbarView::new();
    steps_view.add_top_bar(&strip);
    steps_view.set_content(Some(&steps_content));
    let steps_page = adw::NavigationPage::with_tag(&steps_view, "Steps", STEPS_TAG);

    let nav = adw::NavigationView::new();
    nav.set_pop_on_escape(false);
    nav.add(&steps_page);
    nav.add(&notes_page.page);
    nav.set_hexpand(true);
    nav.set_vexpand(true);

    // "Edit Notes": show the page of the selected channel and move the
    // keyboard into the piano roll.
    {
        let (nav, r) = (nav.clone(), roll.clone());
        app.on_command(move |c| {
            if c != UiCommand::EditNotes {
                return;
            }
            r.reveal();
            let showing = on_notes(&nav);
            if !showing {
                nav.push_by_tag(NOTES_TAG);
            }
            let r = r.clone();
            glib::idle_add_local_once(move || {
                r.grab_focus();
            });
        });
    }
    // The notes page needs a channel: if the last one goes away, leave.
    {
        let (nav, a) = (nav.clone(), app.clone());
        app.on_change(move || {
            let showing = on_notes(&nav);
            if showing && a.current_channel().is_none() {
                nav.pop();
            }
        });
    }
    // The back button, Escape (when the roll has nothing to clear) and
    // Alt+Left return to the steps.
    {
        let nav2 = nav.clone();
        notes_page.back.connect_clicked(move |_| {
            nav2.pop();
        });
        let keys = gtk::EventControllerKey::new();
        let nav2 = nav.clone();
        keys.connect_key_pressed(move |_, key, _, state| {
            let back = key == gtk::gdk::Key::Escape
                || (key == gtk::gdk::Key::Left && state.contains(gtk::gdk::ModifierType::ALT_MASK));
            let on_page = on_notes(&nav2);
            if back && on_page {
                nav2.pop();
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
        notes_page.page.add_controller(keys);
    }

    PatternPage {
        widget: nav.clone().upcast(),
        roll,
        snap,
        channels,
        nav,
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

    for w in [
        pattern_btn.upcast_ref::<gtk::Widget>(),
        add.upcast_ref(),
        remove.upcast_ref(),
        bars.upcast_ref(),
        bars_label.upcast_ref(),
        swing_label.upcast_ref(),
        swing.upcast_ref(),
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
            if a.session.borrow().document().project.patterns.len() <= 1 {
                a.toast("A project needs at least one pattern");
                return;
            }
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

/// A status page with one button that makes the first pattern.
fn no_pattern_page(app: &Rc<App>) -> adw::StatusPage {
    let page = adw::StatusPage::new();
    page.set_icon_name(Some("view-list-symbolic"));
    page.set_title("No Pattern");
    page.set_description(Some("A pattern holds the steps and notes of your beat."));
    page.add_css_class("compact");
    let b = gtk::Button::with_label("New Pattern");
    b.add_css_class("suggested-action");
    b.add_css_class("pill");
    b.set_halign(gtk::Align::Center);
    let a = app.clone();
    b.connect_clicked(move |_| {
        if let Some(p) = a.ensure_pattern() {
            a.select_pattern(p);
        }
    });
    page.set_child(Some(&b));
    page
}

fn build_steps(app: &Rc<App>, channels: &Rc<ChannelList>) -> gtk::Widget {
    let stack = gtk::Stack::new();
    stack.set_vexpand(true);
    stack.set_hexpand(true);
    stack.add_css_class("view");

    // Empty state: no channels.
    let empty = adw::StatusPage::new();
    empty.set_icon_name(Some("audio-x-generic-symbolic"));
    empty.set_title("No Channels Yet");
    empty.set_description(Some(
        "Add a sound to start your beat. You can also drag one in from the sounds list.",
    ));
    empty.add_css_class("compact");
    let add = gtk::MenuButton::new();
    add.set_label("Add Channel");
    add.set_menu_model(Some(&crate::menus::add_channel_menu()));
    add.add_css_class("suggested-action");
    add.add_css_class("pill");
    add.set_halign(gtk::Align::Center);
    add.update_property(&[gtk::accessible::Property::Label("Add channel")]);
    empty.set_child(Some(&add));
    stack.add_named(&empty, Some("empty"));
    stack.add_named(&no_pattern_page(app), Some("nopattern"));

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
    // Names and steps are set apart by space, not by a line.
    left.set_margin_end(12);
    let both = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    both.append(&left);
    both.append(&hscroll);
    let add_row = gtk::MenuButton::new();
    add_row.set_child(Some(
        &adw::ButtonContent::builder()
            .icon_name("list-add-symbolic")
            .label("Add Channel")
            .build(),
    ));
    add_row.set_menu_model(Some(&crate::menus::add_channel_menu()));
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
    install_deselect(app, &vscroll);
    stack.add_named(&vscroll, Some("rows"));
    // WAV files dropped from the file manager become sampler channels.
    crate::samples_ui::install_drop_target(&stack, app);

    let sync = {
        let (a, st) = (app.clone(), stack.clone());
        move || {
            let state = {
                let s = a.session.borrow();
                selection::steps_state(&s.document().project, &a.selection())
            };
            st.set_visible_child_name(match state {
                StepsState::NoChannels => "empty",
                StepsState::NoPattern => "nopattern",
                StepsState::Rows => "rows",
            });
        }
    };
    sync();
    app.on_change(sync);
    stack.upcast()
}

/// Whether a press on `picked` (inside `area`) landed on empty space: no
/// row, button, grid, or other control on the way up.
fn is_empty_space(picked: &gtk::Widget, area: &gtk::Widget) -> bool {
    let mut w = Some(picked.clone());
    while let Some(x) = w {
        if &x == area {
            return true;
        }
        if x.is::<gtk::ListBoxRow>()
            || x.is::<gtk::Button>()
            || x.is::<gtk::Editable>()
            || x.is::<StepGrid>()
            || x.is::<LaneEditor>()
            || x.is::<gtk::Scrollbar>()
        {
            return false;
        }
        w = x.parent();
    }
    false
}

/// Escape, or a click on empty space, clears the channel selection.
fn install_deselect(app: &Rc<App>, area: &gtk::ScrolledWindow) {
    let click = gtk::GestureClick::new();
    click.set_button(gtk::gdk::BUTTON_PRIMARY);
    let (a, ar) = (app.clone(), area.clone());
    click.connect_released(move |_, n, x, y| {
        if n != 1 {
            return;
        }
        let picked = ar.pick(x, y, gtk::PickFlags::DEFAULT);
        if picked.is_some_and(|p| is_empty_space(&p, ar.upcast_ref())) {
            a.deselect_channel();
        }
    });
    area.add_controller(click);
    let keys = gtk::EventControllerKey::new();
    // Before the focused row or grid: neither uses a plain Escape.
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let a = app.clone();
    keys.connect_key_pressed(move |c, key, _, state| {
        let plain = !state.intersects(
            gtk::gdk::ModifierType::SHIFT_MASK
                | gtk::gdk::ModifierType::CONTROL_MASK
                | gtk::gdk::ModifierType::ALT_MASK,
        );
        if key != gtk::gdk::Key::Escape || !plain {
            return glib::Propagation::Proceed;
        }
        // An entry being edited handles its own Escape first.
        let editing = c
            .widget()
            .and_then(|w| w.root())
            .and_then(|r| r.focus())
            .is_some_and(|f| f.is::<gtk::Text>() || f.is::<gtk::Editable>());
        if !editing && a.deselect_channel() {
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
    });
    area.add_controller(keys);
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

/// The notes page and the buttons the Pattern page wires up.
struct NotesPage {
    page: adw::NavigationPage,
    back: gtk::Button,
}

fn build_notes(app: &Rc<App>) -> (PianoRoll, NotesPage, gtk::DropDown) {
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
    let title = gtk::Label::new(None);
    title.add_css_class("heading");
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    title.set_xalign(0.0);
    title.set_hexpand(true);
    title.set_accessible_role(gtk::AccessibleRole::Heading);
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
        title.upcast_ref(),
        snap.upcast_ref(),
        zoom_out.upcast_ref(),
        zoom_in.upcast_ref(),
        vel.upcast_ref(),
    ] {
        tools.append(w);
    }

    let roll = PianoRoll::new(app.clone());
    let grid = gtk::Grid::new();
    let vs = gtk::Scrollbar::new(gtk::Orientation::Vertical, Some(&roll.vadj()));
    let hs = gtk::Scrollbar::new(gtk::Orientation::Horizontal, Some(&roll.hadj()));
    grid.attach(&roll, 0, 0, 1, 1);
    grid.attach(&vs, 1, 0, 1, 1);
    grid.attach(&hs, 0, 1, 1, 1);
    let hint = gtk::Label::new(Some("Click to add notes"));
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
    stack.add_named(&no_pattern_page(app), Some("nopattern"));
    stack.set_vexpand(true);
    stack.add_css_class("view");

    let view = adw::ToolbarView::new();
    view.add_top_bar(&tools);
    view.set_content(Some(&stack));
    let page = adw::NavigationPage::with_tag(&view, "Notes", NOTES_TAG);

    {
        let r = roll.clone();
        snap.connect_selected_notify(move |d| r.set_snap_index(d.selected() as usize));
        let r = roll.clone();
        zoom_in.connect_clicked(move |_| r.zoom_x(1.3));
        let r = roll.clone();
        zoom_out.connect_clicked(move |_| r.zoom_x(1.0 / 1.3));
        let r = roll.clone();
        vel.connect_toggled(move |b| r.set_velocity_lane(b.is_active()));
    }

    let sync = {
        let (a, stack, hint, page, title) = (
            app.clone(),
            stack.clone(),
            hint.clone(),
            page.clone(),
            title.clone(),
        );
        move || {
            let sel = a.selection();
            let (state, show_hint, name) = {
                let s = a.session.borrow();
                let p = &s.document().project;
                (
                    selection::notes_state(&sel),
                    selection::show_notes_hint(p, &sel),
                    sel.channel
                        .and_then(|id| p.channel(id))
                        .map(|c| c.name.clone()),
                )
            };
            stack.set_visible_child_name(match state {
                NotesState::NoPattern => "nopattern",
                NotesState::PickChannel => "none",
                NotesState::Roll => "roll",
            });
            hint.set_visible(state == NotesState::Roll && show_hint);
            // The page is named after the channel it edits.
            let name = name.unwrap_or_else(|| "Notes".to_string());
            if page.title() != name {
                page.set_title(&name);
            }
            if title.text() != name {
                title.set_text(&name);
            }
        }
    };
    sync();
    app.on_change(sync);
    (roll, NotesPage { page, back }, snap)
}
