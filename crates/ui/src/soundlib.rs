// SPDX-License-Identifier: GPL-3.0-or-later
//! Sound packs and user sample folders for the sound browser (SPEC 15.3),
//! without GTK.
//!
//! A pack root holds `index.toml` (every file with its size and SHA-256)
//! and one directory per kit, each with a `kit.toml` that lists its pieces
//! (file, role, root key, choke group, gain, pan). Roots are searched in
//! `$XDG_DATA_DIRS/libredaw/sounds/`, `~/.local/share/libredaw/sounds/`
//! (`$XDG_DATA_HOME`), and for development the directory named by
//! `LIBREDAW_SOUNDS_DIR`. A user folder of WAV files becomes one kit whose
//! pieces are `local_only` (17.2): they are never copied into a project.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use doc::sha256::sha256_hex;

/// Where a kit came from, which decides how its sounds are imported.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// An installed pack: samples are copied into the project.
    Pack,
    /// A folder the user added: samples stay where they are (`local_only`).
    UserFolder,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Piece {
    pub file: String,
    /// "Kick Distorted".
    pub name: String,
    /// The role tag of the kit manifest: "kick", "hat_closed", "808".
    pub role: String,
    /// Key of the sample's pitch; `None` for unpitched pieces.
    pub root_key: Option<u8>,
    pub choke_group: u8,
    pub gain_db: f64,
    pub pan: f64,
    pub length_ms: u32,
    pub path: PathBuf,
    /// SHA-256 from the pack index, when the index lists the file.
    pub sha256: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Kit {
    /// Directory name: "phonk", "boom-bap".
    pub id: String,
    /// "Phonk", "Boom Bap".
    pub title: String,
    pub description: String,
    pub license: String,
    pub dir: PathBuf,
    pub source: Source,
    pub pieces: Vec<Piece>,
}

/// The directories to search for pack roots, given the environment (passed
/// in so tests do not touch the process environment).
pub fn search_roots(
    data_dirs: Option<&str>,
    data_home: Option<&Path>,
    home: Option<&Path>,
    dev_dir: Option<&Path>,
) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    let mut push = |p: PathBuf| {
        if !out.contains(&p) {
            out.push(p);
        }
    };
    if let Some(d) = dev_dir {
        push(d.to_path_buf());
    }
    let dirs = match data_dirs {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => "/usr/local/share:/usr/share".to_string(),
    };
    if let Some(h) = data_home {
        push(h.join("libredaw").join("sounds"));
    } else if let Some(h) = home {
        push(h.join(".local/share/libredaw/sounds"));
    }
    for d in dirs.split(':').filter(|d| d.starts_with('/')) {
        push(Path::new(d).join("libredaw").join("sounds"));
    }
    out
}

/// The search roots of the running process.
pub fn default_roots() -> Vec<PathBuf> {
    let env_path = |k: &str| {
        std::env::var_os(k)
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
    };
    search_roots(
        std::env::var("XDG_DATA_DIRS").ok().as_deref(),
        env_path("XDG_DATA_HOME").as_deref(),
        env_path("HOME").as_deref(),
        env_path("LIBREDAW_SOUNDS_DIR").as_deref(),
    )
}

/// `"C#5"` to a MIDI key with C4 = 60 (C2 = 36).
pub fn note_key(name: &str) -> Option<u8> {
    let mut chars = name.trim().chars();
    let base: i32 = match chars.next()?.to_ascii_uppercase() {
        'C' => 0,
        'D' => 2,
        'E' => 4,
        'F' => 5,
        'G' => 7,
        'A' => 9,
        'B' => 11,
        _ => return None,
    };
    let rest: String = chars.collect();
    let (acc, oct) = match rest.chars().next()? {
        '#' => (1, &rest[1..]),
        'b' => (-1, &rest[1..]),
        _ => (0, &rest[..]),
    };
    let octave: i32 = oct.parse().ok()?;
    let key = (octave + 1) * 12 + base + acc;
    (0..=127).contains(&key).then_some(key as u8)
}

