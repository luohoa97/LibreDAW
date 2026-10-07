// SPDX-License-Identifier: GPL-3.0-or-later
//! The compact note text used by `notes_write` (SPEC 18.4).
//!
//! # Grammar
//!
//! Notes are separated by spaces, commas, semicolons or newlines. One note:
//!
//! ```text
//! <pitch><octave>:<start>:<length>[:<vel>]
//! ```
//!
//! - `pitch`: a letter `A` to `G` (either case), optionally followed by `#`
//!   (sharp) or `b` (flat). Several pitches joined by `+` make a chord with
//!   the same start, length and velocity (`C3+E3+G3:0:1/2`).
//! - `octave`: an integer, `-1` to `9`. `C4` is MIDI 60 (middle C), `C-1` is
//!   0, so the MIDI number is `12 * (octave + 1) + semitone`.
//! - `start` and `length`: a fraction of a bar counted from the start of the
//!   content: `1/4` is a quarter of a bar (one beat in 4/4), `0` is the
//!   start, `1` is one whole bar. A decimal (`0.25`) works too. A trailing
//!   `t` means ticks instead (`240t`, 960 ticks per quarter note). The value
//!   must be a whole number of ticks.
//! - `vel`: 1 to 127, default 100.
//!
//! Example, in 4/4: `C3:0:1/4 E3:1/4:1/8 G3:3/8:1/8:90 C2+G2:1/2:1/2`.

use protocol::edit::NewNote;
use protocol::model::Note;

pub const DEFAULT_VEL: u8 = 100;
pub const MAX_NOTES: usize = 4096;

pub const GRAMMAR: &str = "Note text: notes separated by spaces, commas or newlines; each note is <pitch><octave>:<start>:<length>[:<vel>]. \
pitch = letter A-G (+ '#' sharp or 'b' flat); join pitches with '+' for a chord (C3+E3+G3:0:1/2). Octave -1 to 9, C4 = MIDI 60 (middle C), C2 = 36. \
start and length are fractions of a BAR from the start of the clip (its content): 1/4 = one beat in 4/4, 0 = the start, 1 = a whole bar; decimals (0.25) and ticks \
with a 't' suffix (240t, 960 ticks per quarter note) also work, and the value must be a whole number of ticks. vel 1-127, default 100. \
Example (4/4): \"C3:0:1/4 E3:1/4:1/8 G3:3/8:1/8:90\".";

const NAMES: [&str; 12] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];

/// `60` -> `C4`.
pub fn key_name(key: u8) -> String {
    format!("{}{}", NAMES[(key % 12) as usize], (key / 12) as i32 - 1)
}

fn parse_pitch(p: &str, token: &str) -> Result<u8, String> {
    let mut chars = p.chars();
    let letter = chars
        .next()
        .ok_or_else(|| format!("note '{token}' has an empty pitch"))?;
    let base: i32 = match letter.to_ascii_uppercase() {
        'C' => 0,
        'D' => 2,
        'E' => 4,
        'F' => 5,
        'G' => 7,
        'A' => 9,
        'B' => 11,
        _ => {
            return Err(format!(
                "pitch '{p}' in '{token}' must start with a letter A to G (then optional # or b, then the octave, for example C#3 or Bb2)"
            ));
        }
    };
    let rest = chars.as_str();
    let (acc, octave_text) = match rest.chars().next() {
        Some('#') => (1, &rest[1..]),
        Some('b') => (-1, &rest[1..]),
        _ => (0, rest),
    };
    let octave: i32 = octave_text.parse().map_err(|_| {
        format!(
            "pitch '{p}' in '{token}' needs an octave number from -1 to 9 after the letter, for example C3 (C4 is middle C)"
        )
    })?;
    if !(-1..=9).contains(&octave) {
        return Err(format!(
            "octave {octave} in '{token}' is out of range: use -1 to 9 (C4 = 60)"
        ));
    }
    let key = 12 * (octave + 1) + base + acc;
    if !(0..=127).contains(&key) {
        return Err(format!(
            "pitch '{p}' in '{token}' is MIDI {key}, outside 0 to 127 (the highest note is G9)"
        ));
    }
    Ok(key as u8)
}

/// A value as a fraction `num/den` of a bar, or ticks.
enum Amount {
    Bars(u128, u128),
    Ticks(u64),
}

fn parse_amount(s: &str, what: &str, token: &str) -> Result<Amount, String> {
    let bad = || {
        format!(
            "{what} '{s}' in '{token}' is not a number: write a fraction of a bar like 1/4, 0, 1, a decimal like 0.25, or ticks like 240t"
        )
    };
    if let Some(t) = s.strip_suffix('t') {
        return t.parse::<u64>().map(Amount::Ticks).map_err(|_| bad());
    }
    if let Some((a, b)) = s.split_once('/') {
        let a: u128 = a.trim().parse().map_err(|_| bad())?;
        let b: u128 = b.trim().parse().map_err(|_| bad())?;
        if b == 0 {
            return Err(format!("{what} '{s}' in '{token}' divides by zero"));
        }
        return Ok(Amount::Bars(a, b));
    }
    if let Some((i, f)) = s.split_once('.') {
        if f.is_empty() || f.len() > 9 || !f.bytes().all(|c| c.is_ascii_digit()) {
            return Err(bad());
        }
        let i: u128 = if i.is_empty() {
            0
        } else {
            i.parse().map_err(|_| bad())?
        };
        let den = 10u128.pow(f.len() as u32);
        let frac: u128 = f.parse().map_err(|_| bad())?;
        return Ok(Amount::Bars(i * den + frac, den));
    }
    s.parse::<u128>()
        .map(|n| Amount::Bars(n, 1))
        .map_err(|_| bad())
}

