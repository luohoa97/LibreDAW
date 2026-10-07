// SPDX-License-Identifier: GPL-3.0-or-later
//! Turns the compact text arguments of the efficient tools (18.4) into
//! `Edit` batches plus a compact diff. Pure: works on a `Project` snapshot.

use std::collections::HashSet;

use protocol::edit::Edit;
use protocol::ids::{ChannelId, PatternId};
use protocol::model::{Project, ticks_per_bar};
use serde_json::Value;

use super::grid;
use super::notes;
use super::summary::{channel_view, quoted};
use super::tools::RowArg;

/// Most diff lines returned; the rest is summarised.
const MAX_DIFF_LINES: usize = 24;

/// `beat_grid_set`: edits for every row that changes, and one diff line per
/// changed row. Errors name the row.
pub fn grid_edits(
    project: &Project,
    pattern_id: PatternId,
    rows: &[RowArg],
) -> Result<(Vec<Edit>, Vec<String>), String> {
    let pattern = project.pattern(pattern_id).ok_or_else(|| {
        format!("pattern {pattern_id} does not exist; call project_summary for the pattern ids")
    })?;
    let len = pattern.length_steps as usize;
    let bar = grid::GROUP;
    let mut seen = HashSet::new();
    let mut edits = Vec::new();
    let mut diff = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        let ctx = |m: String| format!("row {i} (channel {}): {m}", row.channel);
        let channel = project.channel(row.channel).ok_or_else(|| {
            ctx("the channel does not exist; call project_summary for the channel ids".into())
        })?;
        if !seen.insert(row.channel) {
            return Err(ctx("the channel appears twice in rows".into()));
        }
        let parsed = grid::parse_grid(&row.grid, len, pattern_id).map_err(&ctx)?;
        let existing = grid::read_existing(project, pattern, row.channel);
        let vel = row.vel.unwrap_or(grid::DEFAULT_VEL);
        let wanted = grid::wanted(&parsed, vel, row.ratchet);
        let row_edits = grid::row_edits(pattern_id, row.channel, &existing, &wanted);
        if row_edits.is_empty() {
            continue;
        }
        let (old_row, _) = grid::existing_to_row(&existing);
        let new_row = new_row_text(&wanted, &parsed);
        diff.push(format!(
            "pattern {pattern_id} {} {} -> {}",
            quoted(&channel.name),
            grid::format_grid(&old_row, Some(bar)),
            grid::format_grid(&new_row, Some(bar)),
        ));
        edits.extend(row_edits);
    }
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

/// `notes_write`: remove (optional) and add notes.
pub fn notes_edits(
    project: &Project,
    pattern_id: PatternId,
    channel_id: ChannelId,
    text: &str,
    replace: bool,
) -> Result<(Vec<Edit>, Vec<String>), String> {
    let pattern = project.pattern(pattern_id).ok_or_else(|| {
        format!("pattern {pattern_id} does not exist; call project_summary for the pattern ids")
    })?;
    let channel = project.channel(channel_id).ok_or_else(|| {
        format!("channel {channel_id} does not exist; call project_summary for the channel ids")
    })?;
    let tpb = ticks_per_bar(project.time_sig_num);
    let new = notes::parse_notes(text, tpb, pattern.length_ticks())?;
    let mut edits = Vec::new();
    let mut diff = Vec::new();
    if replace {
        let old: Vec<_> = channel_view(project, pattern, channel_id).other;
        if !old.is_empty() {
            diff.push(format!(
                "pattern {pattern_id} {}: removed {} note(s)",
                quoted(&channel.name),
                old.len()
            ));
            edits.push(Edit::RemoveNotes {
                pattern: pattern_id,
                notes: old.iter().map(|n| n.id).collect(),
            });
        }
    }
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
    diff.push(format!(
        "pattern {pattern_id} {}: added {} note(s) {}{}",
        quoted(&channel.name),
        new.len(),
        shown.join(" "),
        if new.len() > shown.len() { " ..." } else { "" }
    ));
    edits.push(Edit::AddNotes {
        pattern: pattern_id,
        channel: channel_id,
        notes: new,
    });
    Ok((edits, diff))
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
    use protocol::model::{Channel, ChannelNotes, Instrument, Mix, Note, Pattern};
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
        let mut pat = Pattern::new(PatternId(2), "Beat".into());
        pat.notes.push(ChannelNotes {
            channel: ChannelId(1),
            notes: vec![
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
            ],
        });
        p.patterns.push(Arc::new(pat));
        p
    }

    fn row(channel: u32, grid: &str) -> RowArg {
        RowArg {
            channel: ChannelId(channel),
            grid: grid.into(),
            vel: None,
            ratchet: None,
        }
    }

    #[test]
    fn grid_edits_and_diff() {
        let p = project();
        let (edits, diff) = grid_edits(&p, PatternId(2), &[row(1, "x...x...x...x...")]).unwrap();
        // Step 0 already on; steps 4, 8, 12 are added.
        assert_eq!(edits.len(), 3);
        assert_eq!(diff.len(), 1);
        assert!(
            diff[0].contains("x...|....|....|.... -> x...|x...|x...|x..."),
            "{:?}",
            diff
        );
        let (none, d) = grid_edits(&p, PatternId(2), &[row(1, "x...............")]).unwrap();
        assert!(none.is_empty() && d.is_empty());
    }

    #[test]
    fn grid_errors_name_the_row() {
        let p = project();
        let e = grid_edits(&p, PatternId(2), &[row(1, "x"), row(1, "x")]).unwrap_err();
        assert!(
            e.starts_with("row 0 (channel 1): the grid has 1 steps"),
            "{e}"
        );
        let e = grid_edits(&p, PatternId(2), &[row(9, "x...............")]).unwrap_err();
        assert!(
            e.contains("row 0 (channel 9)") && e.contains("does not exist"),
            "{e}"
        );
        let e = grid_edits(
            &p,
            PatternId(2),
            &[row(1, "................"), row(1, "................")],
        )
        .unwrap_err();
        assert!(e.contains("row 1") && e.contains("twice"), "{e}");
        let e = grid_edits(&p, PatternId(5), &[row(1, "x")]).unwrap_err();
        assert!(e.contains("pattern 5 does not exist"), "{e}");
    }

    #[test]
    fn notes_edits_replace_removes_only_piano_roll_notes() {
        let p = project();
        let (edits, diff) = notes_edits(
            &p,
            PatternId(2),
            ChannelId(1),
            "C2:0:1/4 E2:1/4:1/8:90",
            true,
        )
        .unwrap();
        assert_eq!(edits.len(), 2);
        let Edit::RemoveNotes { notes, .. } = &edits[0] else {
            panic!()
        };
        assert_eq!(notes, &vec![NoteId(11)]);
        assert!(
            diff.iter().any(|l| l.contains("added 2 note(s)")),
            "{diff:?}"
        );
        let e = notes_edits(&p, PatternId(2), ChannelId(1), "C2:1:1/4", false).unwrap_err();
        assert!(e.contains("end of the pattern"), "{e}");
    }

    #[test]
    fn describe_merges_runs_and_caps() {
        let edits: Vec<Edit> = (0..16)
            .map(|step| Edit::SetStep {
                pattern: PatternId(1),
                channel: ChannelId(1),
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
