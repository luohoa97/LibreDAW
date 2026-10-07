// SPDX-License-Identifier: GPL-3.0-or-later
//! Shape lanes (SPEC 24.2-1), without GTK: which lane sits under which
//! row, what a shape controls and in which range, how a curve runs from
//! point to point, how points are added, moved and removed, and the edits
//! the lane menu's presets write.

use control::shapes::{self, Preset};
use protocol::consts::PPQ;
use protocol::edit::Edit;
use protocol::ids::{ChannelId, TrackId};
use protocol::model::{Curve, Insert, Instrument, Project, Shape, ShapePoint, ShapeTarget};

use crate::timeline_logic::Row;

/// The instrument whose row a shape sits under: its own for pitch and
/// filter, the first instrument on the track for the rest. `None` for a
/// track without instruments (Main Output); its lane goes at the bottom.
pub fn owner(p: &Project, t: &ShapeTarget) -> Option<ChannelId> {
    match t {
        ShapeTarget::Pitch { instrument } | ShapeTarget::Filter { instrument } => {
            p.channel(*instrument).map(|c| c.id)
        }
        ShapeTarget::Volume { track }
        | ShapeTarget::Pan { track }
        | ShapeTarget::FxParam { track, .. } => {
            p.channels.iter().find(|c| c.track == *track).map(|c| c.id)
        }
    }
}

/// The displayed rows: each instrument, then the lanes of its shapes.
pub fn layout(p: &Project) -> Vec<Row> {
    let mut out = Vec::with_capacity(p.channels.len() + p.shapes.len());
    for c in &p.channels {
        out.push(Row::Instrument(c.id));
        for s in p
            .shapes
            .iter()
            .filter(|s| owner(p, &s.target) == Some(c.id))
        {
            out.push(Row::Shape(s.id));
        }
    }
    for s in p.shapes.iter().filter(|s| owner(p, &s.target).is_none()) {
        out.push(Row::Shape(s.id));
    }
    out
}

/// The own range of an effect parameter.
fn fx_range(p: &Project, t: &ShapeTarget) -> Option<(f64, f64)> {
    let ShapeTarget::FxParam {
        track,
        instance,
        param,
    } = t
    else {
        return None;
    };
    p.track(*track)?.inserts.iter().find_map(|i| match i {
        Insert::Builtin {
            instance: n, fx, ..
        } if n == instance => fx.param_range(*param as usize),
        _ => None,
    })
}

/// The lowest and highest value of a shape's target.
pub fn range(p: &Project, t: &ShapeTarget) -> (f32, f32) {
    let r = shapes::range_of(t, fx_range(p, t));
    (r.lo, r.hi)
}

/// What a shape is called: "Volume", "Pan", "Pitch", "Filter", or the
/// effect and its setting.
pub fn label(p: &Project, t: &ShapeTarget) -> String {
    match t {
        ShapeTarget::Volume { .. } => "Volume".into(),
        ShapeTarget::Pan { .. } => "Left/Right".into(),
        ShapeTarget::Pitch { .. } => "Pitch".into(),
        ShapeTarget::Filter { .. } => "Filter".into(),
        ShapeTarget::FxParam {
            track,
            instance,
            param,
        } => p
            .track(*track)
            .and_then(|tr| {
                tr.inserts.iter().find_map(|i| match i {
                    Insert::Builtin {
                        instance: n, fx, ..
                    } if n == instance => Some((
                        crate::menus::effect_name(fx.kind()).0,
                        fx.param_name(*param as usize).unwrap_or("setting"),
                    )),
                    _ => None,
                })
            })
            .map(|(e, n)| format!("{e}: {n}"))
            .unwrap_or_else(|| "Effect setting".into()),
    }
}

/// The lane's name in the instrument list: "Volume shape".
pub fn lane_name(p: &Project, s: &Shape) -> String {
    format!("{} shape", label(p, &s.target))
}

/// What the lane says about itself (SPEC 20.6).
pub const TOOLTIP: &str = "A shape: a line that changes this setting over time. Double-click to add a point, drag a point to move it, right-click for more";

