// SPDX-License-Identifier: GPL-3.0-or-later
//! The clip editor docked under the Timeline (SPEC 20.3): one editor, two
//! views of the selected clip's content. Steps (the step row with its
//! velocity, pitch and ratchet lanes) suits drums; Notes (the piano roll)
//! suits tonal instruments. The default follows the instrument; the user's
//! choice is kept per instrument for the session.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use protocol::ids::ChannelId;
use protocol::model::{Instrument, Project};

use crate::app::App;
use crate::lane_logic::Lane;
use crate::shortcuts;
use crate::view_math::SNAPS;
use crate::widgets::lane_editor::LaneEditor;
use crate::widgets::piano_roll::PianoRoll;
use crate::widgets::step_grid::StepGrid;
use doc::presets;

/// Which view of a clip the editor shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Steps,
    Notes,
}

/// The view an instrument opens in: Steps for drums (samplers and the
/// built-in drum sounds), Notes for everything tonal.
pub fn default_mode(p: &Project, instrument: ChannelId) -> Mode {
    let Some(c) = p.channel(instrument) else {
        return Mode::Notes;
    };
    match &c.instrument {
        Instrument::Sampler(_) => Mode::Steps,
        Instrument::Synth(params) => match presets::index_of(params) {
            Some(i) if presets::presets()[i].role == "Drum" => Mode::Steps,
            _ => Mode::Notes,
        },
        // An audio row has no editor (SPEC 21.1): sounds show a waveform on the timeline.
        Instrument::Bass808(_) | Instrument::Clap(_) | Instrument::Audio => Mode::Notes,
    }
}

/// The text over the editor: "Kick 1 - Kick", and how many clips share it.
pub fn heading(p: &Project, pattern: protocol::ids::PatternId) -> (String, usize) {
    let Some(pat) = p.pattern(pattern) else {
        return (String::new(), 0);
    };
    let inst = p
        .channel(pat.instrument)
        .map(|c| c.name.clone())
        .unwrap_or_default();
    let linked = crate::selection::linked_count(p, pattern);
    (format!("{} - {inst}", pat.name), linked)
}

pub struct ClipEditor {
    pub widget: gtk::Widget,
    pub grid: StepGrid,
    pub roll: PianoRoll,
    pub snap: gtk::DropDown,
    mode_of: RefCell<HashMap<ChannelId, Mode>>,
    stack: gtk::Stack,
    steps_btn: gtk::ToggleButton,
    notes_btn: gtk::ToggleButton,
}

fn flat_button(icon: &str, tip: &str) -> gtk::Button {
    let b = gtk::Button::from_icon_name(icon);
    b.add_css_class("flat");
    b.set_tooltip_text(Some(tip));
    b.update_property(&[gtk::accessible::Property::Label(tip)]);
    b
}