fn to_ticks(
    a: Amount,
    ticks_per_bar: u32,
    what: &str,
    s: &str,
    token: &str,
) -> Result<u32, String> {
    let ticks = match a {
        Amount::Ticks(t) => t,
        Amount::Bars(n, d) => {
            let total = n * ticks_per_bar as u128;
            if !total.is_multiple_of(d) {
                return Err(format!(
                    "{what} '{s}' in '{token}' is {:.2} ticks (a bar is {ticks_per_bar} ticks), not a whole number: use a fraction like 1/4, 1/8, 1/16, 1/32 or a triplet such as 1/12, or add 't' for ticks",
                    total as f64 / d as f64
                ));
            }
            u64::try_from(total / d).unwrap_or(u64::MAX)
        }
    };
    u32::try_from(ticks).map_err(|_| format!("{what} '{s}' in '{token}' is too large"))
}

/// A time position or length written like the note text does it: a
/// fraction of a bar (`1/4`, `2`, `0.5`) or ticks (`240t`).
pub fn parse_ticks(s: &str, ticks_per_bar: u32, what: &str) -> Result<u32, String> {
    let s = s.trim();
    to_ticks(parse_amount(s, what, s)?, ticks_per_bar, what, s, s)
}

/// Parses note text. `pattern_ticks` is the content length, so a note that
/// starts after the end is reported.
pub fn parse_notes(
    text: &str,
    ticks_per_bar: u32,
    pattern_ticks: u32,
) -> Result<Vec<NewNote>, String> {
    let mut notes = Vec::new();
    for token in text.split(|c: char| c.is_whitespace() || c == ',' || c == ';') {
        if token.is_empty() {
            continue;
        }
        let parts: Vec<&str> = token.split(':').collect();
        if !(3..=4).contains(&parts.len()) {
            return Err(format!(
                "note '{token}' must be <pitch><octave>:<start>:<length>[:<vel>] with 3 or 4 parts separated by ':', for example C3:0:1/4 or C3:1/4:1/8:90"
            ));
        }
        let keys: Vec<u8> = parts[0]
            .split('+')
            .map(|p| parse_pitch(p, token))
            .collect::<Result<_, _>>()?;
        let start = to_ticks(
            parse_amount(parts[1], "start", token)?,
            ticks_per_bar,
            "start",
            parts[1],
            token,
        )?;
        let len = to_ticks(
            parse_amount(parts[2], "length", token)?,
            ticks_per_bar,
            "length",
            parts[2],
            token,
        )?;
        if len == 0 {
            return Err(format!(
                "length '{}' in '{token}' is zero: a note needs at least 1 tick",
                parts[2]
            ));
        }
        if start >= pattern_ticks {
            return Err(format!(
                "start '{}' in '{token}' is tick {start}, at or after the end of the content ({pattern_ticks} ticks = {} bar(s)): use a start before the end, or make the content longer first with content_set",
                parts[1],
                pattern_ticks as f64 / ticks_per_bar as f64
            ));
        }
        let vel = match parts.get(3) {
            None => DEFAULT_VEL,
            Some(v) => match v.parse::<u8>() {
                Ok(x) if (1..=127).contains(&x) => x,
                _ => {
                    return Err(format!(
                        "velocity '{v}' in '{token}' must be a whole number from 1 to 127"
                    ));
                }
            },
        };
        for key in keys {
            notes.push(NewNote {
                start,
                len,
                key,
                vel,
            });
        }
        if notes.len() > MAX_NOTES {
            return Err(format!(
                "more than {MAX_NOTES} notes in one call: split them into several parts or calls"
            ));
        }
    }
    if notes.is_empty() {
        return Err(
            "no notes found: write notes like \"C3:0:1/4 E3:1/4:1/8\" (<pitch><octave>:<start>:<length>[:<vel>])"
                .into(),
        );
    }
    Ok(notes)
}

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 { a } else { gcd(b, a % b) }
}

/// Ticks as a reduced fraction of a bar (`0`, `1/4`, `3`, `1/12`).
pub fn fraction(ticks: u32, ticks_per_bar: u32) -> String {
    if ticks == 0 {
        return "0".into();
    }
    let g = gcd(ticks as u64, ticks_per_bar as u64);
    let (n, d) = (ticks as u64 / g, ticks_per_bar as u64 / g);
    if d == 1 {
        n.to_string()
    } else {
        format!("{n}/{d}")
    }
}

