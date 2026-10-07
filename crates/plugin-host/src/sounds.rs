// SPDX-License-Identifier: GPL-3.0-or-later
//! The beginner instrument list (SPEC 20.6, Amendment 23): libre CLAP plugins
//! with factory presets, grouped by role. The data is `presets/instruments.toml`.

use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

use crate::host::{PluginDesc, extension_root};

/// A plugin the list draws from.
#[derive(Clone, Debug, PartialEq)]
pub struct SoundPlugin {
    pub key: String,
    pub name: String,
    pub clap_id: String,
    /// Flatpak extension that ships the plugin.
    pub extension: String,
    /// Directory of factory presets, relative to the extension root.
    pub preset_root: String,
}

/// One entry: a preset of a plugin, filed under a role.
#[derive(Clone, Debug, PartialEq)]
pub struct Sound {
    pub role: String,
    /// The name in the data file.
    pub name: String,
    pub plugin: String,
    /// Path under the plugin's preset root, for example "Basses/Sub 1.fxp".
    pub preset: String,
    /// MIDI key that shows the sound off.
    pub note: u8,
    /// Level change for presets that peak above full scale.
    pub gain_db: f64,
}

impl Sound {
    /// The preset's file name without its extension: "Sub 1".
    pub fn label(&self) -> String {
        Path::new(&self.preset)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.name.clone())
    }
}

struct Catalog {
    plugins: Vec<SoundPlugin>,
    sounds: Vec<Sound>,
}

fn catalog() -> &'static Catalog {
    static C: OnceLock<Catalog> = OnceLock::new();
    C.get_or_init(|| {
        parse(include_str!("../presets/instruments.toml")).expect("instruments.toml is valid")
    })
}

fn parse(text: &str) -> Result<Catalog, String> {
    let doc: toml::Table = text.parse().map_err(|e: toml::de::Error| e.to_string())?;
    let str_of = |t: &toml::Value, k: &str| -> Result<String, String> {
        t.get(k)
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .ok_or_else(|| format!("missing `{k}`"))
    };
    let mut plugins = Vec::new();
    for p in doc
        .get("plugin")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
    {
        plugins.push(SoundPlugin {
            key: str_of(p, "key")?,
            name: str_of(p, "name")?,
            clap_id: str_of(p, "clap_id")?,
            extension: str_of(p, "extension")?,
            preset_root: str_of(p, "preset_root")?,
        });
    }
    let mut sounds = Vec::new();
    for e in doc
        .get("instrument")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
    {
        let plugin = str_of(e, "plugin")?;
        if !plugins.iter().any(|p| p.key == plugin) {
            return Err(format!("unknown plugin `{plugin}`"));
        }
        sounds.push(Sound {
            role: str_of(e, "role")?,
            name: str_of(e, "name")?,
            plugin,
            preset: str_of(e, "preset")?,
            note: e
                .get("note")
                .and_then(|v| v.as_integer())
                .and_then(|n| u8::try_from(n).ok())
                .unwrap_or(60),
            gain_db: e
                .get("gain_db")
                .and_then(|v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64)))
                .unwrap_or(0.0),
        });
    }
    Ok(Catalog { plugins, sounds })
}

/// The plugins the list draws from.
pub fn plugins() -> &'static [SoundPlugin] {
    &catalog().plugins
}

/// Every entry, in file order.
pub fn sounds() -> &'static [Sound] {
    &catalog().sounds
}

/// The roles in file order, each once.
pub fn roles() -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::new();
    for s in sounds() {
        if !out.contains(&s.role.as_str()) {
            out.push(&s.role);
        }
    }
    out
}

/// The plugin a sound plays through.
pub fn plugin_of(s: &Sound) -> Option<&'static SoundPlugin> {
    plugins().iter().find(|p| p.key == s.plugin)
}

/// The entry for a plugin id and preset path, if the list has one.
pub fn find(clap_id: &str, preset: &str) -> Option<&'static Sound> {
    sounds()
        .iter()
        .find(|s| s.preset == preset && plugin_of(s).is_some_and(|p| p.clap_id == clap_id))
}

/// The preset file for `preset` under the plugin's factory presets, or `None`
/// if the plugin has none, the path would leave the preset root, or the file
/// is missing. `preset` may come from an agent, so it is checked.
pub fn preset_file(desc: &PluginDesc, preset: &str) -> Option<PathBuf> {
    let p = plugins().iter().find(|p| p.clap_id == desc.id)?;
    let rel = Path::new(preset);
    if !rel.components().all(|c| matches!(c, Component::Normal(_))) {
        return None;
    }
    let file = extension_root(desc)?.join(&p.preset_root).join(rel);
    file.is_file().then_some(file)
}

/// The Flatpak extension to offer when `clap_id` is not installed.
pub fn extension_of(clap_id: &str) -> Option<&'static str> {
    plugins()
        .iter()
        .find(|p| p.clap_id == clap_id)
        .map(|p| p.extension.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_role_has_an_entry_and_every_entry_a_plugin() {
        let want = [
            "Bass", "808", "Lead", "Pad", "Keys", "Pluck", "Bell", "Strings", "Brass", "FX", "Arp",
        ];
        assert_eq!(roles(), want);
        for r in want {
            assert!(sounds().iter().any(|s| s.role == r), "{r} has no entry");
        }
        for s in sounds() {
            assert!(plugin_of(s).is_some(), "{} has no plugin", s.name);
            assert!(!s.label().is_empty());
        }
    }

    #[test]
    fn labels_drop_path_and_extension() {
        let s = find("org.surge-synth-team.surge-xt", "Basses/Sub 1.fxp").unwrap();
        assert_eq!(s.label(), "Sub 1");
    }

    #[test]
    fn classic_lead_is_turned_down() {
        let s = find("org.surge-synth-team.surge-xt", "Leads/Classic Lead 1.fxp").unwrap();
        assert_eq!(s.gain_db, -3.0);
    }

    #[test]
    fn preset_paths_cannot_leave_the_root() {
        let d = PluginDesc {
            id: "org.surge-synth-team.surge-xt".into(),
            name: String::new(),
            vendor: String::new(),
            version: String::new(),
            path: PathBuf::from("/nonexistent/x/clap/a.clap"),
            instrument: true,
            effect: false,
        };
        assert!(preset_file(&d, "../../etc/passwd").is_none());
        assert!(preset_file(&d, "/etc/passwd").is_none());
    }
}
