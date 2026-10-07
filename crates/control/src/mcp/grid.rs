// SPDX-License-Identifier: GPL-3.0-or-later
//! The compact step-grid text used by `beat_grid_set` and `beat_grid_get`
//! (SPEC 18.4).
//!
//! # Grammar
//!
//! A grid is one character per step of a clip content, first step first:
//!
//! | char | meaning |
//! | --- | --- |
//! | `.` | step off |
//! | `x` | hit at the row velocity (default 100) |
//! | `X` | accent: hit at velocity 120 |
//! | `2` `3` `4` `6` `8` | hit at the row velocity, ratcheted into that many notes |
//! | `\|` | visual separator (this server writes one every 4 steps), ignored; spaces are ignored too |
//!
//! After removing `|` and spaces the number of characters must equal the
//! content's `length_steps`. A row may also carry `ratchet` (2, 3, 4, 6 or 8),
//! the ratchet count given to every hit that has no digit of its own, so an
//! accent can be ratcheted (`X` with row ratchet 2). `beat_grid_get` shows a
//! ratcheted hit as its digit even when it is also loud, so an accent that is
//! ratcheted reads back as a digit.
//!
//! Example, one bar of 16 steps: `x...|x...|x...|x.x.` or `x...x...x...x.x.`.

use protocol::consts::RATCHETS;
use protocol::edit::Edit;
use protocol::ids::PatternId;
use protocol::model::{Pattern, Project};

/// Velocity of an `X` hit.
pub const ACCENT_VEL: u8 = 120;
/// Velocity of an `x` hit unless the row says otherwise.
pub const DEFAULT_VEL: u8 = 100;
/// Steps per `|`-separated group in text this server writes (a beat of sixteenths).
pub const GROUP: usize = 4;

/// The text of the grammar, shared by the tool descriptions.
pub const GRAMMAR: &str = "Grid text: one character per step, first step first. '.' = off, 'x' = hit, 'X' = accent (velocity 120), \
digit 2, 3, 4, 6 or 8 = hit ratcheted into that many notes. '|' (and spaces) may separate bars and are ignored. \
After removing them the length must equal the content's length in steps (16 = one bar of sixteenths). \
Example: \"x...|x...|x...|x.x.\". The row's `vel` (1-127, default 100) is the velocity of x and of ratcheted hits; \
the row's `ratchet` (2, 3, 4, 6, 8) is applied to every x or X that has no digit of its own.";

/// One step that is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hit {
    pub accent: bool,
    /// Ratchet count written in the grid (a digit), if any.
    pub ratchet: Option<u8>,
}

pub type Row = Vec<Option<Hit>>;

/// Parses a grid. `expected` is the content's `length_steps`; the error
/// says exactly what is wrong.
pub fn parse_grid(text: &str, expected: usize, pattern: PatternId) -> Result<Row, String> {
    let mut row: Row = Vec::new();
    for c in text.chars() {
        if c == '|' || c == ' ' {
            continue;
        }
        let step = row.len();
        row.push(match c {
            '.' => None,
            'x' => Some(Hit {
                accent: false,
                ratchet: None,
            }),
            'X' => Some(Hit {
                accent: true,
                ratchet: None,
            }),
            '2' | '3' | '4' | '6' | '8' => Some(Hit {
                accent: false,
                ratchet: Some(c.to_digit(10).expect("digit") as u8),
            }),
            '0' | '1' | '5' | '7' | '9' => {
                return Err(format!(
                    "ratchet count {c} at step {step} is not allowed: use 2, 3, 4, 6 or 8 (or x for a plain hit, . for off)"
                ));
            }
            other => {
                let shown: String = other.escape_default().collect();
                return Err(format!(
                    "grid character '{shown}' at step {step} is not allowed: use '.' off, 'x' hit, 'X' accent, a digit 2, 3, 4, 6 or 8 for a ratchet; '|' and spaces are ignored"
                ));
            }
        });
    }
    if row.is_empty() {
        return Err(
            "the grid is empty: write one character per step, for example \"x...x...x...x...\""
                .into(),
        );
    }
    let n = row.len();
    if n != expected {
        let fix = if n < expected {
            format!("add {} more step(s)", expected - n)
        } else {
            format!("remove {} step(s)", n - expected)
        };
        return Err(format!(
            "the grid has {n} steps but content {pattern} has {expected}: {fix} (or change the content length first with content_set)"
        ));
    }
    Ok(row)
}

/// Text form of a row, with a `|` after every `bar` steps when given.
pub fn format_grid(row: &Row, bar: Option<usize>) -> String {
    let mut s = String::with_capacity(row.len() + row.len() / 4);
    for (i, step) in row.iter().enumerate() {
        if let Some(b) = bar
            && b > 0
            && i > 0
            && i % b == 0
        {
            s.push('|');
        }
        s.push(match step {
            None => '.',
            Some(Hit {
                ratchet: Some(r), ..
            }) => char::from_digit(*r as u32, 10).unwrap_or('x'),
            Some(Hit { accent: true, .. }) => 'X',
            Some(Hit { accent: false, .. }) => 'x',
        });
    }
    s
}

