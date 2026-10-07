// SPDX-License-Identifier: GPL-3.0-or-later
//! Shapes: automation curves on the timeline (SPEC 24.2-1).
//!
//! Between two points `a` and `b` (the segment uses the curve stored on
//! `a`), with `u = (tick - a.tick) / (b.tick - a.tick)` in `[0, 1)` and
//! `d = b.value - a.value`:
//!
//! - linear: `a.value + d * u`
//! - smooth: `a.value + d * s(u)`, `s(u) = u * u * (3 - 2u)` (smoothstep)
//! - hold: `a.value` until `b`
//! - stairs: 8 equal steps, `a.value + d * floor(8u) / 7` (the first step
//!   is `a`, the last is `b`)
//! - pulse: 4 cycles; `a.value` for the first half of a cycle, `b.value`
//!   for the second half
//! - wave: 4 sine cycles between the two values,
//!   `a.value + d * (1 - cos(2 * pi * 4u)) / 2` (starts at `a`)
//!
//! Before the first point and from the last point on, the value is that
//! point's value. The engine evaluates every shape once per 64 frames of
//! absolute position (`CELL`), at the tick of the first frame of the cell.

use crate::compiled::Slots;
use protocol::consts::{FX_PARAMS_PER_INSERT, MAX_INSERTS};
use protocol::engine::{TrackSlot, fx_param_index};
use protocol::ids::InstanceId;
use protocol::model::{Curve, Insert, Project, Shape, ShapeTarget};

/// Frames between shape evaluations, on the absolute sample grid.
pub const CELL: u64 = 64;

/// Most shapes a project may compile (more are ignored).
pub const MAX_SHAPES: usize = 256;

/// What a compiled shape writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShapeDest {
    /// Track volume in dB, clamped to `-60..=6`; track slot.
    TrackVolume(u16),
    /// Track pan, clamped to `-1..=1`; track slot.
    TrackPan(u16),
    /// Pitch offset in semitones, clamped to `-24..=24`; channel slot.
    Pitch(u16),
    /// Low-pass amount `0..=1`; channel slot.
    Filter(u16),
    /// Index in the `ParamTable`.
    FxParam(usize),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PointC {
    pub tick: u32,
    pub value: f32,
    pub curve: Curve,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ShapeC {
    pub dest: ShapeDest,
    /// Sorted by tick; never empty.
    pub points: Vec<PointC>,
}

/// Compiles `project.shapes`. A shape whose target is gone, or that has no
/// points, is dropped; later shapes on the same destination win.
pub fn compile_shapes(project: &Project, slots: &Slots) -> Vec<ShapeC> {
    let mut out = Vec::new();
    for sh in project.shapes.iter().take(MAX_SHAPES) {
        if let Some(dest) = resolve(project, slots, sh)
            && !sh.points.is_empty()
        {
            let mut points: Vec<PointC> = sh
                .points
                .iter()
                .map(|p| PointC {
                    tick: p.tick,
                    value: p.value,
                    curve: p.curve,
                })
                .collect();
            points.sort_by_key(|p| p.tick);
            out.push(ShapeC { dest, points });
        }
    }
    out
}

fn resolve(project: &Project, slots: &Slots, sh: &Shape) -> Option<ShapeDest> {
    match sh.target {
        ShapeTarget::Volume { track } => {
            slots.track_slot(track).map(|s| ShapeDest::TrackVolume(s.0))
        }
        ShapeTarget::Pan { track } => slots.track_slot(track).map(|s| ShapeDest::TrackPan(s.0)),
        ShapeTarget::Pitch { instrument } => slots
            .channel_slot(instrument)
            .map(|s| ShapeDest::Pitch(s.0)),
        ShapeTarget::Filter { instrument } => slots
            .channel_slot(instrument)
            .map(|s| ShapeDest::Filter(s.0)),
        ShapeTarget::FxParam {
            track,
            instance,
            param,
        } => {
            let ts = slots.track_slot(track)?;
            let pos = insert_position(project, track, instance)?;
            (pos < MAX_INSERTS && (param as usize) < FX_PARAMS_PER_INSERT)
                .then(|| ShapeDest::FxParam(fx_param_index(TrackSlot(ts.0), pos, param as usize)))
        }
    }
}

fn insert_position(
    project: &Project,
    track: protocol::ids::TrackId,
    instance: InstanceId,
) -> Option<usize> {
    project
        .track(track)?
        .inserts
        .iter()
        .position(|i| matches!(i, Insert::Builtin { .. }) && i.instance() == instance)
}

/// Value of the sorted `points` at `tick`.
pub fn value_at(points: &[PointC], tick: f64) -> f32 {
    let first = &points[0];
    if tick <= first.tick as f64 {
        return first.value;
    }
    let i = points.partition_point(|p| (p.tick as f64) <= tick);
    if i >= points.len() {
        return points[points.len() - 1].value;
    }
    let (a, b) = (&points[i - 1], &points[i]);
    let span = (b.tick - a.tick) as f64;
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
    v as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pts(c: Curve) -> Vec<PointC> {
        vec![
            PointC {
                tick: 100,
                value: 0.0,
                curve: c,
            },
            PointC {
                tick: 200,
                value: 8.0,
                curve: Curve::Hold,
            },
        ]
    }

    #[test]
    fn holds_outside_and_follows_each_curve() {
        let lin = pts(Curve::Linear);
        assert_eq!(value_at(&lin, 0.0), 0.0);
        assert_eq!(value_at(&lin, 150.0), 4.0);
        assert_eq!(value_at(&lin, 999.0), 8.0);
        let sm = pts(Curve::Smooth);
        assert_eq!(value_at(&sm, 150.0), 4.0);
        assert!((value_at(&sm, 125.0) - 8.0 * 0.15625).abs() < 1e-6);
        let h = pts(Curve::Hold);
        assert_eq!(value_at(&h, 199.0), 0.0);
        assert_eq!(value_at(&h, 200.0), 8.0);
        let st = pts(Curve::Stairs);
        assert_eq!(value_at(&st, 100.0), 0.0);
        assert!((value_at(&st, 199.0) - 8.0).abs() < 1e-6);
        assert!((value_at(&st, 113.0) - 8.0 / 7.0).abs() < 1e-5);
        let pu = pts(Curve::Pulse);
        assert_eq!(value_at(&pu, 110.0), 0.0);
        assert_eq!(value_at(&pu, 140.0), 8.0);
        let wv = pts(Curve::Wave);
        assert_eq!(value_at(&wv, 100.0), 0.0);
        assert!((value_at(&wv, 112.5) - 8.0).abs() < 1e-5);
    }
}
