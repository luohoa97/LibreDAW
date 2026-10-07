// SPDX-License-Identifier: GPL-3.0-or-later
//! Per-bar levels and sections for `analyze` (SPEC 18.7): where a song is
//! quiet, builds, drops, breaks, so an agent knows where to put a part.
//!
//! Pure functions over the rendered mix. Rules, in the order they apply:
//!
//! * Level of a bar: BS.1770 loudness of that bar alone (LUFS), floored at
//!   `SILENT_LUFS` so silence is a number.
//! * Drop: bar `i` (i >= 1) whose level is at least `DROP_RISE_LU` above the
//!   mean level of the previous (up to 4) bars, and whose next bar (if any)
//!   also stays above that mean by at least `DROP_STAYS_LU`. Drops within 4
//!   bars of an earlier one are the same drop. A drop section lasts until a
//!   bar falls `DROP_END_LU` below the drop's level, or the song ends.
//! * Build: before a drop, the bars from the quietest bar of the (up to 8)
//!   bars before it, if at least 2 bars long and the level rises at least
//!   `BUILD_RISE_LU` over them without dipping more than 0.5 LU.
//! * Intro: the unlabelled bars from the start up to the first build or drop.
//! * Outro: unlabelled bars after the last drop, reaching the end of the song.
//! * Break: any other unlabelled bars after the first drop or build.
//!
//! A song with no drop has no sections.

use protocol::control::{BarLevels, Section};
use protocol::model::{Clip, Project, ticks_per_bar};

use crate::analysis::{band_balance, integrated_loudness};

pub const SILENT_LUFS: f64 = -70.0;
pub const DROP_RISE_LU: f64 = 6.0;
pub const DROP_STAYS_LU: f64 = 3.0;
pub const DROP_END_LU: f64 = 6.0;
pub const BUILD_RISE_LU: f64 = 3.0;

/// Levels of each whole bar of `frames`, which start at `first_bar` of the
/// song. `frames_per_bar` is exact (tempo is not a whole number of frames);
/// bars are cut by rounding. A last partial bar is analysed as it is.
pub fn bars(
    frames: &[[f32; 2]],
    rate: u32,
    frames_per_bar: f64,
    first_bar: u32,
    project: &Project,
) -> Vec<BarLevels> {
    if frames_per_bar < 1.0 || frames.is_empty() {
        return Vec::new();
    }
    let count = (frames.len() as f64 / frames_per_bar).ceil() as usize;
    let tpb = ticks_per_bar(project.time_sig_num);
    (0..count)
        .map(|i| {
            let a = (i as f64 * frames_per_bar).round() as usize;
            let b = (((i + 1) as f64 * frames_per_bar).round() as usize).min(frames.len());
            let part = &frames[a.min(b)..b];
            let peak = part
                .iter()
                .flatten()
                .fold(0.0f64, |m, s| m.max(f64::from(*s).abs()));
            let bar = first_bar + i as u32;
            BarLevels {
                bar,
                lufs: integrated_loudness(part, rate).max(SILENT_LUFS),
                peak_dbfs: if peak <= 0.0 {
                    -120.0
                } else {
                    (20.0 * peak.log10()).max(-120.0)
                },
                band_balance: band_balance(part, rate),
                active_tracks: playing_tracks(project, bar * tpb, (bar + 1) * tpb),
            }
        })
        .collect()
}

