// SPDX-License-Identifier: GPL-3.0-or-later
//! Finds FL Studio installs on this machine (Bottles, Wine, Lutris,
//! Steam Proton) and accepts any folder the user picks. Read only, and
//! cheap: a few directory listings, no walking of the sample tree.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallSource {
    Bottles,
    Wine,
    Lutris,
    Proton,
    PlayOnLinux,
    /// A folder the user picked.
    Folder,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlInstall {
    /// "FL Studio 2026" (the install folder's name).
    pub name: String,
    /// "2026", empty when the name has no version.
    pub version: String,
    /// `.../Data/Patches/Packs`
    pub packs: PathBuf,
    /// The Wine prefix (`drive_c` parent); the picked folder for `Folder`.
    pub prefix: PathBuf,
    pub source: InstallSource,
}

fn subdirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut v: Vec<PathBuf> = rd
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir() || t.is_symlink()))
        .map(|e| e.path())
        .collect();
    v.sort();
    v
}

/// FL installs inside one Wine prefix.
fn installs_in_prefix(prefix: &Path, source: InstallSource, out: &mut Vec<FlInstall>) {
    let drive_c = prefix.join("drive_c");
    for pf in ["Program Files", "Program Files (x86)"] {
        for fl in subdirs(&drive_c.join(pf).join("Image-Line")) {
            let Some(name) = fl.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if !name.starts_with("FL Studio") {
                continue;
            }
            let packs = fl.join("Data").join("Patches").join("Packs");
            if packs.is_dir() {
                out.push(FlInstall {
                    name: name.to_string(),
                    version: name.trim_start_matches("FL Studio").trim().to_string(),
                    packs,
                    prefix: prefix.to_path_buf(),
                    source,
                });
            }
        }
    }
}

/// Detection with explicit directories, for tests.
pub fn detect_in(home: &Path, data_home: &Path) -> Vec<FlInstall> {
    let mut found = Vec::new();
    let mut prefix = |p: &Path, s: InstallSource| installs_in_prefix(p, s, &mut found);

    // Bottles: Flatpak and native.
    for base in [
        home.join(".var/app/com.usebottles.bottles/data/bottles/bottles"),
        data_home.join("bottles/bottles"),
    ] {
        for p in subdirs(&base) {
            prefix(&p, InstallSource::Bottles);
        }
    }
    // Wine.
    prefix(&home.join(".wine"), InstallSource::Wine);
    if let Some(p) = env::var_os("WINEPREFIX") {
        prefix(Path::new(&p), InstallSource::Wine);
    }
    for p in subdirs(&data_home.join("wineprefixes")) {
        prefix(&p, InstallSource::Wine);
    }
    // Lutris: prefixes under ~/Games/<game>[/prefix|/pfx].
    for g in subdirs(&home.join("Games")) {
        for sub in ["", "prefix", "pfx"] {
            prefix(&g.join(sub), InstallSource::Lutris);
        }
    }
    // PlayOnLinux.
    for p in subdirs(&home.join(".PlayOnLinux/wineprefix")) {
        prefix(&p, InstallSource::PlayOnLinux);
    }
    // Steam Proton: compatdata/<appid>/pfx, native and Flatpak Steam.
    for steam in [
        home.join(".steam/steam"),
        data_home.join("Steam"),
        home.join(".var/app/com.valvesoftware.Steam/data/Steam"),
    ] {
        for p in subdirs(&steam.join("steamapps/compatdata")) {
            prefix(&p.join("pfx"), InstallSource::Proton);
        }
    }

    found.sort_by(|a, b| a.packs.cmp(&b.packs));
    found.dedup_by(|a, b| a.packs == b.packs);
    found
}

/// Installs found in the usual places on this machine. Under 200 ms when
/// there is none (measured by a test).
pub fn detect_installs() -> Vec<FlInstall> {
    let Some(home) = env::var_os("HOME").map(PathBuf::from) else {
        return Vec::new();
    };
    let data_home = env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home.join(".local/share"));
    detect_in(&home, &data_home)
}

/// Any folder the user picks: a `Packs` folder, an FL `Data` folder, an
/// install folder, or just a folder of samples.
pub fn install_from_folder(folder: &Path) -> Option<FlInstall> {
    if !folder.is_dir() {
        return None;
    }
    let packs = ["Data/Patches/Packs", "Patches/Packs", "Packs"]
        .iter()
        .map(|s| folder.join(s))
        .find(|c| c.is_dir())
        .unwrap_or_else(|| folder.to_path_buf());
    let name = folder
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("Sample folder")
        .to_string();
    Some(FlInstall {
        version: name.trim_start_matches("FL Studio").trim().to_string(),
        name,
        packs,
        prefix: folder.to_path_buf(),
        source: InstallSource::Folder,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;
    use std::time::Instant;

    #[test]
    fn finds_bottles_install_with_version() {
        let t = TempDir::new("detect");
        let packs = t.path().join(
            ".var/app/com.usebottles.bottles/data/bottles/bottles/FL/drive_c/Program Files/Image-Line/FL Studio 2026/Data/Patches/Packs",
        );
        fs::create_dir_all(&packs).unwrap();
        let v = detect_in(t.path(), &t.path().join(".local/share"));
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].name, "FL Studio 2026");
        assert_eq!(v[0].version, "2026");
        assert_eq!(v[0].source, InstallSource::Bottles);
        assert_eq!(v[0].packs, packs);
    }

    #[test]
    fn finds_proton_and_wine() {
        let t = TempDir::new("detect2");
        for p in [
            ".wine/drive_c/Program Files (x86)/Image-Line/FL Studio 21/Data/Patches/Packs",
            ".local/share/Steam/steamapps/compatdata/123/pfx/drive_c/Program Files/Image-Line/FL Studio 20/Data/Patches/Packs",
        ] {
            fs::create_dir_all(t.path().join(p)).unwrap();
        }
        let v = detect_in(t.path(), &t.path().join(".local/share"));
        assert_eq!(v.len(), 2);
        assert!(v.iter().any(|i| i.source == InstallSource::Wine));
        assert!(v.iter().any(|i| i.source == InstallSource::Proton));
    }

    #[test]
    fn nothing_found_is_fast() {
        let t = TempDir::new("detect3");
        fs::create_dir_all(t.path().join("Games/foo")).unwrap();
        let start = Instant::now();
        let v = detect_in(t.path(), &t.path().join(".local/share"));
        let ms = start.elapsed().as_millis();
        assert!(v.is_empty());
        assert!(ms < 200, "detection took {ms} ms");
    }

    #[test]
    fn picked_folder() {
        let t = TempDir::new("detect4");
        fs::create_dir_all(t.path().join("Data/Patches/Packs")).unwrap();
        let i = install_from_folder(t.path()).unwrap();
        assert!(i.packs.ends_with("Data/Patches/Packs"));
        assert_eq!(install_from_folder(&t.path().join("nope")), None);
    }
}