/// Every shape a row can get, by name: Volume, Pan, Pitch, Filter, then
/// each setting of the effects on its track. Shapes that exist are left
/// out. The order is the order of the menu's items.
pub fn choices(p: &Project, ch: ChannelId) -> Vec<(String, ShapeTarget)> {
    let Some(c) = p.channel(ch) else {
        return Vec::new();
    };
    let mut all = vec![
        ShapeTarget::Volume { track: c.track },
        ShapeTarget::Pan { track: c.track },
    ];
    // The engine bends the pitch of its own sounds, not of plugins or
    // recorded audio, so those rows are not offered Pitch.
    if !matches!(c.instrument, Instrument::Clap(_) | Instrument::Audio) {
        all.push(ShapeTarget::Pitch { instrument: ch });
    }
    all.push(ShapeTarget::Filter { instrument: ch });
    if let Some(t) = p.track(c.track) {
        for i in &t.inserts {
            if let Insert::Builtin { instance, fx, .. } = i {
                for param in 0..fx.param_count() {
                    all.push(ShapeTarget::FxParam {
                        track: c.track,
                        instance: *instance,
                        param: param as u16,
                    });
                }
            }
        }
    }
    all.into_iter()
        .filter(|t| !p.shapes.iter().any(|s| s.target == *t))
        .map(|t| (label(p, &t), t))
        .collect()
}

/// How many of `choices` are the fixed four (the rest are effect
/// settings).
pub fn basic_count(list: &[(String, ShapeTarget)]) -> usize {
    list.iter()
        .filter(|(_, t)| !matches!(t, ShapeTarget::FxParam { .. }))
        .count()
}

// ---- the curve ----

/// The value of sorted `points` at `tick`: the same rule the engine plays.
pub fn value_at(points: &[ShapePoint], tick: f64) -> Option<f32> {
    let first = points.first()?;
    if tick <= first.tick as f64 {
        return Some(first.value);
    }
    let i = points.partition_point(|p| (p.tick as f64) <= tick);
    if i >= points.len() {
        return points.last().map(|p| p.value);
    }
    let (a, b) = (&points[i - 1], &points[i]);
    let span = (b.tick - a.tick).max(1) as f64;
    let u = ((tick - a.tick as f64) / span).clamp(0.0, 1.0);
    let d = (b.value - a.value) as f64;
    let v = match a.curve {
        Curve::Linear => a.value as f64 + d * u,
        Curve::Smooth => a.value as f64 + d * (u * u * (3.0 - 2.0 * u)),
        Curve::Hold => a.value as f64,
        Curve::Stairs => a.value as f64 + d * (u * 8.0).floor().min(7.0) / 7.0,
        Curve::Pulse => {
            if (u * 4.0).fract() < 0.5 {
                a.value as f64
            } else {
                b.value as f64
            }
        }
        Curve::Wave => a.value as f64 + d * (1.0 - (std::f64::consts::TAU * 4.0 * u).cos()) * 0.5,
    };
    Some(v as f32)
}

/// Where a value sits in a lane `h` tall starting at `top`: the highest
/// value at the top, with `pad` of room above and below.
pub fn value_y(v: f32, (lo, hi): (f32, f32), top: f64, h: f64, pad: f64) -> f64 {
    let span = (hi - lo).max(f32::EPSILON) as f64;
    let u = ((v - lo) as f64 / span).clamp(0.0, 1.0);
    top + pad + (1.0 - u) * (h - 2.0 * pad)
}

/// The value at height `y`: the inverse of `value_y`.
pub fn y_value(y: f64, (lo, hi): (f32, f32), top: f64, h: f64, pad: f64) -> f32 {
    let inner = (h - 2.0 * pad).max(1.0);
    let u = (1.0 - (y - top - pad) / inner).clamp(0.0, 1.0);
    lo + (hi - lo) * u as f32
}

// ---- points ----