/// Mixer tracks that have an unmuted clip of an unmuted instrument sounding
/// in `[from, to)` ticks. (The tracks are not rendered one by one, so this
/// is what is scheduled, not what is measured.)
fn playing_tracks(project: &Project, from: u32, to: u32) -> Vec<u32> {
    let mut out: Vec<u32> = project
        .clips
        .iter()
        .filter(|c: &&Clip| !c.muted && c.start < to && c.end() > from)
        .filter_map(|c| project.channel(c.instrument))
        .filter(|ch| !ch.mix.mute)
        .map(|ch| ch.track.0)
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

fn mean(v: &[f64]) -> f64 {
    if v.is_empty() {
        0.0
    } else {
        v.iter().sum::<f64>() / v.len() as f64
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Label {
    Drop,
    Build,
}

/// Sections from the bar levels (rules in the module comment).
pub fn sections(levels: &[BarLevels]) -> Vec<Section> {
    let l: Vec<f64> = levels.iter().map(|b| b.lufs).collect();
    let n = l.len();
    let mut label: Vec<Option<Label>> = vec![None; n];
    // Drops.
    let mut last_drop: Option<usize> = None;
    let mut drops = Vec::new();
    for i in 1usize..n {
        let prev = mean(&l[i.saturating_sub(4)..i]);
        let rises = l[i] - prev >= DROP_RISE_LU;
        let stays = l.get(i + 1).is_none_or(|next| next - prev >= DROP_STAYS_LU);
        if rises && stays && last_drop.is_none_or(|d| i - d > 4) {
            last_drop = Some(i);
            drops.push(i);
        }
    }
    for &d in &drops {
        let level = mean(&l[d..(d + 2).min(n)]);
        let end = (d + 1..n)
            .find(|&j| l[j] < level - DROP_END_LU)
            .unwrap_or(n);
        for x in &mut label[d..end] {
            *x = Some(Label::Drop);
        }
    }
    // Builds.
    for &d in &drops {
        let mut b = d;
        while b > 0 && d - b < 8 && label[b - 1].is_none() && l[b - 1] <= l[b] + 0.5 {
            b -= 1;
        }
        if b + 1 >= d {
            continue;
        }
        // Start from the quietest (latest) bar of the climb.
        let low = (b..d)
            .min_by(|&x, &y| l[x].partial_cmp(&l[y]).unwrap().then(y.cmp(&x)))
            .unwrap_or(b);
        if d - low >= 2 && l[d - 1] - l[low] >= BUILD_RISE_LU {
            for x in &mut label[low..d] {
                *x = Some(Label::Build);
            }
        }
    }
    let (Some(first), Some(last)) = (
        label.iter().position(Option::is_some),
        label.iter().rposition(|x| *x == Some(Label::Drop)),
    ) else {
        return Vec::new();
    };
    let mut out: Vec<Section> = Vec::new();
    let mut push = |kind: &str, from: usize, to: usize| {
        let s = Section {
            start_bar: levels[from].bar,
            end_bar: levels[to - 1].bar + 1,
            kind: kind.to_string(),
            lufs: mean(&l[from..to]),
        };
        match out.last_mut() {
            Some(p) if p.kind == s.kind && p.end_bar == s.start_bar => {
                let (a, b) = (
                    (p.end_bar - p.start_bar) as f64,
                    (s.end_bar - s.start_bar) as f64,
                );
                p.lufs = (p.lufs * a + s.lufs * b) / (a + b);
                p.end_bar = s.end_bar;
            }
            _ => out.push(s),
        }
    };
    let mut i = 0;
    while i < n {
        let mut j = i + 1;
        while j < n && label[j] == label[i] {
            j += 1;
        }
        match label[i] {
            Some(Label::Drop) => push("drop", i, j),
            Some(Label::Build) => push("build", i, j),
            None if j <= first => push("intro", i, j),
            None if i > last && j == n => push("outro", i, j),
            None => push("break", i, j),
        }
        i = j;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn levels(l: &[f64]) -> Vec<BarLevels> {
        l.iter()
            .enumerate()
            .map(|(i, &lufs)| BarLevels {
                bar: i as u32,
                lufs,
                peak_dbfs: -6.0,
                band_balance: [0.3, 0.4, 0.3],
                active_tracks: vec![],
            })
            .collect()
    }

    fn kinds(s: &[Section]) -> Vec<(String, u32, u32)> {
        s.iter()
            .map(|s| (s.kind.clone(), s.start_bar, s.end_bar))
            .collect()
    }

    #[test]
    fn eight_quiet_bars_then_a_loud_drop_has_the_drop_at_bar_eight() {
        let mut v = vec![-30.0; 8];
        v.extend(vec![-14.0; 8]);
        let s = sections(&levels(&v));
        assert_eq!(kinds(&s), [("intro".into(), 0, 8), ("drop".into(), 8, 16)]);
    }

    #[test]
    fn a_climb_is_a_build_and_a_dip_after_the_drop_is_a_break() {
        let v = [
            -30.0, -30.0, -29.0, -28.0, -27.0, -26.0, // a slow climb from bar 1
            -17.0, -17.0, -17.0, -17.0, // drop
            -30.0, -30.0, // break
            -17.0, -17.0, -17.0, // drop again
            -26.0, -30.0, // outro
        ];
        let s = sections(&levels(&v));
        let k: Vec<&str> = s.iter().map(|s| s.kind.as_str()).collect();
        assert_eq!(
            k,
            ["intro", "build", "drop", "break", "drop", "outro"],
            "{s:?}"
        );
        let drop = s.iter().find(|s| s.kind == "drop").unwrap();
        assert_eq!(drop.start_bar, 6);
    }

    #[test]
    fn a_flat_song_has_no_sections() {
        assert!(sections(&levels(&[-14.0; 12])).is_empty());
        assert!(sections(&[]).is_empty());
    }

    #[test]
    fn rendered_quiet_then_loud_bars_measure_and_drop() {
        let rate = 8000u32;
        let per_bar = 4000usize; // half a second a bar
        let mut frames = Vec::new();
        for bar in 0..16 {
            let amp = if bar < 8 { 0.03 } else { 0.5 };
            for i in 0..per_bar {
                let s = (2.0 * std::f64::consts::PI * 440.0 * i as f64 / f64::from(rate)).sin();
                frames.push([(s * amp) as f32; 2]);
            }
        }
        let b = bars(&frames, rate, per_bar as f64, 0, &Project::empty());
        assert_eq!(b.len(), 16);
        assert!(b[8].lufs - b[7].lufs > 20.0, "{} {}", b[7].lufs, b[8].lufs);
        assert!((b[8].peak_dbfs + 6.0).abs() < 0.5);
        let s = sections(&b);
        let drop = s.iter().find(|s| s.kind == "drop").expect("a drop");
        assert_eq!(drop.start_bar, 8);
    }
}
