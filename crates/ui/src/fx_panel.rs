// SPDX-License-Identifier: GPL-3.0-or-later
//! The panel of a built-in effect (SPEC 24.2, 20.6): a pick of ready-made
//! styles first, an On switch (every effect has one), and the knobs
//! behind More. It opens as a popover from the effect's name in the mixer
//! strip. Every change is an edit (`SetFxParam` and friends), so it can be
//! undone and is heard at once through the parameter table.

use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;

use control::fxpresets;
use control::mcp::compose::BuildResult;
use control::mcp::ids::IdGen;
use protocol::beats::BuiltinFx;
use protocol::edit::Edit;
use protocol::ids::{InstanceId, TrackId};
use protocol::model::Insert;

use crate::app::App;
use crate::menus;
use crate::native_logic::fx_specs;
use crate::native_panel::ParamPanel;

/// The effect, read from the document.
fn current(app: &App, track: TrackId, inst: InstanceId) -> Option<BuiltinFx> {
    let s = app.session.borrow();
    let t = s.document().project.track(track)?.clone();
    t.inserts.iter().find_map(|i| match i {
        Insert::Builtin { instance, fx, .. } if *instance == inst => Some(fx.clone()),
        _ => None,
    })
}

/// The edit for the On switch: the effect is skipped while it is off
/// (SetInsertBypass), whatever its settings (24.1).
pub fn power_edit(track: TrackId, inst: InstanceId, on: bool) -> Edit {
    Edit::SetInsertBypass {
        track,
        instance: inst,
        bypass: !on,
    }
}

/// Whether the effect is on (not bypassed).
fn is_on(app: &App, track: TrackId, inst: InstanceId) -> bool {
    let s = app.session.borrow();
    s.document()
        .project
        .track(track)
        .and_then(|t| {
            t.inserts.iter().find_map(|i| match i {
                Insert::Builtin {
                    instance, bypass, ..
                } if *instance == inst => Some(!*bypass),
                _ => None,
            })
        })
        .unwrap_or(true)
}

/// Position in the style list: 0 is Custom, then each ready-made style.
fn style_position(fx: &BuiltinFx) -> u32 {
    fxpresets::current(fx)
        .and_then(|c| {
            fxpresets::presets(fx.kind())
                .iter()
                .position(|s| s.name == c.name)
        })
        .map_or(0, |i| i as u32 + 1)
}

