// SPDX-License-Identifier: GPL-3.0-or-later
//! Role, tags, tempo, key and root note from folder and file names.
//!
//! Pure functions on the path relative to the library root, with `/`
//! separators. No file access.

/// Roles a library entry can have (the `role` of `SoundInfo`).
pub const ROLES: [&str; 17] = [
    "kick",
    "snare",
    "hat",
    "clap",
    "perc",
    "cymbal",
    "tom",
    "808",
    "bass",
    "keys",
    "guitar",
    "orchestral",
    "loop",
    "riser",
    "sfx",
    "vocal",
    "fx",
];

/// Convention for note names in file names: `C4` is MIDI 60.
pub const MIDDLE_C_OCTAVE: i32 = 4;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Classified {
    pub role: &'static str,
    pub tags: Vec<String>,
    pub tempo_bpm: Option<u16>,
    pub key: Option<String>,
    /// Root note named in the file name (tonal roles only).
    pub root_note: Option<u8>,
    /// Kit folder (`Legacy/Drums/Kits/<kit>`).
    pub kit: Option<String>,
    /// Instrument a multisample belongs to: parent folder plus the name
    /// without its note or number.
    pub group: Option<String>,
    pub instrument: Option<String>,
    /// Number in the file name (`Jazz Guitar (3)`), for ordering.
    pub seq: Option<u32>,
}

pub fn is_tonal(role: &str) -> bool {
    matches!(role, "bass" | "keys" | "guitar" | "orchestral")
}

fn hard_folder_role(f: &str) -> Option<&'static str> {
    Some(match f {
        "loops" => "loop",
        "risers" => "riser",
        "sfx" => "sfx",
        "fx" => "fx",
        "vocals" => "vocal",
        "shapes" => "fx",
        _ => return None,
    })
}

fn soft_folder_role(f: &str) -> Option<&'static str> {
    Some(match f {
        "kicks" => "kick",
        "snares" => "snare",
        "hats" | "hi hats" => "hat",
        "claps" => "clap",
        "cymbals" => "cymbal",
        "toms" => "tom",
        "percussion" | "rims" | "shakers" | "foley" | "percs" | "hits" => "perc",
        "bass" => "bass",
        "guitar" => "guitar",
        "keyboard" | "piano" | "pianos" => "keys",
        "orchestral" | "strings" | "choirs" | "pads" | "brass" => "orchestral",
        _ => return None,
    })
}

fn tokens(s: &str) -> Vec<String> {
    s.split(|c: char| !(c.is_alphanumeric() || c == '#'))
        .filter(|t| !t.is_empty())
        .map(str::to_lowercase)
        .collect()
}

fn token_role(t: &str) -> Option<&'static str> {
    Some(match t {
        "kick" | "kicks" | "kik" | "bd" | "bassdrum" => "kick",
        "snare" | "snares" | "snr" => "snare",
        "clap" | "claps" => "clap",
        "hat" | "hats" | "hihat" | "hihats" | "hh" | "ch" | "oh" => "hat",
        "cymbal" | "cymbals" | "crash" | "ride" | "splash" | "china" => "cymbal",
        "tom" | "toms" => "tom",
        "rim" | "rims" | "rimshot" | "shaker" | "tamb" | "tambourine" | "cowbell" | "conga"
        | "bongo" | "clav" | "perc" | "snap" | "cabasa" | "guiro" | "triangle" | "block"
        | "maracas" => "perc",
        _ if t.starts_with("hat") && t.len() <= 6 => "hat",
        _ if t.starts_with("tom") && t.len() <= 5 => "tom",
        _ => return None,
    })
}

/// Parses `c4`, `a#2`, `f3ogg`.
fn parse_note(t: &str) -> Option<u8> {
    let t = t.strip_suffix("ogg").unwrap_or(t);
    let b = t.as_bytes();
    if b.len() < 2 || !(b'a'..=b'g').contains(&b[0]) {
        return None;
    }
    let mut pc = match b[0] {
        b'c' => 0,
        b'd' => 2,
        b'e' => 4,
        b'f' => 5,
        b'g' => 7,
        b'a' => 9,
        _ => 11,
    };
    let mut i = 1;
    if b[i] == b'#' {
        pc += 1;
        i += 1;
    } else if b[i] == b'b' && b.len() > i + 1 && b[i + 1].is_ascii_digit() {
        pc -= 1;
        i += 1;
    }
    let digits = &t[i..];
    if digits.is_empty() || digits.len() > 2 || !digits.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let oct: i32 = digits.parse().ok()?;
    let midi = (oct - MIDDLE_C_OCTAVE + 5) * 12 + pc;
    u8::try_from(midi).ok().filter(|&m| m <= 127)
}