/// A point added at `tick` with `value` (or moved onto the same tick as an
/// existing one: the old one is replaced). Returns the points and the new
/// point's place in them.
pub fn with_point(points: &[ShapePoint], tick: u32, value: f32) -> (Vec<ShapePoint>, usize) {
    let mut v: Vec<ShapePoint> = points.iter().copied().filter(|p| p.tick != tick).collect();
    let i = v.partition_point(|p| p.tick < tick);
    // The new point carries the curve of the segment it splits.
    let curve = i
        .checked_sub(1)
        .and_then(|j| v.get(j))
        .map_or(Curve::Smooth, |p| p.curve);
    v.insert(i, ShapePoint { tick, value, curve });
    (v, i)
}

/// Point `i` dragged to `tick` and `value`; it stays between its
/// neighbours so the order never changes while dragging.
pub fn moved(points: &[ShapePoint], i: usize, tick: u32, value: f32) -> Vec<ShapePoint> {
    let mut v = points.to_vec();
    if i >= v.len() {
        return v;
    }
    let lo = i.checked_sub(1).map_or(0, |j| v[j].tick + 1);
    let hi = v.get(i + 1).map_or(u32::MAX, |p| p.tick.saturating_sub(1));
    v[i].tick = tick.clamp(lo, hi.max(lo));
    v[i].value = value;
    v
}

/// The points without point `i`. A shape keeps at least one point.
pub fn without(points: &[ShapePoint], i: usize) -> Vec<ShapePoint> {
    if points.len() <= 1 || i >= points.len() {
        return points.to_vec();
    }
    let mut v = points.to_vec();
    v.remove(i);
    v
}

/// The point whose segment curve is set.
pub fn with_curve(points: &[ShapePoint], i: usize, curve: Curve) -> Vec<ShapePoint> {
    let mut v = points.to_vec();
    if let Some(p) = v.get_mut(i) {
        p.curve = curve;
    }
    v
}

/// The point within `radius` pixels of (`x`, `y`), the nearest one.
pub fn nearest(
    points: &[ShapePoint],
    to_xy: impl Fn(&ShapePoint) -> (f64, f64),
    x: f64,
    y: f64,
    radius: f64,
) -> Option<usize> {
    points
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let (px, py) = to_xy(p);
            (i, ((px - x).powi(2) + (py - y).powi(2)).sqrt())
        })
        .filter(|(_, d)| *d <= radius)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(i, _)| i)
}

/// The curves a point's menu offers, by plain name.
pub const CURVES: &[(&str, Curve)] = &[
    ("Smooth", Curve::Smooth),
    ("Straight", Curve::Linear),
    ("Hold", Curve::Hold),
    ("Staircase", Curve::Stairs),
    ("Pulse", Curve::Pulse),
    ("Wave", Curve::Wave),
];

// ---- presets ----

/// The presets of the lane menu, in order.
pub const PRESETS: &[(&str, Preset)] = &[
    ("Fade In", Preset::FadeIn),
    ("Fade Out", Preset::FadeOut),
    ("Swell", Preset::Swell),
    ("Drop", Preset::Drop),
    ("Pump", Preset::Pump),
    ("Wobble", Preset::Wobble),
    ("Tape Stop", Preset::TapeStop),
];

/// The span a preset covers: the selected range if there is one, else the
/// loop region while it is on, else the four bars from the playhead.
/// Tape Stop is one beat from the start of that span.
pub fn span(p: &Project, selected: Option<(u32, u32)>, playhead: u32) -> (u32, u32) {
    if let Some((a, b)) = selected
        && b > a
    {
        return (a, b);
    }
    let lr = p.loop_region;
    if lr.enabled && lr.end > lr.start {
        return (lr.start, lr.end);
    }
    let bar = protocol::model::ticks_per_bar(p.time_sig_num);
    (playhead, playhead + 4 * bar)
}