/// "hat_closed" to "Closed Hat", "808" to "808".
pub fn role_title(role: &str) -> String {
    match role {
        "hat_closed" => "Closed Hat".into(),
        "hat_open" => "Open Hat".into(),
        "808" => "808".into(),
        other => title_case(other),
    }
}

/// "Drum" for every drum role, "808" for 808s: the filter groups of the
/// browser.
pub fn role_group(role: &str) -> &'static str {
    if role == "808" { "808" } else { "Drum" }
}

/// "kick_distorted" to "Kick Distorted".
pub fn title_case(s: &str) -> String {
    s.split(['_', '-', ' '])
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

// ---------------------------------------------------------------------------
// A small reader for the manifest subset: `[[table]]` headers and
// `key = "string"` or `key = number` lines.

#[derive(Default)]
struct Table {
    name: String,
    values: HashMap<String, String>,
}

fn unquote(v: &str) -> String {
    let v = v.trim();
    match v.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
        Some(s) => s.replace("\\\"", "\"").replace("\\\\", "\\"),
        None => v.to_string(),
    }
}

/// The top-level values (table name "") and every `[[..]]` table in order.
fn read_tables(text: &str) -> Vec<Table> {
    let mut tables = vec![Table::default()];
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(h) = line.strip_prefix("[[").and_then(|l| l.strip_suffix("]]")) {
            tables.push(Table {
                name: h.trim().to_string(),
                values: HashMap::new(),
            });
        } else if line.starts_with('[') {
            tables.push(Table {
                name: line.trim_matches(['[', ']']).to_string(),
                values: HashMap::new(),
            });
        } else if let Some((k, v)) = line.split_once('=')
            && let Some(t) = tables.last_mut()
        {
            t.values.insert(k.trim().to_string(), unquote(v));
        }
    }
    tables
}

/// `file name -> sha256` of the pack named `pack` in `index.toml`.
pub fn parse_index(text: &str, pack: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let mut current = false;
    for t in read_tables(text) {
        match t.name.as_str() {
            "pack" => current = t.values.get("name").map(String::as_str) == Some(pack),
            "pack.file" if current => {
                if let (Some(p), Some(h)) = (t.values.get("path"), t.values.get("sha256")) {
                    out.insert(p.clone(), h.to_lowercase());
                }
            }
            _ => {}
        }
    }
    out
}

/// Reads one kit manifest. Pieces whose file name could escape the kit
/// directory are dropped. `index` is the file-to-hash map of the pack.
pub fn parse_kit(
    text: &str,
    dir: &Path,
    source: Source,
    index: &HashMap<String, String>,
) -> Option<Kit> {
    let tables = read_tables(text);
    let top = &tables[0].values;
    let id = dir.file_name()?.to_string_lossy().to_string();
    let name = top.get("name").cloned().unwrap_or_else(|| id.clone());
    let mut pieces = Vec::new();
    for t in tables.iter().filter(|t| t.name == "piece") {
        let Some(file) = t.values.get("file") else {
            continue;
        };
        if file.contains('/') || file.contains('\\') || file.starts_with('.') {
            continue;
        }
        let role = t.values.get("role").cloned().unwrap_or_default();
        let num = |k: &str| t.values.get(k).and_then(|v| v.parse::<f64>().ok());
        let stem = file.strip_suffix(".wav").unwrap_or(file);
        pieces.push(Piece {
            file: file.clone(),
            name: title_case(stem),
            role,
            root_key: t.values.get("root_key").and_then(|k| note_key(k)),
            choke_group: num("choke_group")
                .map(|g| g.clamp(0.0, 16.0) as u8)
                .unwrap_or(0),
            gain_db: num("gain_db").unwrap_or(0.0).clamp(-24.0, 12.0),
            pan: num("pan").unwrap_or(0.0).clamp(-1.0, 1.0),
            length_ms: num("length_ms").unwrap_or(0.0).max(0.0) as u32,
            path: dir.join(file),
            sha256: index.get(file).cloned(),
        });
    }
    if pieces.is_empty() {
        return None;
    }
    Some(Kit {
        id,
        title: title_case(&name),
        description: top.get("description").cloned().unwrap_or_default(),
        license: top.get("license").cloned().unwrap_or_default(),
        dir: dir.to_path_buf(),
        source,
        pieces,
    })
}