/// Note text for `notes`, in the order given. Velocity is written only when
/// it is not the default.
pub fn format_notes(notes: &[Note], ticks_per_bar: u32) -> String {
    notes
        .iter()
        .map(|n| {
            let mut s = format!(
                "{}:{}:{}",
                key_name(n.key),
                fraction(n.start, ticks_per_bar),
                fraction(n.len, ticks_per_bar)
            );
            if n.vel != DEFAULT_VEL {
                s.push_str(&format!(":{}", n.vel));
            }
            s
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::ids::NoteId;

    const BAR: u32 = 3840;

    fn n(start: u32, len: u32, key: u8, vel: u8) -> NewNote {
        NewNote {
            start,
            len,
            key,
            vel,
        }
    }

    #[test]
    fn parses_pitches_fractions_and_velocity() {
        let v = parse_notes("C4:0:1/4 E3:1/4:1/8 G#3:3/8:1/8:90, Bb2:1/2:1/2", BAR, BAR).unwrap();
        assert_eq!(
            v,
            vec![
                n(0, 960, 60, 100),
                n(960, 480, 52, 100),
                n(1440, 480, 56, 90),
                n(1920, 1920, 46, 100)
            ]
        );
    }

    #[test]
    fn accepts_decimals_ticks_chords_and_lowercase() {
        let v = parse_notes("c3+e3+g3:0.25:240t\nC-1:0:1", BAR, BAR * 2).unwrap();
        assert_eq!(v.len(), 4);
        assert_eq!((v[0].key, v[1].key, v[2].key), (48, 52, 55));
        assert_eq!((v[0].start, v[0].len), (960, 240));
        assert_eq!((v[3].key, v[3].len), (0, 3840));
    }

    #[test]
    fn round_trips_through_text() {
        let text = "C4:0:1/4 E3:1/4:1/8 G#3:3/8:1/12:90 C2:1/2:1/2";
        let parsed = parse_notes(text, BAR, BAR).unwrap();
        let notes: Vec<Note> = parsed
            .iter()
            .enumerate()
            .map(|(i, p)| Note {
                id: NoteId(i as u32 + 1),
                start: p.start,
                len: p.len,
                key: p.key,
                vel: p.vel,
                off: 0,
                repeat: 1,
            })
            .collect();
        let back = format_notes(&notes, BAR);
        assert_eq!(back, text);
        assert_eq!(parse_notes(&back, BAR, BAR).unwrap(), parsed);
    }

    #[test]
    fn key_names() {
        assert_eq!(key_name(60), "C4");
        assert_eq!(key_name(36), "C2");
        assert_eq!(key_name(0), "C-1");
        assert_eq!(key_name(127), "G9");
        assert_eq!(key_name(61), "C#4");
    }

    #[test]
    fn errors_say_exactly_what_is_wrong() {
        let e = parse_notes("C3:0", BAR, BAR).unwrap_err();
        assert!(e.contains("'C3:0'") && e.contains("3 or 4 parts"), "{e}");
        let e = parse_notes("H3:0:1/4", BAR, BAR).unwrap_err();
        assert!(e.contains("letter A to G"), "{e}");
        let e = parse_notes("C:0:1/4", BAR, BAR).unwrap_err();
        assert!(e.contains("octave"), "{e}");
        let e = parse_notes("C12:0:1/4", BAR, BAR).unwrap_err();
        assert!(e.contains("-1 to 9"), "{e}");
        let e = parse_notes("B9:0:1/4", BAR, BAR).unwrap_err();
        assert!(e.contains("outside 0 to 127"), "{e}");
        let e = parse_notes("C3:1/7:1/4", BAR, BAR).unwrap_err();
        assert!(
            e.contains("not a whole number") && e.contains("548.57"),
            "{e}"
        );
        let e = parse_notes("C3:0:0", BAR, BAR).unwrap_err();
        assert!(e.contains("zero"), "{e}");
        let e = parse_notes("C3:1:1/4", BAR, BAR).unwrap_err();
        assert!(e.contains("end of the content"), "{e}");
        let e = parse_notes("C3:0:1/4:0", BAR, BAR).unwrap_err();
        assert!(e.contains("1 to 127"), "{e}");
        let e = parse_notes("C3:abc:1/4", BAR, BAR).unwrap_err();
        assert!(e.contains("not a number"), "{e}");
        let e = parse_notes("C3:1/0:1/4", BAR, BAR).unwrap_err();
        assert!(e.contains("divides by zero"), "{e}");
        let e = parse_notes("  ", BAR, BAR).unwrap_err();
        assert!(e.contains("no notes"), "{e}");
    }

    #[test]
    fn other_time_signatures_use_their_own_bar() {
        // 3/4: a bar is 2880 ticks, 1/3 bar = 960 ticks.
        let v = parse_notes("C3:1/3:1/3", 2880, 2880).unwrap();
        assert_eq!((v[0].start, v[0].len), (960, 960));
        assert_eq!(fraction(960, 2880), "1/3");
    }
}
