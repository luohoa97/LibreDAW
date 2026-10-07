// SPDX-License-Identifier: GPL-3.0-or-later
//! The project file `project.toml` (SPEC 7.2, 7.3).
//!
//! Saving uses a hand-written emitter that writes one canonical text: fixed
//! key order, collections in id order, one note per line as an inline
//! table. Loading parses with `toml` and `serde`, checks the version first,
//! then validates. `parse(emit(p)) == p` and `emit(parse(t)) == t` for any
//! canonical text `t`.
//!
//! Plugin state blobs and the bundle layout (7.1, 7.4) are handled by the
//! `ui` crate; this module only reads and writes the TOML text.

use std::fmt::Write;
use std::sync::Arc;

use serde::Deserialize;

use crate::consts::FORMAT_VERSION;
use crate::model::{
    Adsr, Channel, ClapRef, Insert, Instrument, Metronome, Mix, Osc, Pattern, Project, SynthParams,
    Track, Wave,
};
use crate::validate::{ValidationError, validate};

#[derive(Debug, PartialEq)]
pub enum FormatError {
    /// No `format_version` key.
    MissingVersion,
    /// Written by a newer LibreDAW (7.3).
    TooNew { found: u32, supported: u32 },
    /// Not valid TOML, or keys and types do not match the schema.
    Syntax(String),
    /// Parsed, but breaks a project rule.
    Invalid(ValidationError),
}

impl std::fmt::Display for FormatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FormatError::MissingVersion => {
                write!(f, "not a LibreDAW project: format_version missing")
            }
            FormatError::TooNew { found, supported } => write!(
                f,
                "written by a newer LibreDAW (format {found}; this version reads up to {supported})"
            ),
            FormatError::Syntax(e) => write!(f, "project file error: {e}"),
            FormatError::Invalid(e) => write!(f, "invalid project: {e}"),
        }
    }
}

impl std::error::Error for FormatError {}

// ---------------------------------------------------------------------------
// Emitter

/// Writes the canonical project text. `next_id` is the document's id
/// counter (17.1). Fails only if the project does not validate.
pub fn emit(p: &Project, next_id: u32) -> Result<String, ValidationError> {
    validate(p)?;
    let mut o = String::with_capacity(4096 + p.note_count() * 64);
    let w = &mut o;
    line(w, format_args!("format_version = {FORMAT_VERSION}"));
    line(w, format_args!("next_id = {}", next_id.max(p.max_id() + 1)));
    line(w, format_args!("tempo_bpm = {}", float(p.tempo_bpm)));
    line(w, format_args!("time_sig_num = {}", p.time_sig_num));
    w.push_str("\n[metronome]\n");
    line(w, format_args!("enabled = {}", p.metronome.enabled));
    line(w, format_args!("gain_db = {}", float(p.metronome.gain_db)));

    for c in &p.channels {
        w.push_str("\n[[channels]]\n");
        line(w, format_args!("id = {}", c.id));
        line(w, format_args!("name = {}", string(&c.name)));
        line(w, format_args!("root_key = {}", c.root_key));
        line(w, format_args!("track = {}", c.track));
        line(w, format_args!("mix = {}", mix(&c.mix)));
        w.push_str("\n[channels.instrument]\n");
        match &c.instrument {
            Instrument::Synth(s) => emit_synth(w, s),
            Instrument::Clap(r) => emit_clap(w, r),
        }
    }

    for pat in &p.patterns {
        w.push_str("\n[[patterns]]\n");
        line(w, format_args!("id = {}", pat.id));
        line(w, format_args!("name = {}", string(&pat.name)));
        line(w, format_args!("length_steps = {}", pat.length_steps));
        line(w, format_args!("step_ticks = {}", pat.step_ticks));
        for cn in &pat.notes {
            w.push_str("\n[[patterns.notes]]\n");
            line(w, format_args!("channel = {}", cn.channel));
            w.push_str("notes = [\n");
            for n in &cn.notes {
                line(
                    w,
                    format_args!(
                        "  {{ id = {}, start = {}, len = {}, key = {}, vel = {} }},",
                        n.id, n.start, n.len, n.key, n.vel
                    ),
                );
            }
            w.push_str("]\n");
        }
    }

    for t in &p.tracks {
        w.push_str("\n[[tracks]]\n");
        line(w, format_args!("id = {}", t.id));
        line(w, format_args!("name = {}", string(&t.name)));
        line(w, format_args!("mix = {}", mix(&t.mix)));
        for Insert::Clap(r) in &t.inserts {
            w.push_str("\n[[tracks.inserts]]\n");
            emit_clap(w, r);
        }
    }
    Ok(o)
}

