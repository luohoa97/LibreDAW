// SPDX-License-Identifier: GPL-3.0-or-later
//! The panel of a built-in effect (SPEC 24.2, 20.6): a pick of ready-made
//! styles first, an On switch where the effect has a Mix, and the knobs
//! behind More. It opens as a popover from the effect's name in the mixer
//! strip. Every change is an edit (`SetFxParam` and friends), so it can be
//! undone and is heard at once through the parameter table.

use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;

use control::fxpresets;
use control::mcp::compose::BuildResult;
use control::mcp::ids::IdGen;
use protocol::beats::{
    BuiltinFx, BuiltinFxKind, CompressorParam, DelayParam, ReverbParam, SaturatorParam,
};
use protocol::edit::Edit;
use protocol::ids::{InstanceId, TrackId};
use protocol::model::Insert;

use crate::app::App;
use crate::menus;
use crate::native_logic::fx_specs;
use crate::native_panel::ParamPanel;

/// Table index of the Mix knob, for the effects that have one.
pub fn mix_index(kind: BuiltinFxKind) -> Option<usize> {
    match kind {
        BuiltinFxKind::Compressor => Some(CompressorParam::Mix.index()),
        BuiltinFxKind::Saturator => Some(SaturatorParam::Mix.index()),
        BuiltinFxKind::Reverb => Some(ReverbParam::Mix.index()),
        BuiltinFxKind::Delay => Some(DelayParam::Mix.index()),
        BuiltinFxKind::Eq | BuiltinFxKind::Limiter => None,
    }
}

/// The effect, read from the document.
fn current(app: &App, track: TrackId, inst: InstanceId) -> Option<BuiltinFx> {
    let s = app.session.borrow();
    let t = s.document().project.track(track)?.clone();
    t.inserts.iter().find_map(|i| match i {
        Insert::Builtin { instance, fx, .. } if *instance == inst => Some(fx.clone()),
        _ => None,
    })
}

/// Edits for switching an effect on or off. Off writes Mix 0 (and a
/// Saturator's output back to 0 dB), so the sound passes unchanged. `kept`
/// holds the values to bring back.
pub fn power_edits(
    track: TrackId,
    inst: InstanceId,
    fx: &BuiltinFx,
    on: bool,
    kept: &Cell<Option<(f64, f64)>>,
) -> Vec<Edit> {
    let Some(mix) = mix_index(fx.kind()) else {
        return Vec::new();
    };
    let set = |param: usize, value: f64| Edit::SetFxParam {
        track,
        instance: inst,
        param: param as u8,
        value,
    };
    let out = SaturatorParam::OutputDb.index();
    let is_sat = fx.kind() == BuiltinFxKind::Saturator;
    if on {
        let d = BuiltinFx::new(fx.kind());
        let (m, o) = kept
            .take()
            .unwrap_or((d.param(mix).unwrap_or(1.0), d.param(out).unwrap_or(0.0)));
        let mut e = vec![set(mix, m)];
        if is_sat {
            e.push(set(out, o));
        }
        e
    } else {
        kept.set(Some((
            fx.param(mix).unwrap_or(1.0),
            fx.param(out).unwrap_or(0.0),
        )));
        let mut e = vec![set(mix, 0.0)];
        if is_sat {
            e.push(set(out, 0.0));
        }
        e
    }
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
    power.set_visible(mix_index(kind).is_some());
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
    let kept: Rc<Cell<Option<(f64, f64)>>> = Rc::new(Cell::new(None));

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
            if let Some(m) = mix_index(fx.kind()) {
                power.set_active(fx.param(m).unwrap_or(0.0) > 0.0);
            }
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
        let (a, updating, refresh, kept) = (a.clone(), updating.clone(), refresh.clone(), kept);
        power.connect_active_notify(move |s| {
            if updating.get() {
                return;
            }
            let Some(fx) = current(&a, track, inst) else {
                return;
            };
            let edits = power_edits(track, inst, &fx, s.is_active(), &kept);
            if !edits.is_empty() {
                a.edit(edits);
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
    fn off_and_on_bring_the_values_back() {
        let mut fx = BuiltinFx::new(BuiltinFxKind::Saturator);
        fx.set_param(SaturatorParam::OutputDb.index(), -8.8);
        fx.set_param(SaturatorParam::Mix.index(), 0.7);
        let kept = Cell::new(None);
        let off = power_edits(TrackId(1), InstanceId(2), &fx, false, &kept);
        assert_eq!(off.len(), 2);
        let on = power_edits(TrackId(1), InstanceId(2), &fx, true, &kept);
        match (&on[0], &on[1]) {
            (Edit::SetFxParam { value: m, .. }, Edit::SetFxParam { value: o, .. }) => {
                assert_eq!((*m, *o), (0.7, -8.8))
            }
            _ => panic!("{on:?}"),
        }
    }

    #[test]
    fn effects_without_a_mix_have_no_switch() {
        assert!(mix_index(BuiltinFxKind::Eq).is_none());
        assert!(mix_index(BuiltinFxKind::Limiter).is_none());
        let fx = BuiltinFx::new(BuiltinFxKind::Eq);
        assert!(power_edits(TrackId(1), InstanceId(2), &fx, false, &Cell::new(None)).is_empty());
    }
}