/// What the document holds at one step of a row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Existing {
    pub vel: u8,
    pub repeat: u8,
}

/// The step notes of a clip content, by step. A content holds one
/// instrument's notes (SPEC 20.2); steps play that instrument's root key.
pub fn read_existing(project: &Project, pattern: &Pattern) -> Vec<Option<Existing>> {
    let mut out = vec![None; pattern.length_steps as usize];
    let Some(ch) = project.channel(pattern.instrument) else {
        return out;
    };
    for n in &pattern.notes {
        if n.is_step_note(ch.root_key, pattern) {
            let step = (n.start / pattern.step_ticks) as usize;
            if let Some(slot) = out.get_mut(step) {
                *slot = Some(Existing {
                    vel: n.vel,
                    repeat: n.repeat,
                });
            }
        }
    }
    out
}

/// A row as `beat_grid_get` shows it, plus the velocity of its plain hits.
pub fn existing_to_row(existing: &[Option<Existing>]) -> (Row, u8) {
    let mut counts: Vec<(u8, usize)> = Vec::new();
    let row = existing
        .iter()
        .map(|e| {
            e.map(|e| {
                let accent = e.vel >= ACCENT_VEL;
                if !accent {
                    match counts.iter_mut().find(|(v, _)| *v == e.vel) {
                        Some((_, c)) => *c += 1,
                        None => counts.push((e.vel, 1)),
                    }
                }
                Hit {
                    accent,
                    ratchet: (e.repeat > 1).then_some(e.repeat),
                }
            })
        })
        .collect();
    let vel = counts
        .iter()
        .max_by_key(|(v, c)| (*c, *v))
        .map(|(v, _)| *v)
        .unwrap_or(DEFAULT_VEL);
    (row, vel)
}

/// What a step should look like in the document.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Wanted {
    pub vel: u8,
    pub repeat: u8,
}

pub fn wanted(row: &Row, vel: u8, default_ratchet: Option<u8>) -> Vec<Option<Wanted>> {
    row.iter()
        .map(|s| {
            s.map(|h| Wanted {
                vel: if h.accent { ACCENT_VEL } else { vel },
                repeat: h.ratchet.or(default_ratchet).unwrap_or(1),
            })
        })
        .collect()
}

/// Validates the row-level arguments.
pub fn check_row_args(vel: Option<u8>, ratchet: Option<u8>) -> Result<(), String> {
    if let Some(v) = vel
        && !(1..=127).contains(&v)
    {
        return Err(format!("vel {v} is out of range, use 1 to 127"));
    }
    if let Some(r) = ratchet
        && !(RATCHETS.contains(&r) && r > 1)
    {
        return Err(format!(
            "ratchet {r} is not allowed: use 2, 3, 4, 6 or 8 (omit it for no ratchet)"
        ));
    }
    Ok(())
}

/// The edits that turn `existing` into `wanted`. Steps that already match
/// are skipped; a step that changes is turned off and on again so its
/// velocity and ratchet are exactly as wanted. Empty if nothing changes.
pub fn row_edits(
    pattern: PatternId,
    existing: &[Option<Existing>],
    wanted: &[Option<Wanted>],
) -> Vec<Edit> {
    let mut edits = Vec::new();
    for (i, (have, want)) in existing.iter().zip(wanted).enumerate() {
        let step = i as u8;
        let same = match (have, want) {
            (None, None) => true,
            (Some(h), Some(w)) => h.vel == w.vel && h.repeat == w.repeat,
            _ => false,
        };
        if same {
            continue;
        }
        if have.is_some() {
            edits.push(Edit::SetStep {
                pattern,
                step,
                on: false,
                vel: None,
            });
        }
        if let Some(w) = want {
            edits.push(Edit::SetStep {
                pattern,
                step,
                on: true,
                vel: Some(w.vel),
            });
            if w.repeat > 1 {
                edits.push(Edit::SetStepLanes {
                    pattern,
                    step,
                    vel: None,
                    off: None,
                    repeat: Some(w.repeat),
                });
            }
        }
    }
    edits
}

#[cfg(test)]
mod tests {
    use super::*;

    const P: PatternId = PatternId(7);