/// The panel's content for one effect.
pub fn build(app: &Rc<App>, track: TrackId, inst: InstanceId) -> Option<gtk::Widget> {
    let fx = current(app, track, inst)?;
    let kind = fx.kind();
    let (name, what) = menus::effect_name(kind);
    let col = gtk::Box::new(gtk::Orientation::Vertical, 8);
    col.set_margin_top(10);
    col.set_margin_bottom(10);
    col.set_margin_start(10);
    col.set_margin_end(10);

    let head = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let title = gtk::Label::new(Some(name));
    title.add_css_class("heading");
    title.set_xalign(0.0);
    title.set_hexpand(true);
    title.set_tooltip_text(Some(what));
    head.append(&title);
    let power = gtk::Switch::new();
    power.set_valign(gtk::Align::Center);
    let tip = format!("Turn {name} on or off");
    power.set_tooltip_text(Some(&tip));
    power.update_property(&[gtk::accessible::Property::Label(&tip)]);
    head.append(&power);
    col.append(&head);

    // Styles first; "Custom" stands for values that match none of them.
    let styles = fxpresets::presets(kind);
    let mut names: Vec<&str> = vec!["Custom"];
    names.extend(styles.iter().map(|s| s.name));
    let pick = gtk::DropDown::from_strings(&names);
    pick.set_tooltip_text(Some("Pick a ready-made setting for this effect"));
    pick.update_property(&[gtk::accessible::Property::Label(&format!(
        "Style of {name}"
    ))]);
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let cap = gtk::Label::new(Some("Style"));
    cap.add_css_class("dim-label");
    row.append(&cap);
    pick.set_hexpand(true);
    row.append(&pick);
    col.append(&row);

    let updating = Rc::new(Cell::new(false));

    let a = app.clone();
    let panel = ParamPanel::new(fx_specs(kind), 0, 3, name, {
        let (a, updating, pick) = (a.clone(), updating.clone(), pick.clone());
        move |i, value| {
            if updating.get() {
                return;
            }
            let edit = Edit::SetFxParam {
                track,
                instance: inst,
                param: i as u8,
                value,
            };
            a.edit_resting("Change effect", vec![edit]);
            // Knob moves keep the style pick honest.
            if let Some(fx) = current(&a, track, inst) {
                updating.set(true);
                pick.set_selected(style_position(&fx));
                updating.set(false);
            }
        }
    });
    let more = gtk::Expander::new(Some("More"));
    more.set_child(Some(&panel.widget));
    col.append(&more);

    // Shows the document's values without sending edits.
    let refresh: Rc<dyn Fn()> = {
        let (a, panel, pick, power, updating) = (
            a.clone(),
            panel.clone(),
            pick.clone(),
            power.clone(),
            updating.clone(),
        );
        Rc::new(move || {
            let Some(fx) = current(&a, track, inst) else {
                return;
            };
            updating.set(true);
            panel.set_values(|i| fx.param(i).unwrap_or(0.0));
            let at = fxpresets::current(&fx)
                .and_then(|c| styles.iter().position(|s| s.name == c.name))
                .map_or(0, |i| i as u32 + 1);
            pick.set_selected(at);
            power.set_active(is_on(&a, track, inst));
            updating.set(false);
        })
    };
    refresh();

    {
        let (a, updating, refresh) = (a.clone(), updating.clone(), refresh.clone());
        pick.connect_selected_notify(move |d| {
            let i = d.selected() as usize;
            if updating.get() || i == 0 {
                return;
            }
            let Some(fx) = current(&a, track, inst) else {
                return;
            };
            let edits = fxpresets::edits(track, inst, &fx, &styles[i - 1]);
            if !edits.is_empty() {
                a.edit(edits);
            }
            refresh();
        });
    }
    {
        let (a, updating, refresh) = (a.clone(), updating.clone(), refresh.clone());
        power.connect_active_notify(move |s| {
            if updating.get() {
                return;
            }
            if is_on(&a, track, inst) != s.is_active() {
                a.edit(vec![power_edit(track, inst, s.is_active())]);
            }
            refresh();
        });
    }
    Some(col.upcast())
}

/// Builds a batch from the current project with `build` and applies it as
/// one undo step. The window and the tools build edits with the same
/// functions (`control::mcp::fxchain`). Errors appear as a toast.
pub fn run(
    app: &Rc<App>,
    build: impl FnOnce(&protocol::model::Project, &mut IdGen) -> BuildResult,
) -> bool {
    let built = {
        let s = app.session.borrow();
        let d = s.document();
        build(&d.project, &mut IdGen::new(&d.project, Some(d.next_id)))
    };
    match built {
        Ok(b) if b.edits.is_empty() => true,
        Ok(b) => app.edit(b.edits).is_some(),
        Err(m) => {
            app.toast(&m);
            false
        }
    }
}

/// As `run`, but a burst of calls (a slider drag) is one undo step.
pub fn run_resting(
    app: &Rc<App>,
    description: &str,
    build: impl FnOnce(&protocol::model::Project, &mut IdGen) -> BuildResult,
) {
    let built = {
        let s = app.session.borrow();
        let d = s.document();
        build(&d.project, &mut IdGen::new(&d.project, Some(d.next_id)))
    };
    match built {
        Ok(b) if b.edits.is_empty() => {}
        Ok(b) => app.edit_resting(description, b.edits),
        Err(m) => app.toast(&m),
    }
}

