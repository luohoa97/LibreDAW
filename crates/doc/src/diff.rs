// SPDX-License-Identifier: GPL-3.0-or-later
//! `HistoryDiff` (SPEC 15.11, 15.12): a short, readable list of what changed
//! between two projects, for the Versions panel and for agents.
//!
//! Lines name things the way the screen does: instruments by name, content
//! by clip name, positions in bars. Many alike changes are summarized
//! ("12 clips moved"), and the list is capped, so the answer stays small
//! enough to read.

use std::collections::{BTreeMap, HashMap, HashSet};

use protocol::consts::PPQ;
use protocol::ids::{ChannelId, ClipId, NoteId, PatternId, TrackId};
use protocol::model::{Channel, Clip, Note, Pattern, Project, Track};

/// Most lines returned; the last one says how many were left out.
pub const MAX_LINES: usize = 200;
/// More alike clip changes than this are summarized in one line.
const SUMMARY_AFTER: usize = 4;

fn num(x: f64) -> String {
    if x.fract() == 0.0 {
        format!("{}", x as i64)
    } else {
        let s = format!("{x:.2}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

struct Ctx<'a> {
    a: &'a Project,
    b: &'a Project,
    bar: f64,
}

impl Ctx<'_> {
    fn bars(&self, ticks: u32) -> String {
        num(ticks as f64 / self.bar)
    }

    /// 1-based bar position, as the ruler shows it.
    fn at(&self, tick: u32) -> String {
        num(tick as f64 / self.bar + 1.0)
    }

    fn inst(&self, id: ChannelId) -> String {
        self.b
            .channel(id)
            .or_else(|| self.a.channel(id))
            .map_or_else(|| format!("instrument {}", id.0), |c| c.name.clone())
    }

    fn track(&self, id: TrackId) -> String {
        self.b
            .track(id)
            .or_else(|| self.a.track(id))
            .map_or_else(|| format!("track {}", id.0), |t| t.name.clone())
    }
}

fn by_id<T, K: std::hash::Hash + Eq + Copy>(
    items: &[std::sync::Arc<T>],
    key: impl Fn(&T) -> K,
) -> HashMap<K, &T> {
    items.iter().map(|i| (key(i), &**i)).collect()
}

/// What changed from `a` to `b`. Empty if nothing did.
pub fn diff_projects(a: &Project, b: &Project) -> Vec<String> {
    let cx = Ctx {
        a,
        b,
        bar: (b.time_sig_num.max(1) as u32 * PPQ) as f64,
    };
    let mut out: Vec<String> = Vec::new();
    project_lines(&cx, &mut out);
    channel_lines(&cx, &mut out);
    content_lines(&cx, &mut out);
    clip_lines(&cx, &mut out);
    track_lines(&cx, &mut out);
    sample_lines(&cx, &mut out);
    if out.len() > MAX_LINES {
        let more = out.len() - (MAX_LINES - 1);
        out.truncate(MAX_LINES - 1);
        out.push(format!("... and {more} more changes"));
    }
    out
}

fn project_lines(cx: &Ctx, out: &mut Vec<String>) {
    let (a, b) = (cx.a, cx.b);
    if a.tempo_bpm != b.tempo_bpm {
        out.push(format!(
            "tempo {} -> {}",
            num(a.tempo_bpm),
            num(b.tempo_bpm)
        ));
    }
    if a.time_sig_num != b.time_sig_num {
        out.push(format!(
            "time signature {}/4 -> {}/4",
            a.time_sig_num, b.time_sig_num
        ));
    }
    if a.metronome != b.metronome {
        let on = |m: bool| if m { "on" } else { "off" };
        if a.metronome.enabled != b.metronome.enabled {
            out.push(format!(
                "metronome {} -> {}",
                on(a.metronome.enabled),
                on(b.metronome.enabled)
            ));
        }
        if a.metronome.gain_db != b.metronome.gain_db {
            out.push(format!(
                "metronome volume {} -> {} dB",
                num(a.metronome.gain_db),
                num(b.metronome.gain_db)
            ));
        }
    }
    if a.loop_region != b.loop_region {
        let show = |r: &protocol::model::LoopRegion| {
            if r.end == 0 {
                "none".to_string()
            } else {
                format!(
                    "bars {}-{}{}",
                    cx.at(r.start),
                    cx.at(r.end),
                    if r.enabled { "" } else { " (off)" }
                )
            }
        };
        out.push(format!(
            "loop region {} -> {}",
            show(&a.loop_region),
            show(&b.loop_region)
        ));
    }
}

fn mix_lines(
    name: &str,
    a: &protocol::model::Mix,
    b: &protocol::model::Mix,
    out: &mut Vec<String>,
) {
    if a.volume_db != b.volume_db {
        out.push(format!(
            "{name}: volume {} -> {} dB",
            num(a.volume_db),
            num(b.volume_db)
        ));
    }
    if a.pan != b.pan {
        out.push(format!("{name}: pan {} -> {}", num(a.pan), num(b.pan)));
    }
    if a.mute != b.mute {
        out.push(format!(
            "{name}: {}",
            if b.mute { "muted" } else { "unmuted" }
        ));
    }
    if a.solo != b.solo {
        out.push(format!(
            "{name}: {}",
            if b.solo { "solo on" } else { "solo off" }
        ));
    }
}

fn channel_lines(cx: &Ctx, out: &mut Vec<String>) {
    let a = by_id(&cx.a.channels, |c: &Channel| c.id);
    let b = by_id(&cx.b.channels, |c: &Channel| c.id);
    for c in &cx.b.channels {
        match a.get(&c.id) {
            None => out.push(format!("added instrument {}", c.name)),
            Some(old) if **old != **c => {
                if old.name != c.name {
                    out.push(format!("{} renamed to {}", old.name, c.name));
                }
                mix_lines(&c.name, &old.mix, &c.mix, out);
                if old.root_key != c.root_key {
                    out.push(format!(
                        "{}: root key {} -> {}",
                        c.name, old.root_key, c.root_key
                    ));
                }
                if old.track != c.track {
                    out.push(format!(
                        "{}: output {} -> {}",
                        c.name,
                        cx.track(old.track),
                        cx.track(c.track)
                    ));
                }
                if old.choke_group != c.choke_group {
                    out.push(format!(
                        "{}: choke group {} -> {}",
                        c.name, old.choke_group, c.choke_group
                    ));
                }
                if old.instrument != c.instrument {
                    out.push(format!("{}: sound changed", c.name));
                }
            }
            Some(_) => {}
        }
    }
    for c in &cx.a.channels {
        if !b.contains_key(&c.id) {
            out.push(format!("removed instrument {}", c.name));
        }
    }
}

type StepMap<'a> = BTreeMap<(u32, u8), &'a Note>;

/// A step note keyed by where it sits: `(start, key)`.
fn step_key(n: &Note) -> (u32, u8) {
    (n.start, n.key)
}

fn content_lines(cx: &Ctx, out: &mut Vec<String>) {
    let a = by_id(&cx.a.patterns, |p: &Pattern| p.id);
    let b = by_id(&cx.b.patterns, |p: &Pattern| p.id);
    for p in &cx.b.patterns {
        let inst = cx.inst(p.instrument);
        let Some(old) = a.get(&p.id) else {
            out.push(format!(
                "{inst}: new clip content {} ({})",
                p.name,
                plural(p.notes.len(), "note", "notes")
            ));
            continue;
        };
        if **old == **p {
            continue;
        }
        if old.name != p.name {
            out.push(format!("{inst}: clip {} renamed to {}", old.name, p.name));
        }
        if old.length_steps != p.length_steps || old.step_ticks != p.step_ticks {
            out.push(format!(
                "{inst}: clip {} length {} -> {} steps",
                p.name, old.length_steps, p.length_steps
            ));
        }
        if old.swing != p.swing {
            out.push(format!(
                "{inst}: clip {} swing {} -> {}",
                p.name, old.swing, p.swing
            ));
        }
        note_lines(cx, &inst, old, p, out);
    }
    for p in &cx.a.patterns {
        if !b.contains_key(&p.id) {
            out.push(format!(
                "{}: clip content {} removed",
                cx.inst(p.instrument),
                p.name
            ));
        }
    }
}

fn note_lines(cx: &Ctx, inst: &str, old: &Pattern, new: &Pattern, out: &mut Vec<String>) {
    let root = |p: &Pattern| {
        cx.b.channel(p.instrument)
            .or_else(|| cx.a.channel(p.instrument))
            .map_or(60, |c| c.root_key)
    };
    let (ra, rb) = (root(old), root(new));
    let (mut sa, mut sb): (StepMap, StepMap) = (BTreeMap::new(), BTreeMap::new());
    let (mut pa, mut pb): (HashMap<NoteId, &Note>, HashMap<NoteId, &Note>) =
        (HashMap::new(), HashMap::new());
    for n in &old.notes {
        if n.is_step_note(ra, old) {
            sa.insert(step_key(n), n);
        } else {
            pa.insert(n.id, n);
        }
    }
    for n in &new.notes {
        if n.is_step_note(rb, new) {
            sb.insert(step_key(n), n);
        } else {
            pb.insert(n.id, n);
        }
    }
    let added = sb.keys().filter(|k| !sa.contains_key(k)).count();
    let removed = sa.keys().filter(|k| !sb.contains_key(k)).count();
    let changed = sb
        .iter()
        .filter(|(k, n)| {
            sa.get(k)
                .is_some_and(|o| (o.vel, o.off, o.repeat) != (n.vel, n.off, n.repeat))
        })
        .count();
    let name = &new.name;
    let sp = |n: usize| plural(n, "step", "steps");
    if added > 0 {
        out.push(format!("{inst}: {} added in clip {name}", sp(added)));
    }
    if removed > 0 {
        out.push(format!("{inst}: {} removed in clip {name}", sp(removed)));
    }
    if changed > 0 {
        out.push(format!("{inst}: {} changed in clip {name}", sp(changed)));
    }
    let nn = |n: usize| plural(n, "note", "notes");
    let padd = pb.keys().filter(|k| !pa.contains_key(k)).count();
    let prem = pa.keys().filter(|k| !pb.contains_key(k)).count();
    let pchg = pb
        .iter()
        .filter(|(k, n)| pa.get(*k).is_some_and(|o| **o != ***n))
        .count();
    if padd > 0 {
        out.push(format!("{inst}: {} added in clip {name}", nn(padd)));
    }
    if prem > 0 {
        out.push(format!("{inst}: {} removed in clip {name}", nn(prem)));
    }
    if pchg > 0 {
        out.push(format!("{inst}: {} changed in clip {name}", nn(pchg)));
    }
}

fn clip_lines(cx: &Ctx, out: &mut Vec<String>) {
    let a: HashMap<ClipId, &Clip> = cx.a.clips.iter().map(|c| (c.id, c)).collect();
    let b: HashMap<ClipId, &Clip> = cx.b.clips.iter().map(|c| (c.id, c)).collect();
    let mut added: Vec<String> = Vec::new();
    let mut removed: Vec<String> = Vec::new();
    let mut moved: Vec<String> = Vec::new();
    let mut resized: Vec<String> = Vec::new();
    let mut muted: Vec<String> = Vec::new();
    let mut other: Vec<String> = Vec::new();
    for c in &cx.b.clips {
        let inst = cx.inst(c.instrument);
        match a.get(&c.id) {
            None => added.push(format!("{inst}: clip added at bar {}", cx.at(c.start))),
            Some(o) if **o != *c => {
                if o.instrument != c.instrument {
                    other.push(format!(
                        "clip at bar {} moved from {} to {}",
                        cx.at(c.start),
                        cx.inst(o.instrument),
                        inst
                    ));
                } else if o.start != c.start {
                    moved.push(format!(
                        "{inst}: clip moved from bar {} to bar {}",
                        cx.at(o.start),
                        cx.at(c.start)
                    ));
                }
                if o.len != c.len {
                    resized.push(format!(
                        "{inst}: clip at bar {} resized from {} to {} bars",
                        cx.at(c.start),
                        cx.bars(o.len),
                        cx.bars(c.len)
                    ));
                }
                if o.muted != c.muted {
                    muted.push(format!(
                        "{inst}: clip at bar {} {}",
                        cx.at(c.start),
                        if c.muted { "muted" } else { "unmuted" }
                    ));
                }
                if o.pattern != c.pattern && o.instrument == c.instrument {
                    let name = |id: PatternId, p: &Project| {
                        p.pattern(id)
                            .map_or_else(|| id.0.to_string(), |x| x.name.clone())
                    };
                    other.push(format!(
                        "{inst}: clip at bar {} now plays {} (was {})",
                        cx.at(c.start),
                        name(c.pattern, cx.b),
                        name(o.pattern, cx.a)
                    ));
                }
                if o.offset != c.offset && o.start == c.start && o.len == c.len {
                    other.push(format!(
                        "{inst}: clip at bar {} content shifted",
                        cx.at(c.start)
                    ));
                }
            }
            Some(_) => {}
        }
    }
    for c in &cx.a.clips {
        if !b.contains_key(&c.id) {
            removed.push(format!(
                "{}: clip removed at bar {}",
                cx.inst(c.instrument),
                cx.at(c.start)
            ));
        }
    }
    let summary: [(Vec<String>, &str); 5] = [
        (added, "clips added"),
        (removed, "clips removed"),
        (moved, "clips moved"),
        (resized, "clips resized"),
        (muted, "clips muted or unmuted"),
    ];
    for (lines, what) in summary {
        if lines.len() > SUMMARY_AFTER {
            out.push(format!("{} {}", lines.len(), what));
        } else {
            out.extend(lines);
        }
    }
    out.extend(other);
}

fn track_lines(cx: &Ctx, out: &mut Vec<String>) {
    let a = by_id(&cx.a.tracks, |t: &Track| t.id);
    let b: HashSet<TrackId> = cx.b.tracks.iter().map(|t| t.id).collect();
    for t in &cx.b.tracks {
        let Some(old) = a.get(&t.id) else {
            out.push(format!("added mixer track {}", t.name));
            continue;
        };
        if **old == **t {
            continue;
        }
        if old.name != t.name {
            out.push(format!("mixer track {} renamed to {}", old.name, t.name));
        }
        mix_lines(&format!("track {}", t.name), &old.mix, &t.mix, out);
        if old.inserts.len() != t.inserts.len() {
            out.push(format!(
                "track {}: effects {} -> {}",
                t.name,
                old.inserts.len(),
                t.inserts.len()
            ));
        } else if old.inserts != t.inserts {
            out.push(format!("track {}: effect settings changed", t.name));
        }
        if old.sends != t.sends {
            out.push(format!("track {}: sends changed", t.name));
        }
    }
    for t in &cx.a.tracks {
        if !b.contains(&t.id) {
            out.push(format!("removed mixer track {}", t.name));
        }
    }
}

fn sample_lines(cx: &Ctx, out: &mut Vec<String>) {
    let a: HashSet<&str> = cx.a.samples.iter().map(|s| s.hash.as_str()).collect();
    let b: HashSet<&str> = cx.b.samples.iter().map(|s| s.hash.as_str()).collect();
    for s in &cx.b.samples {
        if !a.contains(s.hash.as_str()) {
            out.push(format!("added sample {}", s.orig_name));
        }
    }
    for s in &cx.a.samples {
        if !b.contains(s.hash.as_str()) {
            out.push(format!("removed sample {}", s.orig_name));
        }
    }
}