    #[test]
    fn parses_every_symbol_and_ignores_bars_and_spaces() {
        let r = parse_grid("x.X.|2.3.|4.6.|8.x.", 16, P).unwrap();
        assert_eq!(r.len(), 16);
        assert_eq!(
            r[0],
            Some(Hit {
                accent: false,
                ratchet: None
            })
        );
        assert_eq!(r[1], None);
        assert!(r[2].unwrap().accent);
        assert_eq!(r[4].unwrap().ratchet, Some(2));
        assert_eq!(r[6].unwrap().ratchet, Some(3));
        assert_eq!(r[8].unwrap().ratchet, Some(4));
        assert_eq!(r[10].unwrap().ratchet, Some(6));
        assert_eq!(r[12].unwrap().ratchet, Some(8));
        assert_eq!(parse_grid("x... x... x... x...", 16, P).unwrap().len(), 16);
    }

    #[test]
    fn round_trips() {
        for text in [
            "x...x...x...x...",
            "X.x.2.3.4.6.8.x.",
            "................",
            "xxxxxxxxxxxxxxxx",
        ] {
            let r = parse_grid(text, 16, P).unwrap();
            assert_eq!(format_grid(&r, None), text);
            let barred = format_grid(&r, Some(4));
            assert_eq!(parse_grid(&barred, 16, P).unwrap(), r);
        }
        assert_eq!(
            format_grid(&parse_grid("x...x...", 8, P).unwrap(), Some(4)),
            "x...|x..."
        );
    }

    #[test]
    fn errors_say_exactly_what_is_wrong() {
        let e = parse_grid("x..q", 4, P).unwrap_err();
        assert!(e.contains("'q'") && e.contains("step 3"), "{e}");
        let e = parse_grid("x.5.", 4, P).unwrap_err();
        assert!(
            e.contains("ratchet count 5") && e.contains("2, 3, 4, 6 or 8"),
            "{e}"
        );
        let e = parse_grid("x...", 16, P).unwrap_err();
        assert!(
            e.contains("4 steps") && e.contains("content 7 has 16") && e.contains("add 12"),
            "{e}"
        );
        let e = parse_grid("x.......x.......x", 16, P).unwrap_err();
        assert!(e.contains("17 steps") && e.contains("remove 1"), "{e}");
        let e = parse_grid("| |", 16, P).unwrap_err();
        assert!(e.contains("empty"), "{e}");
        let e = parse_grid("x\n..", 3, P).unwrap_err();
        assert!(e.contains("\\n"), "{e}");
    }

    #[test]
    fn row_args_are_checked() {
        assert!(check_row_args(Some(100), Some(2)).is_ok());
        assert!(
            check_row_args(Some(0), None)
                .unwrap_err()
                .contains("1 to 127")
        );
        assert!(
            check_row_args(None, Some(5))
                .unwrap_err()
                .contains("2, 3, 4, 6 or 8")
        );
        assert!(check_row_args(None, Some(1)).is_err());
    }

    #[test]
    fn wanted_applies_accent_velocity_and_row_ratchet() {
        let r = parse_grid("xX3.", 4, P).unwrap();
        let w = wanted(&r, 90, Some(2));
        assert_eq!(w[0], Some(Wanted { vel: 90, repeat: 2 }));
        assert_eq!(
            w[1],
            Some(Wanted {
                vel: 120,
                repeat: 2
            })
        );
        assert_eq!(w[2], Some(Wanted { vel: 90, repeat: 3 }));
        assert_eq!(w[3], None);
    }

    #[test]
    fn edits_skip_unchanged_steps_and_rewrite_changed_ones() {
        let ex = Existing {
            vel: 100,
            repeat: 1,
        };
        let existing = vec![Some(ex), None, Some(ex), None];
        let r = parse_grid("x.2X", 4, P).unwrap();
        let e = row_edits(P, &existing, &wanted(&r, 100, None));
        assert_eq!(e.len(), 4);
        assert!(matches!(
            e[0],
            Edit::SetStep {
                step: 2,
                on: false,
                ..
            }
        ));
        assert!(matches!(
            e[1],
            Edit::SetStep {
                step: 2,
                on: true,
                vel: Some(100),
                ..
            }
        ));
        assert!(matches!(
            e[2],
            Edit::SetStepLanes {
                step: 2,
                repeat: Some(2),
                ..
            }
        ));
        assert!(matches!(
            e[3],
            Edit::SetStep {
                step: 3,
                on: true,
                vel: Some(120),
                ..
            }
        ));
        let same = wanted(&parse_grid("x.x.", 4, P).unwrap(), 100, None);
        assert!(row_edits(P, &existing, &same).is_empty());
    }

    #[test]
    fn existing_rows_read_back() {
        let ex = vec![
            Some(Existing {
                vel: 100,
                repeat: 1,
            }),
            None,
            Some(Existing {
                vel: 125,
                repeat: 1,
            }),
            Some(Existing { vel: 90, repeat: 3 }),
        ];
        let (row, vel) = existing_to_row(&ex);
        assert_eq!(format_grid(&row, None), "x.X3");
        assert_eq!(vel, 100);
    }
}