/// The points a preset writes over `start..end` for a shape: the points
/// already outside the span stay, those inside are replaced.
pub fn preset_points(
    p: &Project,
    shape: &Shape,
    preset: Preset,
    start: u32,
    end: u32,
) -> Vec<ShapePoint> {
    let r = shapes::range_of(&shape.target, fx_range(p, &shape.target));
    let new = shapes::preset_points(preset, r, start, end);
    let last = new.last().map_or(end, |q| q.tick);
    let mut out: Vec<ShapePoint> = shape
        .points
        .iter()
        .copied()
        .filter(|q| q.tick < start || q.tick > last)
        .collect();
    out.extend(new);
    out.sort_by_key(|q| q.tick);
    out.dedup_by_key(|q| q.tick);
    out
}

/// Applying a preset to a lane: one edit, plus, for Tape Stop on a Pitch
/// lane, the volume shape that goes down with it over the same beat
/// (the shape is made when the row has none).
pub fn preset_edits(p: &Project, shape: &Shape, preset: Preset, start: u32, end: u32) -> Vec<Edit> {
    let mut edits = vec![Edit::SetShapePoints {
        shape: shape.id,
        points: preset_points(p, shape, preset, start, end),
    }];
    if preset == Preset::TapeStop
        && let ShapeTarget::Pitch { instrument } = shape.target
        && let Some(c) = p.channel(instrument)
    {
        let vol = ShapeTarget::Volume { track: c.track };
        let r = shapes::range_of(&vol, None);
        match p.shapes.iter().find(|s| s.target == vol) {
            Some(s) => edits.push(Edit::SetShapePoints {
                shape: s.id,
                points: preset_points(p, s, preset, start, start + PPQ),
            }),
            None => edits.push(Edit::AddShape {
                target: vol,
                points: shapes::preset_points(preset, r, start, start + PPQ),
            }),
        }
    }
    edits
}

/// The track a volume or pan shape of `ch` belongs to.
pub fn track_of(p: &Project, ch: ChannelId) -> Option<TrackId> {
    p.channel(ch).map(|c| c.track)
}