impl ClipEditor {
    pub fn new(app: &Rc<App>) -> Rc<ClipEditor> {
        // ---- toolbar ----
        let bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        bar.add_css_class("toolbar");
        let title = gtk::Label::new(None);
        title.add_css_class("heading");
        title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        title.set_xalign(0.0);
        title.set_accessible_role(gtk::AccessibleRole::Heading);
        let views = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        views.add_css_class("linked");
        let steps_btn = gtk::ToggleButton::with_label("Grid");
        steps_btn.set_tooltip_text(Some("Click squares to place hits"));
        let notes_btn = gtk::ToggleButton::with_label("Piano");
        notes_btn.set_tooltip_text(Some("Draw notes; higher is higher pitch"));
        notes_btn.set_group(Some(&steps_btn));
        views.append(&steps_btn);
        views.append(&notes_btn);
        let unique = gtk::Button::with_label("Edit Separately");
        unique.add_css_class("flat");
        unique.set_tooltip_text(Some(
            "This clip changes together with its copies; give it its own notes",
        ));
        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        spacer.set_hexpand(true);
        // Notes view tools.
        let labels: Vec<&str> = SNAPS.iter().map(|s| s.0).collect();
        let snap = gtk::DropDown::from_strings(&labels);
        let snap_tip = "Snap to Grid: notes you draw line up to this length";
        snap.set_tooltip_text(Some(snap_tip));
        snap.update_property(&[gtk::accessible::Property::Label(snap_tip)]);
        let snap_label = gtk::Label::new(Some("Snap to Grid"));
        snap_label.add_css_class("dim-label");
        let zoom_out = flat_button(
            "zoom-out-symbolic",
            &shortcuts::tooltip("Zoom Out", "win.zoom-out"),
        );
        let zoom_in = flat_button(
            "zoom-in-symbolic",
            &shortcuts::tooltip("Zoom In", "win.zoom-in"),
        );
        let vel = gtk::ToggleButton::with_label("Volume");
        vel.add_css_class("flat");
        vel.set_active(true);
        vel.set_tooltip_text(Some("Show how loud each note is"));
        let notes_tools = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        for w in [
            snap_label.upcast_ref::<gtk::Widget>(),
            snap.upcast_ref(),
            zoom_out.upcast_ref(),
            zoom_in.upcast_ref(),
            vel.upcast_ref(),
        ] {
            notes_tools.append(w);
        }
        // Grid view: the per-hit lanes and swing sit behind "More" (SPEC
        // 20.6), in a row under the toolbar.
        let more_tip = "Show more: the volume, pitch and repeats of each hit, and swing";
        let more = gtk::ToggleButton::with_label("More");
        more.add_css_class("flat");
        more.set_tooltip_text(Some(more_tip));
        more.update_property(&[gtk::accessible::Property::Label(more_tip)]);
        let lanes = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        lanes.add_css_class("linked");
        let lanes_tip = "Change for each hit";
        lanes.update_property(&[gtk::accessible::Property::Label(lanes_tip)]);
        let lanes_label = gtk::Label::new(Some("Change for each hit"));
        lanes_label.add_css_class("dim-label");
        let swing_tip = "Swing: plays every second hit a little late, for a laid-back groove";
        let swing_label = gtk::Label::new(Some("Swing"));
        swing_label.add_css_class("dim-label");
        let swing = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 75.0, 1.0);
        swing.set_width_request(140);
        swing.set_draw_value(true);
        swing.set_value_pos(gtk::PositionType::Right);
        swing.set_format_value_func(|_, v| format!("{}%", v.round() as i32));
        swing.add_mark(0.0, gtk::PositionType::Bottom, None);
        swing.add_mark(50.0, gtk::PositionType::Bottom, None);
        swing.set_tooltip_text(Some(swing_tip));
        swing.update_property(&[gtk::accessible::Property::Label(swing_tip)]);
        let more_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        more_row.add_css_class("toolbar");
        swing_label.set_margin_start(12);
        for w in [
            lanes_label.upcast_ref::<gtk::Widget>(),
            lanes.upcast_ref(),
            swing_label.upcast_ref(),
            swing.upcast_ref(),
        ] {
            more_row.append(w);
        }
        let more_reveal = gtk::Revealer::new();
        more_reveal.set_child(Some(&more_row));
        more.bind_property("active", &more_reveal, "reveal-child")
            .sync_create()
            .build();
        let close = flat_button("window-close-symbolic", "Close the Editor (Escape)");
        for w in [
            title.upcast_ref::<gtk::Widget>(),
            views.upcast_ref(),
            unique.upcast_ref(),
            spacer.upcast_ref(),
            more.upcast_ref(),
            notes_tools.upcast_ref(),
            close.upcast_ref(),
        ] {
            bar.append(w);
        }
        let bar_scroll = gtk::ScrolledWindow::new();
        bar_scroll.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Never);
        bar_scroll.set_propagate_natural_height(true);
        bar_scroll.set_min_content_width(0);
        bar_scroll.set_child(Some(&bar));
        // Swing of the clip's notes, one undo step per gesture.
        let swing_guard = Rc::new(std::cell::Cell::new(false));
        {
            let (a, g) = (app.clone(), swing_guard.clone());
            swing.connect_value_changed(move |s| {
                if g.get() {
                    return;
                }
                let Some(p) = a.current_pattern() else { return };
                let want = (s.value().round() as u16 * 10).min(protocol::consts::MAX_SWING);
                a.edit_resting(
                    "Swing",
                    vec![protocol::edit::Edit::SetSwing {
                        pattern: p,
                        swing: want,
                    }],
                );
            });
        }

        // ---- Steps ----
        let grid = StepGrid::new(app.clone());
        let lane = LaneEditor::new(app.clone());
        let lane_buttons: Vec<(Lane, gtk::ToggleButton)> = Lane::ALL
            .iter()
            .map(|l| {
                let b = gtk::ToggleButton::with_label(l.label());
                b.set_tooltip_text(Some(l.tooltip()));
                b.update_property(&[gtk::accessible::Property::Label(l.tooltip())]);
                lanes.append(&b);
                (*l, b)
            })
            .collect();
        {
            let guard = Rc::new(std::cell::Cell::new(false));
            for (l, b) in &lane_buttons {
                let (editor, all, guard, l) =
                    (lane.clone(), lane_buttons.clone(), guard.clone(), *l);
                b.connect_toggled(move |me| {
                    if guard.replace(true) {
                        return;
                    }
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
        }
        let steps_box = gtk::Box::new(gtk::Orientation::Vertical, 8);
        steps_box.set_margin_start(12);
        steps_box.set_margin_end(12);
        steps_box.set_margin_top(6);
        steps_box.append(&grid);
        steps_box.append(&lane);
        let steps_scroll = gtk::ScrolledWindow::new();
        steps_scroll.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
        steps_scroll.set_child(Some(&steps_box));

        // ---- Notes ----
        let roll = PianoRoll::new(app.clone());
        let rgrid = gtk::Grid::new();
        let vs = gtk::Scrollbar::new(gtk::Orientation::Vertical, Some(&roll.vadj()));
        let hs = gtk::Scrollbar::new(gtk::Orientation::Horizontal, Some(&roll.hadj()));
        rgrid.attach(&roll, 0, 0, 1, 1);
        rgrid.attach(&vs, 1, 0, 1, 1);
        rgrid.attach(&hs, 0, 1, 1, 1);
        let hint = gtk::Label::new(Some(
            "Click anywhere to draw a note; higher is higher pitch",
        ));
        hint.add_css_class("dim-label");
        hint.set_can_target(false);
        hint.set_halign(gtk::Align::Center);
        hint.set_valign(gtk::Align::Center);
        let notes_overlay = gtk::Overlay::new();
        notes_overlay.set_child(Some(&rgrid));
        notes_overlay.add_overlay(&hint);

        let stack = gtk::Stack::new();
        let grid_hint = gtk::Label::new(Some("Click a square to add a drum hit"));
        grid_hint.add_css_class("dim-label");
        grid_hint.set_can_target(false);
        grid_hint.set_halign(gtk::Align::Center);
        grid_hint.set_valign(gtk::Align::End);
        grid_hint.set_margin_bottom(24);
        let steps_overlay = gtk::Overlay::new();
        steps_overlay.set_child(Some(&steps_scroll));
        steps_overlay.add_overlay(&grid_hint);
        stack.add_named(&steps_overlay, Some("steps"));
        stack.add_named(&notes_overlay, Some("notes"));
        stack.set_vexpand(true);
        stack.add_css_class("view");

        let view = adw::ToolbarView::new();
        view.add_top_bar(&bar_scroll);
        view.add_top_bar(&more_reveal);
        view.set_content(Some(&stack));

        let me = Rc::new(ClipEditor {
            widget: view.upcast(),
            grid,
            roll: roll.clone(),
            snap: snap.clone(),
            mode_of: RefCell::new(HashMap::new()),
            stack: stack.clone(),
            steps_btn: steps_btn.clone(),
            notes_btn: notes_btn.clone(),
        });

        // ---- wiring ----
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
            close.connect_clicked(move |_| {
                a.deselect_clip();
            });
            let a = app.clone();
            unique.connect_clicked(move |_| {
                if let Some(c) = a.current_clip() {
                    crate::clip_ops::make_unique(&a, &[c]);
                }
            });
        }
        for (btn, mode) in [(&steps_btn, Mode::Steps), (&notes_btn, Mode::Notes)] {
            let (m, a) = (Rc::downgrade(&me), app.clone());
            btn.connect_toggled(move |b| {
                if !b.is_active() {
                    return;
                }
                let Some(m) = m.upgrade() else { return };
                if let Some(ch) = a.current_channel() {
                    m.mode_of.borrow_mut().insert(ch, mode);
                }
                m.show(mode);
            });
        }
        let sync = {
            let (m, a) = (Rc::downgrade(&me), app.clone());
            let (title, unique, more, notes_tools, hint, grid_hint, swing, swing_guard) = (
                title.clone(),
                unique.clone(),
                more.clone(),
                notes_tools.clone(),
                hint.clone(),
                grid_hint.clone(),
                swing.clone(),
                swing_guard.clone(),
            );
            move || {
                let Some(m) = m.upgrade() else { return };
                let sel = a.selection();
                let (text, linked, mode, empty) = {
                    let s = a.session.borrow();
                    let p = &s.document().project;
                    let Some(pid) = sel.pattern else {
                        return;
                    };
                    let (text, linked) = heading(p, pid);
                    let inst = p.pattern(pid).map(|pt| pt.instrument);
                    let mode = inst.map(|i| {
                        m.mode_of
                            .borrow()
                            .get(&i)
                            .copied()
                            .unwrap_or_else(|| default_mode(p, i))
                    });
                    (
                        text,
                        linked,
                        mode.unwrap_or(Mode::Notes),
                        crate::selection::show_notes_hint(p, &sel),
                    )
                };
                if title.text() != text {
                    title.set_text(&text);
                }
                unique.set_visible(linked > 1);
                unique.set_tooltip_text(Some(&format!(
                    "This clip changes together with {} other clips; give it its own notes",
                    linked - 1
                )));
                let steps = mode == Mode::Steps;
                if steps != m.steps_btn.is_active() {
                    m.steps_btn.set_active(steps);
                    m.notes_btn.set_active(!steps);
                }
                m.show(mode);
                more.set_visible(steps);
                if !steps {
                    more.set_active(false);
                }
                notes_tools.set_visible(!steps);
                hint.set_visible(!steps && empty);
                grid_hint.set_visible(steps && empty);
                let swing_now = {
                    let s = a.session.borrow();
                    sel.pattern
                        .and_then(|id| s.document().project.pattern(id))
                        .map(|p| p.swing as f64 / 10.0)
                        .unwrap_or(0.0)
                };
                if (swing.value() - swing_now).abs() > 0.5 {
                    swing_guard.set(true);
                    swing.set_value(swing_now);
                    swing_guard.set(false);
                }
            }
        };
        sync();
        app.on_change(sync);
        me
    }

    fn show(&self, mode: Mode) {
        let name = if mode == Mode::Steps {
            "steps"
        } else {
            "notes"
        };
        if self.stack.visible_child_name().as_deref() != Some(name) {
            self.stack.set_visible_child_name(name);
        }
    }

    /// Moves the keyboard into the editor (after "Edit Clip").
    pub fn focus(&self) {
        let (grid, roll, steps) = (
            self.grid.clone(),
            self.roll.clone(),
            self.steps_btn.is_active(),
        );
        glib::idle_add_local_once(move || {
            if steps {
                grid.grab_focus();
            } else {
                roll.reveal();
                roll.grab_focus();
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use doc::document::{Document, apply_batch};
    use protocol::edit::{Edit, NewInstrument};
    use protocol::ids::TrackId;

    #[test]
    fn drums_open_as_steps_and_tonal_sounds_as_notes() {
        let kick = presets::presets()
            .into_iter()
            .find(|p| p.role == "Drum")
            .unwrap();
        let lead = presets::presets()
            .into_iter()
            .find(|p| p.role == "Melody")
            .unwrap();
        let (d, ids) = apply_batch(
            &Document::new(),
            &[
                Edit::AddChannel {
                    name: "Kick".into(),
                    instrument: NewInstrument::Synth {
                        params: kick.params,
                    },
                    root_key: 36,
                    track: TrackId::MASTER,
                },
                Edit::AddChannel {
                    name: "Lead".into(),
                    instrument: NewInstrument::Synth {
                        params: lead.params,
                    },
                    root_key: 60,
                    track: TrackId::MASTER,
                },
                Edit::AddChannel {
                    name: "808".into(),
                    instrument: NewInstrument::Bass808 { mono: true },
                    root_key: 36,
                    track: TrackId::MASTER,
                },
            ],
        )
        .unwrap();
        let p = &d.project;
        assert_eq!(default_mode(p, ChannelId(ids[0])), Mode::Steps);
        assert_eq!(default_mode(p, ChannelId(ids[1])), Mode::Notes);
        assert_eq!(default_mode(p, ChannelId(ids[2])), Mode::Notes);
        assert_eq!(default_mode(p, ChannelId(9999)), Mode::Notes);
    }
}