fn parse_key(t: &str) -> Option<String> {
    let b = t.as_bytes();
    if b.len() < 2 || !(b'a'..=b'g').contains(&b[0]) {
        return None;
    }
    let (root, rest) = if b[1] == b'#' {
        (&t[..2], &t[2..])
    } else {
        (&t[..1], &t[1..])
    };
    let minor = match rest {
        "m" | "min" | "minor" => true,
        "maj" | "major" => false,
        _ => return None,
    };
    let mut r = root.to_uppercase();
    if minor {
        r.push('m');
    }
    Some(r)
}

fn parse_tempo(toks: &[String]) -> Option<u16> {
    let ok = |n: u16| (40..=300).contains(&n).then_some(n);
    for (i, t) in toks.iter().enumerate() {
        if let Some(n) = t.strip_suffix("bpm").and_then(|d| d.parse().ok()) {
            return ok(n);
        }
        if let Some(n) = t.strip_prefix("bpm").and_then(|d| d.parse().ok()) {
            return ok(n);
        }
        if t == "bpm" {
            if let Some(n) = i.checked_sub(1).and_then(|j| toks[j].parse().ok()) {
                return ok(n);
            }
            if let Some(n) = toks.get(i + 1).and_then(|d| d.parse().ok()) {
                return ok(n);
            }
        }
    }
    None
}