/// Every kit under the pack roots, sorted by title. A root without
/// `index.toml` still works; its pieces have no hash to verify.
pub fn discover(roots: &[PathBuf]) -> Vec<Kit> {
    let mut kits: Vec<Kit> = Vec::new();
    for root in roots {
        let index_text = std::fs::read_to_string(root.join("index.toml")).unwrap_or_default();
        let Ok(rd) = std::fs::read_dir(root) else {
            continue;
        };
        let mut dirs: Vec<PathBuf> = rd
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.join("kit.toml").is_file())
            .collect();
        dirs.sort();
        for d in dirs {
            let Ok(text) = std::fs::read_to_string(d.join("kit.toml")) else {
                continue;
            };
            let id = d
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            let index = parse_index(&index_text, &id);
            if let Some(k) = parse_kit(&text, &d, Source::Pack, &index)
                && !kits.iter().any(|x| x.id == k.id)
            {
                kits.push(k);
            }
        }
    }
    kits.sort_by(|a, b| a.title.cmp(&b.title));
    kits
}

/// Guesses the role of a loose sample from its file name.
pub fn guess_role(file_stem: &str) -> String {
    let s = file_stem.to_lowercase();
    for (needle, role) in [
        ("808", "808"),
        ("kick", "kick"),
        ("bd", "kick"),
        ("snare", "snare"),
        ("clap", "clap"),
        ("rim", "rim"),
        ("hat", "hat_closed"),
        ("hh", "hat_closed"),
        ("cowbell", "cowbell"),
        ("tom", "tom"),
        ("crash", "crash"),
        ("ride", "ride"),
        ("shaker", "shaker"),
        ("snap", "snap"),
    ] {
        if s.contains(needle) {
            return role.to_string();
        }
    }
    "perc".to_string()
}

/// A user folder of WAV files as one kit (search depth: the folder and its
/// direct subfolders). `None` if there is no WAV file.
pub fn scan_folder(dir: &Path) -> Option<Kit> {
    let mut files: Vec<PathBuf> = Vec::new();
    let mut stack = vec![(dir.to_path_buf(), 0)];
    while let Some((d, depth)) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.filter_map(Result::ok) {
            let p = e.path();
            if p.is_dir() && depth < 1 {
                stack.push((p, depth + 1));
            } else if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("wav")) && p.is_file() {
                files.push(p);
            }
        }
    }
    files.sort();
    files.truncate(2000);
    if files.is_empty() {
        return None;
    }
    let id = dir
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "folder".into());
    let pieces = files
        .iter()
        .filter_map(|p| {
            let stem = p.file_stem()?.to_string_lossy().to_string();
            let file = p.file_name()?.to_string_lossy().to_string();
            Some(Piece {
                role: guess_role(&stem),
                name: title_case(&stem),
                file,
                root_key: None,
                choke_group: 0,
                gain_db: 0.0,
                pan: 0.0,
                length_ms: 0,
                path: p.clone(),
                sha256: None,
            })
        })
        .collect();
    Some(Kit {
        title: title_case(&id),
        id,
        description: "Your folder".into(),
        license: String::new(),
        dir: dir.to_path_buf(),
        source: Source::UserFolder,
        pieces,
    })
}

/// Checks a pack file against the hash of the pack index. A piece without
/// a listed hash passes; one that does not match is refused (the file was
/// damaged or replaced).
pub fn verify(piece: &Piece) -> Result<(), String> {
    let Some(want) = &piece.sha256 else {
        return Ok(());
    };
    let bytes = std::fs::read(&piece.path)
        .map_err(|e| format!("Cannot read {}: {e}", piece.path.display()))?;
    let got = sha256_hex(&bytes);
    if got.eq_ignore_ascii_case(want) {
        Ok(())
    } else {
        Err(format!(
            "{} does not match the pack index, so it was not added",
            piece.file
        ))
    }
}

