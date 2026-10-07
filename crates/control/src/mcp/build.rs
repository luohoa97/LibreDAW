// SPDX-License-Identifier: GPL-3.0-or-later
//! Turns the compact text arguments of the efficient tools (18.4) into
//! `Edit` batches plus a compact diff. Pure: works on a `Project` snapshot.
//!
//! Under the timeline model (SPEC 20) a step row or a note line belongs to
//! one clip content; tools name it by clip, by content, or by instrument
//! when that instrument has exactly one content.

use std::collections::HashSet;

use protocol::edit::{Edit, NewNote};
use protocol::ids::{ChannelId, ClipId, PatternId};
use protocol::model::{Project, ticks_per_bar};
use serde_json::Value;

use super::grid;
use super::notes;
use super::summary::{content_view, quoted};

/// Most diff lines returned; the rest is summarised.
const MAX_DIFF_LINES: usize = 24;

/// Which content a row or note line is for: exactly one of the three.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Target {
    pub clip: Option<ClipId>,
    pub content: Option<PatternId>,
    pub instrument: Option<ChannelId>,
}

/// The content a target names, or an error that says how to name it.
pub fn resolve(project: &Project, t: Target) -> Result<PatternId, String> {
    match (t.clip, t.content, t.instrument) {
        (Some(c), None, None) => project
            .clips
            .iter()
            .find(|x| x.id == c)
            .map(|x| x.pattern)
            .ok_or_else(|| {
                format!("clip {c} does not exist; call project_summary for the clip ids (C<id>)")
            }),
        (None, Some(p), None) => project.pattern(p).map(|p| p.id).ok_or_else(|| {
            format!("content {p} does not exist; call project_summary for the content ids (P<id>)")
        }),
        (None, None, Some(i)) => {
            if project.channel(i).is_none() {
                return Err(format!(
                    "instrument {i} does not exist; call project_summary for the instrument ids (I<id>)"
                ));
            }
            let owned: Vec<PatternId> = project
                .patterns
                .iter()
                .filter(|p| p.instrument == i)
                .map(|p| p.id)
                .collect();
            match owned.as_slice() {
                [one] => Ok(*one),
                [] => Err(format!(
                    "instrument {i} has no clip yet: add one with clips_add (it can take the grid or notes directly)"
                )),
                many => Err(format!(
                    "instrument {i} has {} contents ({}): name the clip or the content instead",
                    many.len(),
                    many.iter()
                        .map(|p| format!("P{p}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                )),
            }
        }
        _ => Err("name exactly one of clip, content or instrument".into()),
    }
}

/// One `beat_grid_set` row with its content resolved.
pub struct GridRow<'a> {
    pub pattern: PatternId,
    pub grid: &'a str,
    pub vel: Option<u8>,
    pub ratchet: Option<u8>,
}

/// `beat_grid_set`: edits for every row that changes, and one diff line per
/// changed row. Errors name the row.
pub fn grid_edits(project: &Project, rows: &[GridRow]) -> Result<(Vec<Edit>, Vec<String>), String> {
    let mut seen = HashSet::new();
    let mut edits = Vec::new();
    let mut diff = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        let ctx = |m: String| format!("row {i} (content {}): {m}", row.pattern);
        let pattern = project
            .pattern(row.pattern)
            .ok_or_else(|| ctx("the content does not exist".into()))?;
        if !seen.insert(row.pattern) {
            return Err(ctx(
                "the content appears twice in rows (linked clips share one content)".into(),
            ));
        }
        let existing = grid::read_existing(project, pattern);
        let name = project
            .channel(pattern.instrument)
            .map(|c| quoted(&c.name))
            .unwrap_or_default();
        let (e, d) = row_edits_and_diff(row, pattern.length_steps as usize, &existing, &name)
            .map_err(&ctx)?;
        edits.extend(e);
        diff.extend(d);
    }
    Ok((edits, diff))
}

/// Edits and diff line for one row against what the content holds now
/// (all off for a content made in the same batch).
pub fn row_edits_and_diff(
    row: &GridRow,
    len: usize,
    existing: &[Option<grid::Existing>],
    name: &str,
) -> Result<(Vec<Edit>, Vec<String>), String> {
    let parsed = grid::parse_grid(row.grid, len, row.pattern)?;
    let vel = row.vel.unwrap_or(grid::DEFAULT_VEL);
    let wanted = grid::wanted(&parsed, vel, row.ratchet);
    let edits = grid::row_edits(row.pattern, existing, &wanted);
    if edits.is_empty() {
        return Ok((edits, Vec::new()));
    }
    let (old_row, _) = grid::existing_to_row(existing);
    let new_row = new_row_text(&wanted, &parsed);
    let diff = vec![format!(
        "P{} {name} {} -> {}",
        row.pattern,
        grid::format_grid(&old_row, Some(grid::GROUP)),
        grid::format_grid(&new_row, Some(grid::GROUP)),
    )];
    Ok((edits, diff))
}

/// The row as it will read back (ratchets from the row default included).
fn new_row_text(wanted: &[Option<grid::Wanted>], parsed: &grid::Row) -> grid::Row {
    wanted
        .iter()
        .zip(parsed)
        .map(|(w, p)| {
            w.map(|w| grid::Hit {
                accent: p.is_some_and(|h| h.accent),
                ratchet: (w.repeat > 1).then_some(w.repeat),
            })
        })
        .collect()
}

/// `notes_write` for one content: remove (optional) and add notes.
pub fn notes_edits(
    project: &Project,
    pattern_id: PatternId,
    text: &str,
    replace: bool,
) -> Result<(Vec<Edit>, Vec<String>), String> {
    let pattern = project.pattern(pattern_id).ok_or_else(|| {
        format!("content {pattern_id} does not exist; call project_summary for the content ids")
    })?;
    let tpb = ticks_per_bar(project.time_sig_num);
    let new = notes::parse_notes(text, tpb, pattern.length_ticks())?;
    let mut edits = Vec::new();
    let mut diff = Vec::new();
    if replace {
        let old = content_view(project, pattern).other;
        if !old.is_empty() {
            diff.push(format!("P{pattern_id}: removed {} note(s)", old.len()));
            edits.push(Edit::RemoveNotes {
                pattern: pattern_id,
                notes: old.iter().map(|n| n.id).collect(),
            });
        }
    }
    diff.push(added_line(pattern_id, &new, tpb));
    edits.push(Edit::AddNotes {
        pattern: pattern_id,
        notes: new,
    });
    Ok((edits, diff))
}

/// "P7: added 3 note(s) C2:0:1/4 ..."
pub fn added_line(pattern: PatternId, new: &[NewNote], tpb: u32) -> String {
    let shown: Vec<String> = new
        .iter()
        .take(12)
        .map(|n| {
            let mut s = format!(
                "{}:{}:{}",
                notes::key_name(n.key),
                notes::fraction(n.start, tpb),
                notes::fraction(n.len, tpb)
            );
            if n.vel != notes::DEFAULT_VEL {
                s.push_str(&format!(":{}", n.vel));
            }
            s
        })
        .collect();
    format!(
        "P{pattern}: added {} note(s) {}{}",
        new.len(),
        shown.join(" "),
        if new.len() > shown.len() { " ..." } else { "" }
    )
}

/// A one-line description per edit, runs of the same kind merged.
pub fn describe_edits(edits: &[Edit]) -> Vec<String> {
    let mut lines: Vec<(String, usize, String)> = Vec::new();
    for e in edits {
        let v = serde_json::to_value(e).unwrap_or(Value::Null);
        let kind = v["edit"].as_str().unwrap_or("edit").to_string();
        let mut args = Vec::new();
        if let Value::Object(m) = &v {
            for (k, val) in m {
                if k == "edit" {
                    continue;
                }
                args.push(match val {
                    Value::Array(a) => format!("{k}=[{}]", a.len()),
                    Value::Object(o) if o.len() > 3 => format!("{k}={{..}}"),
                    Value::String(s) => format!("{k}={}", quoted(s)),
                    other => format!("{k}={other}"),
                });
            }
        }
        match lines.last_mut() {
            Some((k, n, _)) if *k == kind => *n += 1,
            _ => lines.push((kind, 1, args.join(" "))),
        }
    }
    cap(lines
        .into_iter()
        .map(|(k, n, a)| {
            if n > 1 {
                format!("{k} x{n} (first: {a})")
            } else {
                format!("{k} {a}")
            }
        })
        .collect())
}

/// Limits a diff to `MAX_DIFF_LINES` lines.
pub fn cap(mut lines: Vec<String>) -> Vec<String> {
    if lines.len() > MAX_DIFF_LINES {
        let more = lines.len() - MAX_DIFF_LINES;
        lines.truncate(MAX_DIFF_LINES);
        lines.push(format!("... and {more} more"));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::ids::{NoteId, TrackId};
    use protocol::model::{Channel, Clip, Instrument, Mix, Note, Pattern};
    use std::sync::Arc;

    fn project() -> Project {
        let mut p = Project::empty();
        p.channels.push(Arc::new(Channel {
            id: ChannelId(1),
            name: "kick".into(),
            root_key: 36,
            track: TrackId::MASTER,
            mix: Mix::default(),
            instrument: Instrument::Synth(Default::default()),
            choke_group: 0,
        }));
        let mut pat = Pattern::new(PatternId(2), "Kick 1".into(), ChannelId(1));
        pat.notes = vec![
            Note {
                id: NoteId(10),
                start: 0,
                len: 240,
                key: 36,
                vel: 100,
                off: 0,
                repeat: 1,
            },
            Note {
                id: NoteId(11),
                start: 960,
                len: 480,
                key: 40,
                vel: 100,
                off: 0,
                repeat: 1,
            },
        ];
        p.patterns.push(Arc::new(pat));
        p.clips.push(Clip {
            id: ClipId(3),
            instrument: ChannelId(1),
            pattern: PatternId(2),
            start: 0,
            len: 3840,
            offset: 0,
            muted: false,
            audio: None,
            group: None,
        });
        p
    }

    fn row(pattern: u32, grid: &str) -> GridRow<'_> {
        GridRow {
            pattern: PatternId(pattern),
            grid,
            vel: None,
            ratchet: None,
        }
    }

    #[test]
    fn targets_resolve_by_clip_content_or_single_instrument() {
        let p = project();
        let t = |clip: Option<u32>, content: Option<u32>, instrument: Option<u32>| Target {
            clip: clip.map(ClipId),
            content: content.map(PatternId),
            instrument: instrument.map(ChannelId),
        };
        assert_eq!(resolve(&p, t(Some(3), None, None)), Ok(PatternId(2)));
        assert_eq!(resolve(&p, t(None, Some(2), None)), Ok(PatternId(2)));
        assert_eq!(resolve(&p, t(None, None, Some(1))), Ok(PatternId(2)));
        assert!(
            resolve(&p, t(Some(9), None, None))
                .unwrap_err()
                .contains("clip 9")
        );
        assert!(
            resolve(&p, t(None, None, None))
                .unwrap_err()
                .contains("exactly one")
        );
        assert!(resolve(&p, t(Some(3), Some(2), None)).is_err());
    }

    #[test]
    fn grid_edits_and_diff() {
        let p = project();
        let (edits, diff) = grid_edits(&p, &[row(2, "x...x...x...x...")]).unwrap();
        // Step 0 already on; steps 4, 8, 12 are added.
        assert_eq!(edits.len(), 3);
        assert_eq!(diff.len(), 1);
        assert!(
            diff[0].contains("x...|....|....|.... -> x...|x...|x...|x..."),
            "{diff:?}"
        );
        let (none, d) = grid_edits(&p, &[row(2, "x...............")]).unwrap();
        assert!(none.is_empty() && d.is_empty());
    }

    #[test]
    fn grid_errors_name_the_row() {
        let p = project();
        let e = grid_edits(&p, &[row(2, "x"), row(2, "x")]).unwrap_err();
        assert!(
            e.starts_with("row 0 (content 2): the grid has 1 steps"),
            "{e}"
        );
        let e = grid_edits(
            &p,
            &[row(2, "................"), row(2, "................")],
        )
        .unwrap_err();
        assert!(e.contains("row 1") && e.contains("twice"), "{e}");
        let e = grid_edits(&p, &[row(5, "x")]).unwrap_err();
        assert!(
            e.contains("content 5") && e.contains("does not exist"),
            "{e}"
        );
    }

    #[test]
    fn notes_edits_replace_removes_only_piano_roll_notes() {
        let p = project();
        let (edits, diff) = notes_edits(&p, PatternId(2), "C2:0:1/4 E2:1/4:1/8:90", true).unwrap();
        assert_eq!(edits.len(), 2);
        let Edit::RemoveNotes { notes, .. } = &edits[0] else {
            panic!()
        };
        assert_eq!(notes, &vec![NoteId(11)]);
        assert!(
            diff.iter().any(|l| l.contains("added 2 note(s)")),
            "{diff:?}"
        );
        let e = notes_edits(&p, PatternId(2), "C2:1:1/4", false).unwrap_err();
        assert!(e.contains("end of the content"), "{e}");
    }

    #[test]
    fn describe_merges_runs_and_caps() {
        let edits: Vec<Edit> = (0..16)
            .map(|step| Edit::SetStep {
                pattern: PatternId(1),
                step,
                on: true,
                vel: None,
            })
            .collect();
        let d = describe_edits(&edits);
        assert_eq!(d.len(), 1);
        assert!(d[0].starts_with("set_step x16"), "{d:?}");
        let many: Vec<String> = (0..30).map(|i| i.to_string()).collect();
        let c = cap(many);
        assert_eq!(c.len(), 25);
        assert_eq!(c[24], "... and 6 more");
    }
}