/// `rel` is the path under the library root, for example
/// `Drums/Kicks/808 Kick.wav`.
pub fn classify(rel: &str) -> Classified {
    let comps: Vec<&str> = rel.split('/').collect();
    let file = comps.last().copied().unwrap_or("");
    let stem = file.rsplit_once('.').map_or(file, |(s, _)| s);
    let folders: Vec<String> = comps[..comps.len().saturating_sub(1)]
        .iter()
        .map(|f| f.to_lowercase())
        .collect();
    let toks = tokens(stem);

    let hard = folders.iter().rev().find_map(|f| hard_folder_role(f));
    let role = hard
        .or_else(|| toks.iter().find_map(|t| token_role(t)))
        .or_else(|| folders.iter().rev().find_map(|f| soft_folder_role(f)))
        .or_else(|| {
            (toks.iter().any(|t| t == "808")
                && toks.iter().any(|t| matches!(t.as_str(), "bass" | "sub")))
            .then_some("808")
        })
        .unwrap_or("fx");

    let mut tags: Vec<String> = Vec::new();
    let mut push = |t: &str| {
        if !t.is_empty() && !tags.iter().any(|x| x == t) {
            tags.push(t.to_string());
        }
    };
    for f in &folders {
        // The root's own folder names are tags: "kicks", "legacy", ...
        for t in f
            .split(|c: char| !c.is_alphanumeric())
            .filter(|t| t.len() > 1)
        {
            if !matches!(t, "drums" | "instruments" | "kits") || f.contains("modeaudio") {
                push(t);
            }
        }
    }
    if toks.iter().any(|t| matches!(t.as_str(), "oh" | "open")) && role == "hat" {
        push("open");
    }
    if toks.iter().any(|t| matches!(t.as_str(), "ch" | "closed")) && role == "hat" {
        push("closed");
    }
    if toks
        .iter()
        .any(|t| matches!(t.as_str(), "rim" | "rims" | "rimshot"))
    {
        push("rim");
    }
    for t in &toks {
        if matches!(
            t.as_str(),
            "707" | "808" | "909" | "analog" | "acoustic" | "electric" | "reverse" | "rev"
        ) {
            push(t);
        }
    }
    push(if matches!(role, "loop") {
        "loop"
    } else if is_tonal(role) {
        "multisample"
    } else {
        "one-shot"
    });

    let tonal = is_tonal(role);
    let tempo_bpm = parse_tempo(&toks);
    let key = if tonal || role == "loop" {
        toks.iter().find_map(|t| parse_key(t))
    } else {
        None
    };
    let root_note = if tonal {
        toks.iter().rev().find_map(|t| parse_note(t))
    } else {
        None
    };

    let kit = comps
        .windows(2)
        .position(|w| w[0].eq_ignore_ascii_case("kits"))
        .filter(|i| i + 2 < comps.len())
        .map(|i| comps[i + 1].to_string());

    let in_instruments = folders.iter().any(|f| f == "instruments");
    let (group, instrument, seq) = if tonal && in_instruments {
        let words: Vec<&str> = stem
            .split(|c: char| !(c.is_alphanumeric() || c == '#'))
            .filter(|t| !t.is_empty())
            .collect();
        let seq = words.iter().rev().find_map(|w| w.parse::<u32>().ok());
        let name: Vec<&str> = words
            .iter()
            .copied()
            .filter(|w| {
                w.parse::<u32>().is_err()
                    && parse_note(&w.to_lowercase()).is_none()
                    && !w.eq_ignore_ascii_case("ogg")
            })
            .collect();
        let inst = name.join(" ");
        let parent = comps[..comps.len() - 1].join("/");
        (Some(format!("{parent}/{inst}")), Some(inst), seq)
    } else {
        (None, None, None)
    };

    Classified {
        role,
        tags,
        tempo_bpm,
        key,
        root_note,
        kit,
        group,
        instrument,
        seq,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drum_roles() {
        assert_eq!(classify("Drums/Kicks/808 Kick.wav").role, "kick");
        assert_eq!(classify("Drums/Snares/14in Rim.wav").role, "perc");
        assert_eq!(classify("Drums/Hats/909 OH.wav").role, "hat");
        assert!(
            classify("Drums/Hats/909 OH.wav")
                .tags
                .contains(&"open".into())
        );
        assert_eq!(classify("Drums/Percussion/909 Clap.wav").role, "clap");
        assert_eq!(classify("Drums/SFX/Alma Crash SFX.wav").role, "sfx");
        assert_eq!(
            classify("Drums (ModeAudio)/Hi Hats/Attack Hat 01.wv").role,
            "hat"
        );
        assert_eq!(
            classify("Legacy/Drums/Kits/Drum Kit 04/FLS_Hatpd 04.wav").role,
            "hat"
        );
    }

    #[test]
    fn folders_decide_the_rest() {
        assert_eq!(classify("Loops/DL Breaker.wav").role, "loop");
        assert_eq!(classify("Risers/Riser Cymbal.wv").role, "riser");
        assert_eq!(classify("Vocals/Laurie Webb Ahh A.wav").role, "vocal");
        assert_eq!(classify("Legacy/FX/FX_Scratch.wav").role, "fx");
        assert_eq!(
            classify("Instruments/Keyboard/Rhodes/Piano Electric (3).wav").role,
            "keys"
        );
    }

    #[test]
    fn notes_tempo_key() {
        let c = classify("Instruments/Orchestral/Strings Section/OSTR C2.wav");
        assert_eq!(c.role, "orchestral");
        assert_eq!(c.root_note, Some(36));
        assert_eq!(c.instrument.as_deref(), Some("OSTR"));
        assert_eq!(
            classify("Legacy/Instruments/Piano/Piano 1/X_D6OGG.wav").root_note,
            Some(86)
        );
        assert_eq!(
            classify("Legacy/Instruments/Bass/Bass_A#2.wav").root_note,
            Some(46)
        );
        assert_eq!(
            classify("Legacy/FX/Scratch hit 120bpm.wav").tempo_bpm,
            Some(120)
        );
        assert_eq!(classify("Loops/Dub 90 BPM Am.wav").tempo_bpm, Some(90));
        assert_eq!(
            classify("Loops/Dub 90 BPM Am.wav").key.as_deref(),
            Some("Am")
        );
    }

    #[test]
    fn numbered_multisample() {
        let c = classify("Instruments/Guitar/Jazz/Jazz Guitar (3).wav");
        assert_eq!(c.seq, Some(3));
        assert_eq!(c.root_note, None);
        assert_eq!(c.instrument.as_deref(), Some("Jazz Guitar"));
    }

    #[test]
    fn kit_folder() {
        let c = classify("Legacy/Drums/Kits/Drum Kit 03/OverH_Crash_002cogg.wav");
        assert_eq!(c.kit.as_deref(), Some("Drum Kit 03"));
    }
}