// ---------------------------------------------------------------------------
// The folders the user added.

/// One folder path per `path = "..."` line.
pub fn parse_folders(text: &str) -> Vec<PathBuf> {
    read_tables(text)
        .iter()
        .filter_map(|t| t.values.get("path"))
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .collect()
}

pub fn emit_folders(folders: &[PathBuf]) -> String {
    let mut s = String::from("# Oto: sound folders added in the sound browser.\n");
    for f in folders {
        s.push_str("[[folder]]\npath = ");
        s.push_str(&doc::persist::quote(&f.to_string_lossy()));
        s.push('\n');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ldaw-soundlib-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    const KIT: &str = r#"
name = "phonk"
description = "hard kicks"
license = "CC0-1.0"

[[piece]]
file = "kick_deep.wav"
role = "kick"
choke_group = 0
gain_db = -1.5

[[piece]]
file = "hat_closed_tight.wav"
role = "hat_closed"
choke_group = 1

[[piece]]
file = "808_sub.wav"
role = "808"
root_key = "C1"

[[piece]]
file = "../escape.wav"
role = "kick"
"#;

    #[test]
    fn note_names_use_c4_is_60() {
        assert_eq!(note_key("C4"), Some(60));
        assert_eq!(note_key("C2"), Some(36));
        assert_eq!(note_key("C1"), Some(24));
        assert_eq!(note_key("C#5"), Some(73));
        assert_eq!(note_key("Bb3"), Some(58));
        assert_eq!(note_key("H4"), None);
        assert_eq!(note_key("C"), None);
        assert_eq!(note_key("G10"), None);
    }

    #[test]
    fn kit_manifest_is_read_and_unsafe_names_are_dropped() {
        let mut idx = HashMap::new();
        idx.insert("kick_deep.wav".to_string(), "ab".repeat(32));
        let k = parse_kit(KIT, Path::new("/x/phonk"), Source::Pack, &idx).unwrap();
        assert_eq!(k.id, "phonk");
        assert_eq!(k.title, "Phonk");
        assert_eq!(k.pieces.len(), 3, "the path with .. is dropped");
        assert_eq!(k.pieces[0].name, "Kick Deep");
        assert_eq!(k.pieces[0].gain_db, -1.5);
        assert_eq!(
            k.pieces[0].sha256.as_deref(),
            Some("ab".repeat(32).as_str())
        );
        assert_eq!(k.pieces[1].choke_group, 1);
        assert_eq!(k.pieces[2].root_key, Some(24));
        assert_eq!(k.pieces[2].path, Path::new("/x/phonk/808_sub.wav"));
        assert_eq!(role_title("hat_closed"), "Closed Hat");
        assert_eq!(role_group("808"), "808");
        assert_eq!(role_group("kick"), "Drum");
    }

    #[test]
    fn index_hashes_are_per_pack() {
        let text = "index_version = 1\n[[pack]]\nname = \"a\"\n[[pack.file]]\npath = \"k.wav\"\nsha256 = \"AA\"\n[[pack]]\nname = \"b\"\n[[pack.file]]\npath = \"k.wav\"\nsha256 = \"bb\"\n";
        assert_eq!(parse_index(text, "a")["k.wav"], "aa");
        assert_eq!(parse_index(text, "b")["k.wav"], "bb");
        assert!(parse_index(text, "c").is_empty());
    }

    #[test]
    fn search_roots_follow_the_xdg_rules() {
        let r = search_roots(
            Some("/usr/local/share:/usr/share:relative"),
            None,
            Some(Path::new("/home/u")),
            Some(Path::new("/dev/packs")),
        );
        assert_eq!(
            r,
            vec![
                PathBuf::from("/dev/packs"),
                PathBuf::from("/home/u/.local/share/libredaw/sounds"),
                PathBuf::from("/usr/local/share/libredaw/sounds"),
                PathBuf::from("/usr/share/libredaw/sounds"),
            ]
        );
        // The default data dirs apply when the variable is unset; the
        // data home wins over the home directory.
        let r = search_roots(
            None,
            Some(Path::new("/d")),
            Some(Path::new("/home/u")),
            None,
        );
        assert_eq!(r[0], PathBuf::from("/d/libredaw/sounds"));
        assert!(r.contains(&PathBuf::from("/usr/share/libredaw/sounds")));
    }

    #[test]
    fn discovery_finds_kits_and_verifies_hashes() {
        let root = temp("discover");
        let kit = root.join("phonk");
        fs::create_dir_all(&kit).unwrap();
        fs::write(kit.join("kick_deep.wav"), b"RIFF-not-really").unwrap();
        fs::write(kit.join("kit.toml"), KIT).unwrap();
        let hash = sha256_hex(b"RIFF-not-really");
        fs::write(
            root.join("index.toml"),
            format!(
                "[[pack]]\nname = \"phonk\"\n[[pack.file]]\npath = \"kick_deep.wav\"\nsha256 = \"{hash}\"\n"
            ),
        )
        .unwrap();
        // A directory without kit.toml is not a kit.
        fs::create_dir_all(root.join("junk")).unwrap();
        let kits = discover(&[root.clone(), root.join("missing")]);
        assert_eq!(kits.len(), 1);
        let p = &kits[0].pieces[0];
        assert_eq!(verify(p), Ok(()));
        // Damage the file: the index no longer matches.
        fs::write(kit.join("kick_deep.wav"), b"tampered").unwrap();
        assert!(verify(p).unwrap_err().contains("does not match"));
        // A piece the index does not list has nothing to check.
        assert_eq!(verify(&kits[0].pieces[1]), Ok(()));
        let _ = fs::remove_dir_all(&root);
    }

    /// With `LIBREDAW_SOUNDS_DIR` pointing at the real packs, every piece
    /// of every kit is listed with a hash that matches its file.
    #[test]
    fn the_real_packs_verify_when_available() {
        let Some(dir) = std::env::var_os("LIBREDAW_SOUNDS_DIR") else {
            return;
        };
        let kits = discover(&[PathBuf::from(dir)]);
        assert!(!kits.is_empty());
        for k in &kits {
            for p in &k.pieces {
                assert!(p.sha256.is_some(), "{} has no hash", p.file);
                assert_eq!(verify(p), Ok(()), "{}", p.file);
            }
        }
    }

    #[test]
    fn a_user_folder_becomes_one_local_kit() {
        let root = temp("folder");
        fs::create_dir_all(root.join("sub")).unwrap();
        fs::write(root.join("Big Kick.WAV"), b"x").unwrap();
        fs::write(root.join("sub/hat 1.wav"), b"x").unwrap();
        fs::write(root.join("notes.txt"), b"x").unwrap();
        let k = scan_folder(&root).unwrap();
        assert_eq!(k.source, Source::UserFolder);
        assert_eq!(k.pieces.len(), 2);
        let kick = k.pieces.iter().find(|p| p.name == "Big Kick").unwrap();
        assert_eq!(kick.role, "kick");
        assert!(k.pieces.iter().any(|p| p.role == "hat_closed"));
        let empty = temp("empty");
        assert!(scan_folder(&empty).is_none());
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&empty);
    }

    #[test]
    fn folder_list_round_trips() {
        let f = vec![PathBuf::from("/a/b"), PathBuf::from("/c d/e\"f")];
        assert_eq!(parse_folders(&emit_folders(&f)), f);
        assert!(parse_folders("[[folder]]\npath = \"relative\"\n").is_empty());
    }

    #[test]
    fn titles_and_guesses() {
        assert_eq!(title_case("kick_distorted"), "Kick Distorted");
        assert_eq!(title_case("808_long"), "808 Long");
        assert_eq!(guess_role("Open HH 3"), "hat_closed");
        assert_eq!(guess_role("weird"), "perc");
    }
}