fn line(w: &mut String, args: std::fmt::Arguments<'_>) {
    w.write_fmt(args).expect("writing to a String cannot fail");
    w.push('\n');
}

/// Shortest round-trip form, always with a decimal point or exponent so
/// TOML reads it back as a float. `-0.0` is written as `0.0`. Callers
/// validate first, so NaN and infinity never reach here.
fn float(v: f64) -> String {
    debug_assert!(v.is_finite());
    let v = if v == 0.0 { 0.0 } else { v };
    let s = format!("{v:?}");
    if s.contains(['.', 'e', 'E']) {
        s
    } else {
        format!("{s}.0")
    }
}

/// TOML basic string with escapes.
fn string(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            c if c.is_control() => {
                let _ = write!(o, "\\u{:04X}", c as u32);
            }
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

fn mix(m: &Mix) -> String {
    format!(
        "{{ volume_db = {}, pan = {}, mute = {}, solo = {} }}",
        float(m.volume_db),
        float(m.pan),
        m.mute,
        m.solo
    )
}

fn wave(w: Wave) -> &'static str {
    match w {
        Wave::Sine => "sine",
        Wave::Saw => "saw",
        Wave::Square => "square",
        Wave::Triangle => "triangle",
    }
}

fn osc(o: &Osc) -> String {
    format!(
        "{{ wave = \"{}\", semitones = {}, cents = {} }}",
        wave(o.wave),
        float(o.semitones),
        float(o.cents)
    )
}

fn adsr(a: &Adsr) -> String {
    format!(
        "{{ attack_ms = {}, decay_ms = {}, sustain = {}, release_ms = {} }}",
        float(a.attack_ms),
        float(a.decay_ms),
        float(a.sustain),
        float(a.release_ms)
    )
}

fn emit_synth(w: &mut String, s: &SynthParams) {
    line(w, format_args!("kind = \"synth\""));
    line(w, format_args!("osc1 = {}", osc(&s.osc1)));
    line(w, format_args!("osc2 = {}", osc(&s.osc2)));
    line(w, format_args!("osc_mix = {}", float(s.osc_mix)));
    line(w, format_args!("cutoff_hz = {}", float(s.cutoff_hz)));
    line(w, format_args!("resonance = {}", float(s.resonance)));
    line(
        w,
        format_args!("filter_env_octaves = {}", float(s.filter_env_octaves)),
    );
    line(w, format_args!("amp_env = {}", adsr(&s.amp_env)));
    line(w, format_args!("filter_env = {}", adsr(&s.filter_env)));
    line(w, format_args!("gain_db = {}", float(s.gain_db)));
}

fn emit_clap(w: &mut String, r: &ClapRef) {
    line(w, format_args!("kind = \"clap\""));
    line(w, format_args!("instance = {}", r.instance));
    line(w, format_args!("plugin_id = {}", string(&r.plugin_id)));
    line(
        w,
        format_args!("plugin_version = {}", string(&r.plugin_version)),
    );
    if let Some(f) = &r.state_file {
        line(w, format_args!("state_file = {}", string(f)));
    }
    if !r.params.is_empty() {
        w.push_str("params = [\n");
        for p in &r.params {
            line(
                w,
                format_args!("  {{ id = {}, value = {} }},", p.id, float(p.value)),
            );
        }
        w.push_str("]\n");
    }
}

// ---------------------------------------------------------------------------
// Loader