/// The edit that makes a new shape for `target`: a flat line at the
/// target's resting value, so nothing changes until the user draws.
pub fn new_shape_edit(p: &Project, target: ShapeTarget, at: u32) -> Edit {
    let r = shapes::range_of(&target, fx_range(p, &target));
    Edit::AddShape {
        target,
        points: vec![
            ShapePoint {
                tick: at,
                value: r.neutral,
                curve: Curve::Smooth,
            },
            ShapePoint {
                tick: at + 4 * PPQ,
                value: r.neutral,
                curve: Curve::Smooth,
            },
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::beats::{BuiltinFx, BuiltinFxKind};
    use protocol::ids::{InstanceId, ShapeId};
    use protocol::model::{Channel, Instrument, Mix};

    fn pt(tick: u32, value: f32, curve: Curve) -> ShapePoint {
        ShapePoint { tick, value, curve }
    }

    fn project() -> Project {
        let mut p = Project::empty();
        let t = TrackId(1);
        p.tracks.push(std::sync::Arc::new(protocol::model::Track {
            id: t,
            name: "Kick".into(),
            mix: Mix::default(),
            inserts: vec![Insert::Builtin {
                instance: InstanceId(9),
                fx: BuiltinFx::new(BuiltinFxKind::Reverb),
                bypass: false,
            }],
            sends: Vec::new(),
        }));
        for (id, track) in [(1u32, t), (2, TrackId::MASTER)] {
            p.channels.push(std::sync::Arc::new(Channel {
                id: ChannelId(id),
                name: format!("c{id}"),
                instrument: if id == 1 {
                    Instrument::Synth(protocol::model::SynthParams::default())
                } else {
                    Instrument::Audio
                },
                root_key: 60,
                track,
                mix: Mix::default(),
                choke_group: 0,
            }));
        }
        p
    }

    fn shape(id: u32, target: ShapeTarget) -> Shape {
        Shape {
            id: ShapeId(id),
            target,
            points: vec![pt(0, 0.0, Curve::Linear), pt(960, 1.0, Curve::Smooth)],
        }
    }

    #[test]
    fn each_lane_sits_under_its_row() {
        let mut p = project();
        p.shapes.push(shape(
            10,
            ShapeTarget::Pitch {
                instrument: ChannelId(2),
            },
        ));
        p.shapes
            .push(shape(11, ShapeTarget::Volume { track: TrackId(1) }));
        p.shapes
            .push(shape(12, ShapeTarget::Pan { track: TrackId(1) }));
        p.shapes.push(shape(
            13,
            ShapeTarget::Volume {
                track: TrackId::MASTER,
            },
        ));
        assert_eq!(
            layout(&p),
            vec![
                Row::Instrument(ChannelId(1)),
                Row::Shape(ShapeId(11)),
                Row::Shape(ShapeId(12)),
                Row::Instrument(ChannelId(2)),
                Row::Shape(ShapeId(10)),
                // Main Output has no instrument: its lane goes last.
                Row::Shape(ShapeId(13)),
            ]
        );
    }

    #[test]
    fn the_menu_offers_four_basics_and_the_effect_settings() {
        let mut p = project();
        let all = choices(&p, ChannelId(1));
        assert_eq!(basic_count(&all), 4);
        assert!(all.len() > 4);
        assert!(all[4].0.contains(':'), "{}", all[4].0);
        assert_eq!(all[0].0, "Volume");
        // A shape that exists is not offered again.
        p.shapes
            .push(shape(10, ShapeTarget::Volume { track: TrackId(1) }));
        assert_eq!(choices(&p, ChannelId(1))[0].0, "Left/Right");
        // An audio row on the main output has no effects and no Pitch.
        let audio = choices(&p, ChannelId(2));
        assert_eq!(audio.len(), 3);
        assert!(audio.iter().all(|(n, _)| n != "Pitch"));
    }

    #[test]
    fn values_and_heights_round_trip() {
        let r = (-60.0, 6.0);
        let y = value_y(0.0, r, 100.0, 40.0, 4.0);
        assert!((y_value(y, r, 100.0, 40.0, 4.0) - 0.0).abs() < 1e-4);
        assert_eq!(value_y(6.0, r, 100.0, 40.0, 4.0), 104.0);
        assert_eq!(value_y(-60.0, r, 100.0, 40.0, 4.0), 136.0);
        assert_eq!(y_value(0.0, r, 100.0, 40.0, 4.0), 6.0);
        assert_eq!(y_value(500.0, r, 100.0, 40.0, 4.0), -60.0);
    }

    #[test]
    fn the_curve_runs_as_the_engine_plays_it() {
        let pts = [pt(100, 0.0, Curve::Linear), pt(200, 8.0, Curve::Hold)];
        assert_eq!(value_at(&pts, 0.0), Some(0.0));
        assert_eq!(value_at(&pts, 150.0), Some(4.0));
        assert_eq!(value_at(&pts, 900.0), Some(8.0));
        let hold = [pt(100, 0.0, Curve::Hold), pt(200, 8.0, Curve::Hold)];
        assert_eq!(value_at(&hold, 199.0), Some(0.0));
        assert_eq!(value_at(&[], 5.0), None);
    }

    #[test]
    fn points_add_move_remove_and_keep_their_order() {
        let pts = vec![pt(0, 0.0, Curve::Hold), pt(960, 1.0, Curve::Linear)];
        let (v, i) = with_point(&pts, 480, 0.5);
        assert_eq!((v.len(), i), (3, 1));
        assert_eq!(v[1].curve, Curve::Hold, "it splits a Hold segment");
        // Adding on an existing tick replaces that point.
        assert_eq!(with_point(&v, 480, 0.9).0.len(), 3);
        // A drag cannot pass its neighbours.
        let m = moved(&v, 1, 5000, 0.2);
        assert_eq!(m[1].tick, 959);
        assert_eq!(moved(&v, 1, 0, 0.2)[1].tick, 1);
        assert_eq!(moved(&v, 0, 0, 0.7)[0].value, 0.7);
        // The last point cannot be removed.
        assert_eq!(without(&v, 1).len(), 2);
        assert_eq!(without(&without(&v, 1), 1).len(), 1);
        assert_eq!(without(&[pt(0, 0.0, Curve::Smooth)], 0).len(), 1);
        assert_eq!(with_curve(&v, 2, Curve::Wave)[2].curve, Curve::Wave);
        let hit = nearest(&v, |p| (p.tick as f64, 0.0), 470.0, 3.0, 12.0);
        assert_eq!(hit, Some(1));
        assert_eq!(
            nearest(&v, |p| (p.tick as f64, 0.0), 300.0, 3.0, 12.0),
            None
        );
    }

    #[test]
    fn presets_write_over_the_span_and_keep_the_rest() {
        let mut p = project();
        p.loop_region = protocol::model::LoopRegion {
            start: 3840,
            end: 7680,
            enabled: true,
        };
        assert_eq!(span(&p, None, 100), (3840, 7680));
        assert_eq!(span(&p, Some((960, 1920)), 100), (960, 1920));
        p.loop_region.enabled = false;
        assert_eq!(span(&p, None, 100), (100, 100 + 4 * 3840));
        let s = Shape {
            id: ShapeId(5),
            target: ShapeTarget::Volume { track: TrackId(1) },
            points: vec![pt(0, 0.0, Curve::Smooth), pt(20000, 0.0, Curve::Smooth)],
        };
        for (name, preset) in PRESETS {
            let pts = preset_points(&p, &s, *preset, 3840, 7680);
            assert!(pts.windows(2).all(|w| w[0].tick < w[1].tick), "{name}");
            assert!(
                pts.iter().all(|q| (-60.0..=6.0).contains(&q.value)),
                "{name}"
            );
            assert_eq!(
                pts.first().map(|q| q.tick),
                Some(0),
                "{name} keeps what is before"
            );
            assert_eq!(
                pts.last().map(|q| q.tick),
                Some(20000),
                "{name} keeps what is after"
            );
        }
        // Fade In goes from silence up to the resting level.
        let fi = preset_points(&p, &s, Preset::FadeIn, 3840, 7680);
        let at = |t: u32| fi.iter().find(|q| q.tick == t).map(|q| q.value);
        assert_eq!(at(3840), Some(-60.0));
        assert_eq!(at(7680), Some(0.0));
        // Pump dips once per beat over four beats.
        let pump = preset_points(&p, &s, Preset::Pump, 0, 4 * PPQ);
        assert!(pump.iter().filter(|q| q.value < 0.0).count() >= 4);
    }

    #[test]
    fn tape_stop_on_pitch_brings_volume_down_too() {
        let mut p = project();
        let pitch = shape(
            5,
            ShapeTarget::Pitch {
                instrument: ChannelId(1),
            },
        );
        p.shapes.push(pitch.clone());
        let e = preset_edits(&p, &pitch, Preset::TapeStop, 960, 1920);
        assert_eq!(e.len(), 2);
        assert!(matches!(e[0], Edit::SetShapePoints { .. }));
        match &e[1] {
            Edit::AddShape { target, points } => {
                assert_eq!(*target, ShapeTarget::Volume { track: TrackId(1) });
                assert!(points.last().is_some_and(|q| q.tick == 960 + PPQ));
            }
            x => panic!("{x:?}"),
        }
        // On a volume lane it is one edit.
        let vol = shape(6, ShapeTarget::Volume { track: TrackId(1) });
        assert_eq!(preset_edits(&p, &vol, Preset::TapeStop, 960, 1920).len(), 1);
    }

    #[test]
    fn a_new_shape_starts_flat() {
        let p = project();
        match new_shape_edit(
            &p,
            ShapeTarget::Filter {
                instrument: ChannelId(1),
            },
            0,
        ) {
            Edit::AddShape { points, .. } => {
                assert_eq!(points.len(), 2);
                assert_eq!(points[0].value, points[1].value);
            }
            e => panic!("{e:?}"),
        }
        assert_eq!(
            label(
                &p,
                &ShapeTarget::Pitch {
                    instrument: ChannelId(1)
                }
            ),
            "Pitch"
        );
    }
}
