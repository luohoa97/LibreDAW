// SPDX-License-Identifier: GPL-3.0-or-later
//! Ready-made shapes (24.2-1): the points a preset writes over a range of
//! the song. Pure: no project, no UI.

use protocol::consts::PPQ;
use protocol::model::{Curve, ShapePoint, ShapeTarget};

/// What a shape controls, in its own units.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Range {
    /// The quiet or closed end.
    pub lo: f32,
    /// The loud or open end.
    pub hi: f32,
    /// Where the value rests when the shape does nothing.
    pub neutral: f32,
    /// How far a dip or a wobble goes.
    pub depth: f32,
}

/// The range of a target. `fx` is the parameter's own range, for
/// effect parameters.
pub fn range_of(target: &ShapeTarget, fx: Option<(f64, f64)>) -> Range {
    match target {
        ShapeTarget::Volume { .. } => Range {
            lo: -60.0,
            hi: 6.0,
            neutral: 0.0,
            depth: 12.0,
        },
        ShapeTarget::Pan { .. } => Range {
            lo: -1.0,
            hi: 1.0,
            neutral: 0.0,
            depth: 1.0,
        },
        ShapeTarget::Pitch { .. } => Range {
            lo: -24.0,
            hi: 24.0,
            neutral: 0.0,
            depth: 12.0,
        },
        ShapeTarget::Filter { .. } => Range {
            lo: 0.0,
            hi: 1.0,
            neutral: 1.0,
            depth: 0.7,
        },
        ShapeTarget::FxParam { .. } => {
            let (lo, hi) = fx.unwrap_or((0.0, 1.0));
            Range {
                lo: lo as f32,
                hi: hi as f32,
                neutral: hi as f32,
                depth: ((hi - lo) * 0.5) as f32,
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preset {
    FadeIn,
    FadeOut,
    Swell,
    Drop,
    Pump,
    Wobble,
    TapeStop,
}

pub const NAMES: &str = "fade_in, fade_out, swell, drop, pump, wobble, tape_stop";

impl Preset {
    pub fn parse(name: &str) -> Option<Preset> {
        let n = name.trim().to_lowercase().replace([' ', '-'], "_");
        Some(match n.as_str() {
            "fade_in" => Preset::FadeIn,
            "fade_out" => Preset::FadeOut,
            "swell" => Preset::Swell,
            "drop" => Preset::Drop,
            "pump" => Preset::Pump,
            "wobble" => Preset::Wobble,
            "tape_stop" => Preset::TapeStop,
            _ => return None,
        })
    }
}

fn pt(tick: u32, value: f32, curve: Curve) -> ShapePoint {
    ShapePoint { tick, value, curve }
}

/// The points of `preset` over `start..end` ticks. Values stay inside the
/// range; ticks are sorted. Tape Stop ignores `end` and lasts one beat.
pub fn preset_points(preset: Preset, r: Range, start: u32, end: u32) -> Vec<ShapePoint> {
    let end = end.max(start + 1);
    let len = end - start;
    let dip = (r.neutral - r.depth).max(r.lo);
    let mut v = match preset {
        Preset::FadeIn => vec![
            pt(start, r.lo, Curve::Smooth),
            pt(end, r.neutral, Curve::Smooth),
        ],
        Preset::FadeOut => vec![
            pt(start, r.neutral, Curve::Smooth),
            pt(end, r.lo, Curve::Smooth),
        ],
        Preset::Swell => vec![
            pt(start, r.lo, Curve::Smooth),
            pt(start + len / 2, r.neutral, Curve::Smooth),
            pt(end, r.lo, Curve::Smooth),
        ],
        Preset::Drop => vec![
            pt(start, r.neutral, Curve::Smooth),
            pt(start + len / 4, r.lo, Curve::Smooth),
            pt(end, r.lo, Curve::Hold),
        ],
        Preset::Pump => {
            // A dip on every beat, back up before the next one.
            let mut out = Vec::new();
            let mut t = start;
            while t < end {
                out.push(pt(t, dip, Curve::Smooth));
                let up = t + PPQ / 2;
                if up < end {
                    out.push(pt(up, r.neutral, Curve::Hold));
                }
                t += PPQ;
            }
            out
        }
        Preset::Wobble => {
            // Up and down every half beat.
            let mut out = Vec::new();
            let mut t = start;
            let mut low = false;
            while t <= end {
                out.push(pt(t, if low { dip } else { r.neutral }, Curve::Smooth));
                low = !low;
                t += PPQ / 2;
            }
            out
        }
        Preset::TapeStop => vec![
            pt(start, r.neutral, Curve::Smooth),
            pt(start + PPQ, r.lo, Curve::Smooth),
        ],
    };
    let (lo, hi) = (r.lo.min(r.hi), r.lo.max(r.hi));
    for p in &mut v {
        p.value = p.value.clamp(lo, hi);
    }
    v.sort_by_key(|p| p.tick);
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::ids::TrackId;

    const T: ShapeTarget = ShapeTarget::Volume { track: TrackId(1) };

    fn r() -> Range {
        range_of(&T, None)
    }

    #[test]
    fn fades_run_between_quiet_and_rest() {
        let p = preset_points(Preset::FadeIn, r(), 0, 3840);
        assert_eq!((p[0].tick, p[0].value), (0, -60.0));
        assert_eq!((p[1].tick, p[1].value), (3840, 0.0));
        let p = preset_points(Preset::FadeOut, r(), 960, 1920);
        assert_eq!((p[0].value, p[1].value), (0.0, -60.0));
    }

    #[test]
    fn pump_dips_once_per_beat() {
        let p = preset_points(Preset::Pump, r(), 0, 3840);
        let dips: Vec<u32> = p.iter().filter(|x| x.value < 0.0).map(|x| x.tick).collect();
        assert_eq!(dips, vec![0, 960, 1920, 2880]);
        assert!(p.windows(2).all(|w| w[0].tick < w[1].tick));
    }

    #[test]
    fn wobble_alternates_and_tape_stop_lasts_one_beat() {
        let p = preset_points(Preset::Wobble, r(), 0, 1920);
        assert_eq!(p.len(), 5);
        assert!(p[0].value > p[1].value && p[2].value > p[1].value);
        let pitch = range_of(
            &ShapeTarget::Pitch {
                instrument: protocol::ids::ChannelId(1),
            },
            None,
        );
        let p = preset_points(Preset::TapeStop, pitch, 960, 99999);
        assert_eq!(p.len(), 2);
        assert_eq!((p[1].tick, p[1].value), (1920, -24.0));
    }

    #[test]
    fn swell_and_drop_stay_in_range() {
        for preset in [Preset::Swell, Preset::Drop] {
            let p = preset_points(preset, r(), 0, 3840);
            assert!(p.iter().all(|x| (-60.0..=6.0).contains(&x.value)));
        }
        assert_eq!(Preset::parse("Tape Stop"), Some(Preset::TapeStop));
        assert_eq!(Preset::parse("nope"), None);
    }
}