/// "Duck to Kick" on a mixer track: a switch and an amount. `None` when
/// the track is Main Output or there is no Kick row to duck to.
pub fn duck_row(
    app: &Rc<App>,
    project: &protocol::model::Project,
    track: TrackId,
) -> Option<gtk::Widget> {
    use control::mcp::fxchain::{self, DuckArgs};
    if track == TrackId::MASTER || fxchain::find_kick(project, track).is_none() {
        return None;
    }
    let state = project
        .track(track)
        .and_then(|t| fxchain::duck_state(project, t));
    let col = gtk::Box::new(gtk::Orientation::Vertical, 2);
    let head = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let cap = gtk::Label::new(Some("Duck to Kick"));
    cap.add_css_class("caption-heading");
    cap.set_xalign(0.0);
    cap.set_hexpand(true);
    head.append(&cap);
    let on = gtk::Switch::new();
    on.set_valign(gtk::Align::Center);
    on.set_tooltip_text(Some(
        "Makes this sound dip every time the kick hits, so the kick cuts through",
    ));
    on.update_property(&[gtk::accessible::Property::Label("Duck to Kick")]);
    head.append(&on);
    col.append(&head);
    let amount = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 100.0, 5.0);
    amount.set_draw_value(false);
    amount.set_tooltip_text(Some("How far this sound dips when the kick hits"));
    amount.update_property(&[gtk::accessible::Property::Label("Duck amount")]);
    col.append(&amount);

    let updating = Rc::new(Cell::new(true));
    on.set_active(state.is_some());
    amount.set_value(state.map_or(100.0, |(_, a)| (a * 100.0).round()));
    amount.set_sensitive(state.is_some());
    updating.set(false);

    let args = move |pct: f64| DuckArgs {
        row: None,
        track: Some(track),
        amount: Some(pct),
        kick: None,
    };
    {
        let (a, u, amount) = (app.clone(), updating.clone(), amount.clone());
        on.connect_active_notify(move |s| {
            if u.get() {
                return;
            }
            let pct = if s.is_active() { amount.value() } else { 0.0 };
            amount.set_sensitive(s.is_active());
            run(&a, |p, ids| fxchain::duck_to_kick(p, ids, &args(pct)));
        });
    }
    {
        let (a, u, on) = (app.clone(), updating, on);
        amount.connect_value_changed(move |s| {
            if u.get() || !on.is_active() {
                return;
            }
            let pct = s.value();
            run_resting(&a, "Duck to Kick", |p, ids| {
                fxchain::duck_to_kick(p, ids, &args(pct))
            });
        });
    }
    Some(col.upcast())
}

