// SPDX-License-Identifier: GPL-3.0-or-later
//! Built-in synth sounds offered when adding a channel (SPEC 8, 13.1). Each
//! is plain `SynthParams` plus the key a step plays, so "Add channel" starts
//! from a sound instead of a blank row. Sample kits come with Milestone B.

use protocol::model::{Adsr, Osc, SynthParams, Wave};

pub struct Preset {
    pub name: &'static str,
    /// What kind of part it suits, for the picker's subtitle.
    pub role: &'static str,
    /// The key a step plays (5.2).
    pub root_key: u8,
    pub params: SynthParams,
}

fn osc(wave: Wave, semitones: f64) -> Osc {
    Osc {
        wave,
        semitones,
        cents: 0.0,
    }
}

fn env(a: f64, d: f64, s: f64, r: f64) -> Adsr {
    Adsr {
        attack_ms: a,
        decay_ms: d,
        sustain: s,
        release_ms: r,
    }
}

/// The built-in presets, in the order the picker lists them.
pub fn presets() -> Vec<Preset> {
    vec![
        Preset {
            name: "Kick",
            role: "Drum",
            root_key: 36,
            params: SynthParams {
                osc1: osc(Wave::Sine, -12.0),
                osc2: osc(Wave::Triangle, -24.0),
                osc_mix: 0.35,
                cutoff_hz: 900.0,
                resonance: 0.1,
                filter_env_octaves: 3.0,
                amp_env: env(1.0, 220.0, 0.0, 60.0),
                filter_env: env(1.0, 90.0, 0.0, 40.0),
                gain_db: -3.0,
            },
        },
        Preset {
            name: "Snare",
            role: "Drum",
            root_key: 38,
            params: SynthParams {
                osc1: osc(Wave::Triangle, 7.0),
                osc2: osc(Wave::Square, 19.0),
                osc_mix: 0.5,
                cutoff_hz: 5200.0,
                resonance: 0.15,
                filter_env_octaves: 2.0,
                amp_env: env(1.0, 150.0, 0.0, 70.0),
                filter_env: env(1.0, 120.0, 0.1, 60.0),
                gain_db: -6.0,
            },
        },
        Preset {
            name: "Closed hat",
            role: "Drum",
            root_key: 42,
            params: SynthParams {
                osc1: osc(Wave::Square, 36.0),
                osc2: osc(Wave::Square, 31.0),
                osc_mix: 0.5,
                cutoff_hz: 11000.0,
                resonance: 0.2,
                filter_env_octaves: 0.0,
                amp_env: env(0.5, 45.0, 0.0, 25.0),
                filter_env: env(1.0, 40.0, 0.0, 20.0),
                gain_db: -12.0,
            },
        },
        Preset {
            name: "Open hat",
            role: "Drum",
            root_key: 46,
            params: SynthParams {
                osc1: osc(Wave::Square, 36.0),
                osc2: osc(Wave::Square, 31.0),
                osc_mix: 0.5,
                cutoff_hz: 10000.0,
                resonance: 0.2,
                filter_env_octaves: 0.0,
                amp_env: env(0.5, 260.0, 0.0, 120.0),
                filter_env: env(1.0, 200.0, 0.0, 100.0),
                gain_db: -12.0,
            },
        },
        Preset {
            name: "Sub bass",
            role: "Bass",
            root_key: 36,
            params: SynthParams {
                osc1: osc(Wave::Sine, -12.0),
                osc2: osc(Wave::Saw, -24.0),
                osc_mix: 0.25,
                cutoff_hz: 700.0,
                resonance: 0.25,
                filter_env_octaves: 1.5,
                amp_env: env(3.0, 250.0, 0.8, 120.0),
                filter_env: env(3.0, 200.0, 0.3, 120.0),
                gain_db: -6.0,
            },
        },
        Preset {
            name: "Pluck",
            role: "Melody",
            root_key: 60,
            params: SynthParams {
                osc1: osc(Wave::Saw, 0.0),
                osc2: osc(Wave::Square, 0.0),
                osc_mix: 0.4,
                cutoff_hz: 2400.0,
                resonance: 0.3,
                filter_env_octaves: 3.0,
                amp_env: env(1.0, 280.0, 0.0, 160.0),
                filter_env: env(1.0, 220.0, 0.05, 120.0),
                gain_db: -8.0,
            },
        },
        Preset {
            name: "Lead",
            role: "Melody",
            root_key: 72,
            params: SynthParams {
                osc1: osc(Wave::Saw, 0.0),
                osc2: osc(Wave::Saw, 0.12),
                osc_mix: 0.5,
                cutoff_hz: 4200.0,
                resonance: 0.2,
                filter_env_octaves: 1.0,
                amp_env: env(4.0, 200.0, 0.75, 220.0),
                filter_env: env(4.0, 300.0, 0.5, 200.0),
                gain_db: -9.0,
            },
        },
        Preset {
            name: "Pad",
            role: "Harmony",
            root_key: 60,
            params: SynthParams {
                osc1: osc(Wave::Triangle, 0.0),
                osc2: osc(Wave::Saw, -12.0),
                osc_mix: 0.45,
                cutoff_hz: 1800.0,
                resonance: 0.15,
                filter_env_octaves: 1.5,
                amp_env: env(420.0, 600.0, 0.8, 900.0),
                filter_env: env(500.0, 800.0, 0.5, 800.0),
                gain_db: -10.0,
            },
        },
        Preset {
            name: "Init synth",
            role: "Blank",
            root_key: 60,
            params: SynthParams::default(),
        },
    ]
}

/// A name for a new channel that is not already taken: "Kick", "Kick 2", ...
pub fn unique_name(base: &str, taken: &[&str]) -> String {
    if !taken.contains(&base) {
        return base.to_string();
    }
    let mut n = 2;
    loop {
        let c = format!("{base} {n}");
        if !taken.contains(&c.as_str()) {
            return c;
        }
        n += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::validate::check_synth;

    #[test]
    fn every_preset_is_a_valid_document_value() {
        for p in presets() {
            check_synth(&p.params).unwrap_or_else(|e| panic!("{}: {e}", p.name));
            assert!(p.root_key <= 127, "{}", p.name);
            assert!(!p.name.is_empty() && p.name.chars().count() <= 128);
        }
    }

    #[test]
    fn names_are_unique_and_cover_a_beat() {
        let ps = presets();
        let mut names: Vec<_> = ps.iter().map(|p| p.name).collect();
        let n = names.len();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), n);
        for need in ["Kick", "Snare", "Closed hat", "Sub bass"] {
            assert!(names.contains(&need), "{need}");
        }
    }

    #[test]
    fn unique_names_count_up() {
        assert_eq!(unique_name("Kick", &[]), "Kick");
        assert_eq!(unique_name("Kick", &["Kick"]), "Kick 2");
        assert_eq!(unique_name("Kick", &["Kick", "Kick 2", "Kick 3"]), "Kick 4");
        assert_eq!(unique_name("Kick", &["Kick 2"]), "Kick");
    }
}
