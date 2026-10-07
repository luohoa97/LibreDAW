// SPDX-License-Identifier: GPL-3.0-or-later
//! User preferences (docs/ui-design.md 3.11): a small file next to
//! `last.toml`. Not part of any project. No GTK here.

use std::fs;

use doc::persist::{Dirs, key_values, quote, unquote};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ColorScheme {
    #[default]
    System,
    Light,
    Dark,
}

impl ColorScheme {
    pub const ALL: [ColorScheme; 3] = [ColorScheme::System, ColorScheme::Light, ColorScheme::Dark];

    pub fn label(self) -> &'static str {
        match self {
            ColorScheme::System => "Follow System",
            ColorScheme::Light => "Light",
            ColorScheme::Dark => "Dark",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            ColorScheme::System => "system",
            ColorScheme::Light => "light",
            ColorScheme::Dark => "dark",
        }
    }

    pub fn from_name(s: &str) -> Option<ColorScheme> {
        ColorScheme::ALL.into_iter().find(|c| c.name() == s)
    }
}

/// Buffer sizes offered in the Audio page, in frames.
pub const BUFFER_SIZES: [u32; 5] = [64, 128, 256, 512, 1024];

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    /// Play a sound when a step, note, or key is clicked.
    pub preview_notes: bool,
    pub color_scheme: ColorScheme,
    /// Output device name; `None` is the system default. Used at start-up.
    pub output_device: Option<String>,
    /// Buffer size in frames; 0 is the engine's choice. Used at start-up.
    pub buffer_frames: u32,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            preview_notes: true,
            color_scheme: ColorScheme::System,
            output_device: None,
            buffer_frames: 0,
        }
    }
}

impl Settings {
    pub fn emit(&self) -> String {
        let mut o = String::from("# Oto preferences.\n");
        o.push_str(&format!("preview_notes = {}\n", self.preview_notes));
        o.push_str(&format!(
            "color_scheme = {}\n",
            quote(self.color_scheme.name())
        ));
        if let Some(d) = &self.output_device {
            o.push_str(&format!("output_device = {}\n", quote(d)));
        }
        o.push_str(&format!("buffer_frames = {}\n", self.buffer_frames));
        o
    }

    pub fn parse(text: &str) -> Settings {
        let mut s = Settings::default();
        for (k, v) in key_values(text) {
            match k {
                "preview_notes" => {
                    if let Ok(b) = v.parse() {
                        s.preview_notes = b;
                    }
                }
                "color_scheme" => {
                    if let Some(c) = unquote(v).and_then(|n| ColorScheme::from_name(&n)) {
                        s.color_scheme = c;
                    }
                }
                "output_device" => s.output_device = unquote(v),
                "buffer_frames" => {
                    if let Ok(n) = v.parse::<u32>()
                        && (n == 0 || BUFFER_SIZES.contains(&n))
                    {
                        s.buffer_frames = n;
                    }
                }
                _ => {}
            }
        }
        s
    }

    pub fn read(dirs: &Dirs) -> Settings {
        fs::read_to_string(dirs.settings_file())
            .map(|t| Settings::parse(&t))
            .unwrap_or_default()
    }

    pub fn write(&self, dirs: &Dirs) -> std::io::Result<()> {
        let f = dirs.settings_file();
        if let Some(d) = f.parent() {
            fs::create_dir_all(d)?;
        }
        let tmp = f.with_extension("toml.tmp");
        fs::write(&tmp, self.emit())?;
        fs::rename(&tmp, &f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_preview_on() {
        let s = Settings::default();
        assert!(s.preview_notes);
        assert_eq!(s.color_scheme, ColorScheme::System);
        assert_eq!(Settings::parse(""), s);
    }

    #[test]
    fn round_trip() {
        let s = Settings {
            preview_notes: false,
            color_scheme: ColorScheme::Dark,
            output_device: Some("Speakers \"A\"".into()),
            buffer_frames: 256,
        };
        assert_eq!(Settings::parse(&s.emit()), s);
    }

    #[test]
    fn bad_values_fall_back() {
        let s = Settings::parse(
            "preview_notes = maybe\ncolor_scheme = \"purple\"\nbuffer_frames = 99\nzzz = 1\n",
        );
        assert_eq!(s, Settings::default());
    }

    #[test]
    fn file_round_trip() {
        let dir = std::env::temp_dir().join(format!("ldaw-settings-{}", std::process::id()));
        let dirs = Dirs {
            music: dir.join("m"),
            data: dir.join("d"),
            config: dir.join("c"),
        };
        assert_eq!(Settings::read(&dirs), Settings::default());
        let s = Settings {
            preview_notes: false,
            ..Settings::default()
        };
        s.write(&dirs).unwrap();
        assert_eq!(Settings::read(&dirs), s);
        let _ = fs::remove_dir_all(dir);
    }
}