/// The Loudness control for Main Output: a 0 to 10 amount, the three
/// sounds, and a reading of the last 10 s. `hold(true)` is called while the
/// slider is pressed so the strip is not rebuilt under the pointer, and
/// `hold(false)` when it is let go. Returns the card and its reading.
pub fn loudness_card(
    app: &Rc<App>,
    project: &protocol::model::Project,
    hold: impl Fn(bool) + 'static,
) -> (gtk::Widget, gtk::Label) {
    use control::mcp::fxchain::{self, LOUDNESS_PRESETS, LoudnessArgs};
    let state = project
        .track(TrackId::MASTER)
        .and_then(|t| fxchain::loudness_state(t));
    let col = gtk::Box::new(gtk::Orientation::Vertical, 4);
    let cap = gtk::Label::new(Some("Loudness"));
    cap.add_css_class("caption-heading");
    cap.set_xalign(0.0);
    col.append(&cap);

    let mut names = vec!["Custom", "Clean", "Punchy", "Hard (Phonk)"];
    if state.is_none() {
        names[0] = "Off";
    }
    let pick = gtk::DropDown::from_strings(&names);
    pick.set_tooltip_text(Some("Pick how loud the whole song is"));
    pick.update_property(&[gtk::accessible::Property::Label("Loudness style")]);
    col.append(&pick);
    let amount = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 10.0, 0.5);
    amount.set_draw_value(true);
    amount.set_value_pos(gtk::PositionType::Right);
    amount.set_digits(1);
    amount.set_tooltip_text(Some(
        "Louder and denser from 0 to 10. A limiter keeps the sound from going over the top",
    ));
    amount.update_property(&[gtk::accessible::Property::Label("Loudness")]);
    col.append(&amount);
    let reading = gtk::Label::new(Some("No reading yet"));
    reading.add_css_class("caption");
    reading.add_css_class("dim-label");
    reading.set_xalign(0.0);
    reading.set_tooltip_text(Some("How loud the last 10 seconds were, in LUFS"));
    col.append(&reading);

    let updating = Rc::new(Cell::new(true));
    amount.set_value(state.unwrap_or(0.0));
    let at = |v: f64| {
        LOUDNESS_PRESETS
            .iter()
            .position(|(_, p)| (p - v).abs() < 0.25)
            .map_or(0, |i| i as u32 + 1)
    };
    pick.set_selected(state.map_or(0, at));
    updating.set(false);

    let press = gtk::GestureClick::new();
    press.set_propagation_phase(gtk::PropagationPhase::Capture);
    let hold = Rc::new(hold);
    {
        let h = hold.clone();
        press.connect_pressed(move |_, _, _, _| h(true));
        let h = hold;
        press.connect_released(move |_, _, _, _| h(false));
    }
    amount.add_controller(press);
    {
        let (a, u, pick) = (app.clone(), updating.clone(), pick.clone());
        amount.connect_value_changed(move |s| {
            if u.get() {
                return;
            }
            let v = s.value();
            u.set(true);
            pick.set_selected(at(v));
            u.set(false);
            run_resting(&a, "Loudness", |p, ids| {
                fxchain::loudness(
                    p,
                    ids,
                    &LoudnessArgs {
                        amount: Some(v),
                        preset: None,
                    },
                )
            });
        });
    }
    {
        let (a, u, amount) = (app.clone(), updating, amount);
        pick.connect_selected_notify(move |d| {
            let i = d.selected() as usize;
            if u.get() || i == 0 {
                return;
            }
            let (name, v) = LOUDNESS_PRESETS[i - 1];
            u.set(true);
            amount.set_value(v);
            u.set(false);
            run(&a, |p, ids| {
                fxchain::loudness(
                    p,
                    ids,
                    &LoudnessArgs {
                        amount: None,
                        preset: Some(name.into()),
                    },
                )
            });
        });
    }
    (col.upcast(), reading)
}

/// The reading's text for `lufs`.
pub fn lufs_text(lufs: Option<f64>) -> String {
    match lufs {
        Some(l) => format!("{l:.1} LUFS"),
        None => "No reading yet".into(),
    }
}

/// Opens the panel of an effect from `anchor`.
pub fn show(app: &Rc<App>, anchor: &gtk::Widget, track: TrackId, inst: InstanceId) {
    let Some(content) = build(app, track, inst) else {
        return;
    };
    let pop = gtk::Popover::new();
    pop.set_child(Some(&content));
    pop.set_parent(anchor);
    pop.connect_closed(|p| {
        let p = p.clone();
        gtk::glib::idle_add_local_once(move || p.unparent());
    });
    pop.popup();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_switch_bypasses_without_touching_the_settings() {
        match power_edit(TrackId(1), InstanceId(2), false) {
            Edit::SetInsertBypass { bypass, .. } => assert!(bypass),
            e => panic!("{e:?}"),
        }
        match power_edit(TrackId(1), InstanceId(2), true) {
            Edit::SetInsertBypass { bypass, .. } => assert!(!bypass),
            e => panic!("{e:?}"),
        }
    }
}
