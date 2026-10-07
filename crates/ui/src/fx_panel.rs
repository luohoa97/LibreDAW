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
        Insert::Builtin { instance, fx } if *instance == inst => Some(fx.clone()),
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