#[derive(Deserialize)]
struct VersionOnly {
    format_version: Option<u32>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileV1 {
    #[allow(dead_code)]
    format_version: u32,
    next_id: u32,
    tempo_bpm: f64,
    time_sig_num: u8,
    metronome: Metronome,
    #[serde(default)]
    channels: Vec<Channel>,
    #[serde(default)]
    patterns: Vec<Pattern>,
    tracks: Vec<Track>,
}

/// Parses project text. Returns the project and its id counter, which is
/// at least the largest id in the file plus one (17.1).
pub fn parse(text: &str) -> Result<(Project, u32), FormatError> {
    let v: VersionOnly = toml::from_str(text).map_err(|e| FormatError::Syntax(e.to_string()))?;
    match v.format_version {
        None => Err(FormatError::MissingVersion),
        Some(found) if found > FORMAT_VERSION => Err(FormatError::TooNew {
            found,
            supported: FORMAT_VERSION,
        }),
        // Migrations from older versions go here (7.3), operating on
        // `toml::Table` before the typed parse.
        Some(_) => parse_v1(text),
    }
}

fn parse_v1(text: &str) -> Result<(Project, u32), FormatError> {
    let f: FileV1 = toml::from_str(text).map_err(|e| FormatError::Syntax(e.to_string()))?;
    let p = Project {
        tempo_bpm: f.tempo_bpm,
        time_sig_num: f.time_sig_num,
        metronome: f.metronome,
        channels: f.channels.into_iter().map(Arc::new).collect(),
        patterns: f.patterns.into_iter().map(Arc::new).collect(),
        tracks: f.tracks.into_iter().map(Arc::new).collect(),
    };
    validate(&p).map_err(FormatError::Invalid)?;
    let next_id = f.next_id.max(p.max_id() + 1);
    Ok((p, next_id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consts::*;
    use crate::ids::*;
    use crate::model::*;

    /// xorshift64*, so tests need no random-number crate and are repeatable.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }
        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }
        fn float(&mut self, lo: f64, hi: f64) -> f64 {
            let unit = (self.next() >> 11) as f64 / (1u64 << 53) as f64;
            lo + unit * (hi - lo)
        }
        fn pick<T: Copy>(&mut self, xs: &[T]) -> T {
            xs[self.below(xs.len() as u64) as usize]
        }
    }

    const NAMES: &[&str] = &[
        "Kick",
        "808 \"sub\"",
        "Hats \\ open",
        "Pad ✨",
        "Lead",
        "Bass",
    ];

    fn synth(r: &mut Rng) -> SynthParams {
        let mut s = SynthParams::default();
        for sp in SynthParam::ALL {
            let (lo, hi) = sp.range();
            // Mix exact values with arbitrary ones to test float formatting.
            let v = match r.below(4) {
                0 => lo,
                1 => hi,
                2 => 0.0f64.clamp(lo, hi),
                _ => r.float(lo, hi),
            };
            s.set(sp, v);
        }
        s.osc1.wave = r.pick(&[Wave::Sine, Wave::Saw, Wave::Square, Wave::Triangle]);
        s.osc2.wave = r.pick(&[Wave::Sine, Wave::Saw, Wave::Square, Wave::Triangle]);
        s
    }

    fn clap(r: &mut Rng, next: &mut u32) -> ClapRef {
        let instance = InstanceId(*next);
        *next += 1;
        let mut params: Vec<ParamValue> = (0..r.below(4))
            .map(|i| ParamValue {
                id: (i * 7 + r.below(5)) as u32,
                value: r.float(-1e6, 1e6),
            })
            .collect();
        params.sort_by_key(|p| p.id);
        params.dedup_by_key(|p| p.id);
        ClapRef {
            instance,
            plugin_id: "org.example.plugin".into(),
            plugin_version: "1.2.3".into(),
            state_file: if r.below(2) == 0 {
                Some(format!("{}-1.bin", instance.0))
            } else {
                None
            },
            state_bytes: None,
            params,
        }
    }

    fn mix(r: &mut Rng) -> Mix {
        Mix {
            volume_db: r.float(MIN_GAIN_DB, MAX_GAIN_DB),
            pan: r.float(-1.0, 1.0),
            mute: r.below(2) == 0,
            solo: r.below(2) == 0,
        }
    }

    fn random_project(seed: u64) -> (Project, u32) {
        let r = &mut Rng(seed | 1);
        let mut next = FIRST_ID;
        let mut p = Project::empty();
        let any_tempo = r.float(20.0, 999.0);
        p.tempo_bpm = r.pick(&[120.0, 133.33, MIN_TEMPO_BPM, MAX_TEMPO_BPM, any_tempo]);
        p.time_sig_num = 1 + r.below(16) as u8;
        p.metronome = Metronome {
            enabled: r.below(2) == 0,
            gain_db: r.float(-96.0, 12.0),
        };
        for _ in 0..r.below(4) {
            let id = TrackId(next);
            next += 1;
            let inserts = (0..r.below(3))
                .map(|_| Insert::Clap(clap(r, &mut next)))
                .collect();
            p.tracks.push(Arc::new(Track {
                id,
                name: r.pick(NAMES).into(),
                mix: mix(r),
                inserts,
            }));
        }
        let track_ids: Vec<TrackId> = p.tracks.iter().map(|t| t.id).collect();
        for _ in 0..r.below(6) {
            let id = ChannelId(next);
            next += 1;
            let instrument = if r.below(3) == 0 {
                Instrument::Clap(clap(r, &mut next))
            } else {
                Instrument::Synth(synth(r))
            };
            p.channels.push(Arc::new(Channel {
                id,
                name: r.pick(NAMES).into(),
                root_key: r.below(128) as u8,
                track: r.pick(&track_ids),
                mix: mix(r),
                instrument,
            }));
        }
        let channel_ids: Vec<ChannelId> = p.channels.iter().map(|c| c.id).collect();
        for _ in 0..r.below(4) {
            let mut pat = Pattern::new(PatternId(next), r.pick(NAMES).into());
            next += 1;
            pat.length_steps = 1 + r.below(64) as u8;
            let any_step = 1 + r.below(3840) as u32;
            pat.step_ticks = r.pick(&[60, 120, 240, 480, any_step]);
            for &ch in &channel_ids {
                if r.below(2) == 0 {
                    continue;
                }
                let mut notes = Vec::new();
                for _ in 0..r.below(20) {
                    notes.push(Note {
                        id: NoteId(next),
                        start: r.below(1 << 20) as u32,
                        len: 1 + r.below(4000) as u32,
                        key: r.below(128) as u8,
                        vel: 1 + r.below(127) as u8,
                    });
                    next += 1;
                }
                notes.sort_by_key(|n| (n.start, n.key, n.id));
                pat.notes.push(ChannelNotes { channel: ch, notes });
            }
            p.patterns.push(Arc::new(pat));
        }
        crate::validate::sort_canonical(&mut p);
        (p, next)
    }

    #[test]
    fn round_trip_random_projects() {
        for seed in 1..=500u64 {
            let (p, next) = random_project(seed);
            let text = emit(&p, next).unwrap_or_else(|e| panic!("seed {seed}: {e}"));
            let (back, back_next) =
                parse(&text).unwrap_or_else(|e| panic!("seed {seed}: {e}\n{text}"));
            assert_eq!(back, p, "seed {seed}");
            assert_eq!(back_next, next, "seed {seed}");
            assert_eq!(
                emit(&back, back_next).unwrap(),
                text,
                "seed {seed}: not canonical"
            );
        }
    }

    #[test]
    fn empty_project_text_is_stable() {
        let text = emit(&Project::empty(), FIRST_ID).unwrap();
        assert_eq!(
            text,
            "format_version = 1\nnext_id = 1\ntempo_bpm = 120.0\ntime_sig_num = 4\n\n\
             [metronome]\nenabled = false\ngain_db = -6.0\n\n\
             [[tracks]]\nid = 0\nname = \"Master\"\n\
             mix = { volume_db = 0.0, pan = 0.0, mute = false, solo = false }\n"
        );
    }

    #[test]
    fn one_note_per_line() {
        let (mut p, _) = random_project(7);
        let ch = ChannelId(100_000);
        p.channels.push(Arc::new(Channel {
            id: ch,
            name: "Kick".into(),
            root_key: 36,
            track: TrackId::MASTER,
            mix: Mix::default(),
            instrument: Instrument::Synth(SynthParams::default()),
        }));
        let mut pat = Pattern::new(PatternId(100_001), "P".into());
        pat.notes.push(ChannelNotes {
            channel: ch,
            notes: vec![
                Note {
                    id: NoteId(100_002),
                    start: 0,
                    len: 240,
                    key: 36,
                    vel: 100,
                },
                Note {
                    id: NoteId(100_003),
                    start: 960,
                    len: 240,
                    key: 36,
                    vel: 100,
                },
            ],
        });
        p.patterns.push(Arc::new(pat.clone()));
        crate::validate::sort_canonical(&mut p);
        let before = emit(&p, 200_000).unwrap();

        // Move one note: exactly one line changes.
        let i = p
            .patterns
            .iter()
            .position(|x| x.id == PatternId(100_001))
            .unwrap();
        Arc::make_mut(&mut p.patterns[i]).notes[0].notes[1].start = 1200;
        let after = emit(&p, 200_000).unwrap();
        let changed = before
            .lines()
            .zip(after.lines())
            .filter(|(a, b)| a != b)
            .count();
        assert_eq!(before.lines().count(), after.lines().count());
        assert_eq!(changed, 1);
    }

    #[test]
    fn negative_zero_and_floats_are_canonical() {
        assert_eq!(float(-0.0), "0.0");
        assert_eq!(float(1.0), "1.0");
        assert_eq!(float(133.33), "133.33");
        assert_eq!(float(1e-7), "1e-7");
        for v in [1e-7, 1e21, -96.0, 0.1, 1.0 / 3.0] {
            let t = format!("v = {}", float(v));
            let back: toml::Table = toml::from_str(&t).unwrap();
            assert_eq!(back["v"].as_float(), Some(v), "{t}");
        }
    }

    #[test]
    fn rejects_newer_version_missing_version_and_unknown_keys() {
        let text = emit(&Project::empty(), 1).unwrap();
        let newer = text.replace("format_version = 1", "format_version = 2");
        assert_eq!(
            parse(&newer).unwrap_err(),
            FormatError::TooNew {
                found: 2,
                supported: 1
            }
        );
        let missing = text.replace("format_version = 1\n", "");
        assert_eq!(parse(&missing).unwrap_err(), FormatError::MissingVersion);
        let unknown = text.replace("time_sig_num = 4", "time_sig_num = 4\nswing = 3");
        assert!(matches!(
            parse(&unknown).unwrap_err(),
            FormatError::Syntax(_)
        ));
        let unknown_inner = text.replace("enabled = false", "enabled = false\nvolume = 1");
        assert!(matches!(
            parse(&unknown_inner).unwrap_err(),
            FormatError::Syntax(_)
        ));
    }

    #[test]
    fn rejects_invalid_values_and_unknown_instrument_fields() {
        let (p, next) = random_project(42);
        let text = emit(&p, next).unwrap();
        let bad_tempo = text.replacen(
            &format!("tempo_bpm = {}", float(p.tempo_bpm)),
            "tempo_bpm = 5000.0",
            1,
        );
        assert!(matches!(
            parse(&bad_tempo).unwrap_err(),
            FormatError::Invalid(_)
        ));

        let mut q = Project::empty();
        q.channels.push(Arc::new(Channel {
            id: ChannelId(1),
            name: "S".into(),
            root_key: 60,
            track: TrackId::MASTER,
            mix: Mix::default(),
            instrument: Instrument::Synth(SynthParams::default()),
        }));
        let t = emit(&q, 2)
            .unwrap()
            .replace("kind = \"synth\"", "kind = \"synth\"\nbogus = 1");
        assert!(matches!(parse(&t).unwrap_err(), FormatError::Syntax(_)));
    }

    #[test]
    fn next_id_never_below_max_id_plus_one() {
        let (p, next) = random_project(99);
        let text = emit(&p, next)
            .unwrap()
            .replace(&format!("next_id = {next}"), "next_id = 1");
        let (_, n) = parse(&text).unwrap();
        assert_eq!(n, p.max_id() + 1);
    }
}
