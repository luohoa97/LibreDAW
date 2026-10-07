// SPDX-License-Identifier: GPL-3.0-or-later
//! The user's own FL Studio sounds in the Sounds pane (SPEC 15.3), without
//! GTK: what is remembered, what a scan gives, how the sounds are searched
//! and filtered, and how a sound becomes a sampler setup.
//!
//! The files are read in place and never copied or shared (`local_only`).
//! The pane holds metadata only; audio is decoded when a sound is added or
//! previewed (SPEC 23).

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use doc::persist::{key_values, quote, unquote};
use library::index::SoundEntry;
use library::kits::SLOTS;
use library::{FlInstall, Instrument, Kit, LibraryIndex};
use protocol::beats::SampleMode;
use protocol::model::SampleRef;

use crate::channels::SamplerSetup;
use crate::soundlib;

/// Everything a finished scan gives, shared with the pane.
pub struct Loaded {
    pub name: String,
    pub index: LibraryIndex,
    pub kits: Vec<Kit>,
    pub instruments: Vec<Instrument>,
    /// False for the fast first pass: numbered multisamples are not placed
    /// yet, so there are no instruments.
    pub complete: bool,
}

/// Where the pane stands.
#[derive(Clone)]
pub enum Status {
    /// Nothing chosen yet.
    Off,
    Scanning,
    /// The folder could not be read.
    Failed,
    Ready(Arc<Loaded>),
}

thread_local! {
    static STATUS: RefCell<Status> = const { RefCell::new(Status::Off) };
}

pub fn status() -> Status {
    STATUS.with(|s| s.borrow().clone())
}

pub fn set_status(s: Status) {
    STATUS.with(|c| *c.borrow_mut() = s);
}

// ---------------------------------------------------------------------------
// What is remembered

/// The `fl-library.toml` text for an install the user chose.
pub fn emit(install: &FlInstall) -> String {
    format!(
        "# The FL Studio sounds Oto reads in place.\nname = {}\nversion = {}\npacks = {}\n",
        quote(&install.name),
        quote(&install.version),
        quote(&install.packs.to_string_lossy()),
    )
}

pub fn parse(text: &str) -> Option<FlInstall> {
    let (mut name, mut version, mut packs) = (None, String::new(), None);
    for (k, v) in key_values(text) {
        match k {
            "name" => name = unquote(v),
            "version" => version = unquote(v).unwrap_or_default(),
            "packs" => packs = unquote(v).map(PathBuf::from),
            _ => {}
        }
    }
    let packs = packs.filter(|p| p.is_absolute())?;
    Some(FlInstall {
        name: name.filter(|n| !n.is_empty())?,
        version,
        prefix: packs.clone(),
        packs,
        source: library::InstallSource::Folder,
    })
}

pub fn file(config: &Path) -> PathBuf {
    config.join("libredaw").join("fl-library.toml")
}

pub fn load(config: &Path) -> Option<FlInstall> {
    parse(&std::fs::read_to_string(file(config)).ok()?)
}

pub fn save(config: &Path, install: &FlInstall) -> std::io::Result<()> {
    let f = file(config);
    if let Some(d) = f.parent() {
        std::fs::create_dir_all(d)?;
    }
    let tmp = f.with_extension("toml.tmp");
    std::fs::write(&tmp, emit(install))?;
    std::fs::rename(&tmp, &f)
}

// ---------------------------------------------------------------------------
// Scanning (runs on a worker thread)

/// Scans `install`: the fast pass reads headers only; the full pass also
/// places numbered multisamples on the keyboard. `None` when the folder
/// cannot be read or holds no sounds.
pub fn scan(install: &FlInstall, cache: &Path, full: bool) -> Option<Loaded> {
    let src = library::Source::fl_studio(install);
    let r = if full {
        library::scan(&src, cache)
    } else {
        library::scan_with(&src, cache, false)
    }
    .ok()?;
    if r.index.entries.is_empty() {
        return None;
    }
    let kits = library::build_kits(&r.index);
    let instruments = if full {
        library::build_instruments(&r.index)
    } else {
        Vec::new()
    };
    Some(Loaded {
        name: install.name.clone(),
        index: r.index,
        kits,
        instruments,
        complete: full,
    })
}

// ---------------------------------------------------------------------------
// Names, roles, search

/// The role names of the filter dropdown a sound shows under (the same
/// words `browser::roles` lists). A kick tagged "808" shows under 808 too.
pub fn filter_roles(e: &SoundEntry) -> Vec<&'static str> {
    let has = |t: &str| e.tags.iter().any(|x| x == t);
    let mut out = match e.role.as_str() {
        "kick" | "snare" | "hat" | "clap" | "perc" | "cymbal" | "tom" => vec!["Drum"],
        "808" => vec!["808"],
        "bass" => vec!["Bass"],
        "keys" => vec!["Keys"],
        "guitar" => vec!["Pluck"],
        "orchestral" => vec!["Strings"],
        "riser" | "sfx" | "fx" => vec!["FX"],
        _ => Vec::new(),
    };
    if has("808") && !out.contains(&"808") {
        out.push("808");
    }
    out
}

/// "Kick", "Closed Hat", "808", "Sound FX".
pub fn role_title(role: &str) -> String {
    match role {
        "808" => "808".into(),
        "sfx" | "fx" => "Sound FX".into(),
        other => soundlib::title_case(other),
    }
}

/// The search words every sound answers to.
fn matches_text(fields: &[&str], search: &str) -> bool {
    let q = search.trim().to_lowercase();
    q.is_empty()
        || q.split_whitespace()
            .all(|w| fields.iter().any(|f| f.to_lowercase().contains(w)))
}

/// "FL Studio: Drums" to "Drums": the pack as a person reads it.
pub fn pack_title(pack: &str) -> &str {
    pack.split_once(": ").map_or(pack, |(_, p)| p)
}

pub fn sound_matches(e: &SoundEntry, search: &str, role: Option<&str>) -> bool {
    if let Some(r) = role
        && !filter_roles(e).contains(&r)
    {
        return false;
    }
    let tags = e.tags.join(" ");
    matches_text(
        &[&e.name, &role_title(&e.role), &tags, &e.pack, "fl studio"],
        search,
    )
}

pub fn kit_matches(k: &Kit, search: &str, role: Option<&str>) -> bool {
    if role.is_some_and(|r| r != "Drum") {
        return false;
    }
    matches_text(&[&k.name, &k.pack, "kit", "drums", "fl studio"], search)
}

pub fn instrument_matches(i: &Instrument, search: &str, role: Option<&str>) -> bool {
    let probe = SoundEntry {
        role: i.role.clone(),
        tags: Vec::new(),
        ..blank_entry()
    };
    if let Some(r) = role
        && !filter_roles(&probe).contains(&r)
    {
        return false;
    }
    matches_text(
        &[
            &i.name,
            &role_title(&i.role),
            &i.pack,
            "instrument",
            "fl studio",
        ],
        search,
    )
}

pub(crate) fn blank_entry() -> SoundEntry {
    SoundEntry {
        id: String::new(),
        name: String::new(),
        role: String::new(),
        tags: Vec::new(),
        pack: String::new(),
        kit: None,
        instrument: None,
        group: None,
        seq: None,
        rel: String::new(),
        size: 0,
        mtime_ms: 0,
        format: library::header::Format::Wav,
        sample_rate: 0,
        channels: 0,
        frames: 0,
        tempo_bpm: None,
        key: None,
        root_note: None,
        root_source: None,
        local_only: true,
        path: PathBuf::new(),
    }
}

// ---------------------------------------------------------------------------
// Becoming instruments

/// A sampler setup for one sound: pitched when the name or the scan found
/// a root key, a one-shot otherwise. `sample` is the registered file.
pub fn sound_setup(e: &SoundEntry, sample: SampleRef) -> SamplerSetup {
    SamplerSetup {
        sample: Some(sample),
        name: e.name.clone(),
        root_key: e.root_note.unwrap_or(60),
        mode: if e.root_note.is_some() {
            SampleMode::Pitched
        } else {
            SampleMode::OneShot
        },
        choke_group: 0,
        gain_db: 0.0,
        pan: 0.0,
    }
}

/// The slot names of a kit as they are shown, in kit order.
pub fn slot_title(slot: &str) -> &'static str {
    match slot {
        "kick" => "Kick",
        "snare" => "Snare",
        "clap" => "Clap",
        "hat" => "Closed Hat",
        "open_hat" => "Open Hat",
        _ => "Percussion",
    }
}

/// The sounds of a kit in slot order: (slot, entry).
pub fn kit_sounds<'a>(kit: &Kit, index: &'a LibraryIndex) -> Vec<(&'static str, &'a SoundEntry)> {
    SLOTS
        .iter()
        .filter_map(|slot| {
            let id = kit.slots.get(*slot)?;
            Some((*slot, index.find(id)?))
        })
        .collect()
}

/// The setup of a kit piece: the two hats choke each other.
pub fn kit_piece_setup(slot: &str, e: &SoundEntry, sample: SampleRef) -> SamplerSetup {
    let mut s = SamplerSetup::one_shot(sample);
    s.name = slot_title(slot).to_string();
    if matches!(slot, "hat" | "open_hat") {
        s.choke_group = 1;
    }
    s.root_key = e.root_note.unwrap_or(60);
    s
}

/// The sound of a multisampled instrument that Oto plays on all keys: the
/// one whose root is nearest to middle C. The sampler has no key zones yet,
/// so this one sample is pitched across the keyboard.
pub fn instrument_root<'a>(
    inst: &Instrument,
    index: &'a LibraryIndex,
) -> Option<(u8, &'a SoundEntry)> {
    let z = inst
        .zones
        .iter()
        .min_by_key(|z| (i16::from(z.root) - 60).abs())?;
    Some((z.root, index.find(&z.sound_id)?))
}

/// The setup of an instrument played from its one root sample.
pub fn instrument_setup(root: u8, name: &str, sample: SampleRef) -> SamplerSetup {
    SamplerSetup {
        sample: Some(sample),
        name: name.to_string(),
        root_key: root,
        mode: SampleMode::Pitched,
        choke_group: 0,
        gain_db: 0.0,
        pan: 0.0,
    }
}

/// A key for the engine's sample store that is not a content hash: audition
/// reads the file once and needs no hashing of it.
pub fn preview_key(path: &Path) -> [u8; 32] {
    let mut h = doc::sha256::Sha256::new();
    h.update(b"oto-preview:");
    h.update(path.to_string_lossy().as_bytes());
    h.finalize()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn install() -> FlInstall {
        FlInstall {
            name: "FL Studio 2026".into(),
            version: "2026".into(),
            packs: PathBuf::from("/data/FL Studio 2026/Data/Patches/Packs"),
            prefix: PathBuf::from("/data/FL Studio 2026"),
            source: library::InstallSource::Folder,
        }
    }

    #[test]
    fn choice_round_trips() {
        let i = install();
        let back = parse(&emit(&i)).unwrap();
        assert_eq!(back.name, i.name);
        assert_eq!(back.packs, i.packs);
        assert_eq!(parse(""), None);
        assert_eq!(parse("name = \"x\"\npacks = \"relative\"\n"), None);
    }

    #[test]
    fn choice_file_round_trips_on_disk() {
        let dir = std::env::temp_dir().join(format!("oto-fl-choice-{}", std::process::id()));
        assert!(load(&dir).is_none());
        save(&dir, &install()).unwrap();
        assert_eq!(load(&dir).unwrap().name, "FL Studio 2026");
        let _ = std::fs::remove_dir_all(dir);
    }

    fn entry(name: &str, role: &str, tags: &[&str]) -> SoundEntry {
        SoundEntry {
            name: name.into(),
            role: role.into(),
            tags: tags.iter().map(|t| t.to_string()).collect(),
            pack: "FL Studio: Drums".into(),
            ..blank_entry()
        }
    }

    #[test]
    fn eight_oh_eight_kicks_show_under_both() {
        let k = entry("808 Kick", "kick", &["808", "kicks"]);
        assert_eq!(filter_roles(&k), vec!["Drum", "808"]);
        assert!(sound_matches(&k, "", Some("808")));
        assert!(sound_matches(&k, "", Some("Drum")));
        assert!(!sound_matches(&k, "", Some("Bass")));
        let plain = entry("909 Kick", "kick", &["kicks"]);
        assert!(!sound_matches(&plain, "", Some("808")));
    }

    #[test]
    fn search_covers_name_role_tags_and_pack() {
        let k = entry("909 Kick", "kick", &["kicks", "legacy"]);
        for q in ["909", "kick", "legacy", "drums", "fl studio", "909 drums"] {
            assert!(sound_matches(&k, q, None), "{q}");
        }
        assert!(!sound_matches(&k, "snare", None));
    }

    #[test]
    fn plain_words_only() {
        for r in library::classify::ROLES {
            assert!(
                crate::vocabulary::violations(&role_title(r)).is_empty(),
                "{r}"
            );
        }
    }

    fn wav(seed: u8) -> Vec<u8> {
        let data: Vec<u8> = (0..400u32)
            .flat_map(|i| (((i as i16) * 40) ^ i16::from(seed)).to_le_bytes())
            .collect();
        let mut v = b"RIFF".to_vec();
        v.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
        v.extend_from_slice(b"WAVEfmt ");
        v.extend_from_slice(&16u32.to_le_bytes());
        v.extend_from_slice(&1u16.to_le_bytes());
        v.extend_from_slice(&1u16.to_le_bytes());
        v.extend_from_slice(&44_100u32.to_le_bytes());
        v.extend_from_slice(&88_200u32.to_le_bytes());
        v.extend_from_slice(&2u16.to_le_bytes());
        v.extend_from_slice(&16u16.to_le_bytes());
        v.extend_from_slice(b"data");
        v.extend_from_slice(&(data.len() as u32).to_le_bytes());
        v.extend_from_slice(&data);
        v
    }

    #[test]
    fn a_fixture_tree_scans_into_a_kit_and_sounds() {
        let root = std::env::temp_dir().join(format!("oto-fl-tree-{}", std::process::id()));
        let packs = root.join("FL Studio 2026/Data/Patches/Packs/Drums");
        for (i, f) in [
            "Kicks/909 Kick.wav",
            "Snares/909 Snare.wav",
            "Hats/909 Hat.wav",
        ]
        .iter()
        .enumerate()
        {
            let p = packs.join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, wav(i as u8)).unwrap();
        }
        let install = library::install_from_folder(&root.join("FL Studio 2026")).unwrap();
        let cache = root.join("cache");
        let fast = scan(&install, &cache, false).unwrap();
        assert!(!fast.complete && fast.instruments.is_empty());
        assert_eq!(fast.index.entries.len(), 3);
        assert!(fast.index.entries.iter().all(|e| e.local_only));
        let kit = library::default_kit(&fast.kits).expect("a kit");
        assert_eq!(kit.name, "909");
        let slots: Vec<&str> = kit_sounds(kit, &fast.index)
            .iter()
            .map(|(s, _)| *s)
            .collect();
        assert_eq!(slots, ["kick", "snare", "hat"]);
        assert!(scan(&install, &cache, true).unwrap().complete);
        // A folder with no sounds is a failed scan, not an empty list.
        std::fs::create_dir_all(root.join("Empty")).unwrap();
        let empty = library::install_from_folder(&root.join("Empty")).unwrap();
        assert!(scan(&empty, &cache, false).is_none());
        // Nothing was written into the FL folder except what was there.
        assert!(!root.join("FL Studio 2026").join("cache").exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn pitched_sounds_keep_their_root() {
        let mut e = entry("Rhodes C3", "keys", &[]);
        e.root_note = Some(48);
        let s = SampleRef {
            hash: "h".into(),
            orig_name: "a.wav".into(),
            size: 1,
            local_only: true,
        };
        let setup = sound_setup(&e, s.clone());
        assert_eq!(setup.root_key, 48);
        assert_eq!(setup.mode, SampleMode::Pitched);
        let drum = sound_setup(&entry("Kick", "kick", &[]), s);
        assert_eq!(drum.mode, SampleMode::OneShot);
    }

    /// A real FL Studio kick through the engine's own loader: Oto must
    /// decode what FL stores (Ogg Vorbis inside WAV) and get a signal.
    /// Needs an FL Studio install: set `OTO_FL_PACKS` to its Packs folder,
    /// or have one Oto can detect.
    #[test]
    #[ignore = "needs the user's FL Studio install"]
    fn real_fl_kick_sounds() {
        let packs = std::env::var_os("OTO_FL_PACKS")
            .map(PathBuf::from)
            .or_else(|| {
                library::detect_installs()
                    .into_iter()
                    .next()
                    .map(|i| i.packs)
            })
            .expect("no FL Studio install found; set OTO_FL_PACKS");
        let install = library::install_from_folder(&packs).unwrap();
        let cache = std::env::temp_dir().join(format!("oto-fl-real-{}", std::process::id()));
        let loaded = scan(&install, &cache, false).expect("scan");
        let kick = loaded
            .index
            .entries
            .iter()
            .find(|e| e.role == "kick" && e.format == library::header::Format::WavVorbis)
            .or_else(|| loaded.index.entries.iter().find(|e| e.role == "kick"))
            .expect("a kick");
        let data = engine::samples::load_sample_file(&kick.path, 48_000).expect("decode");
        let peak = data.data.iter().fold(0.0f32, |p, s| p.max(s.abs()));
        println!("{} ({:?}): peak {peak}", kick.rel, kick.format);
        assert!(peak > 0.0);
        let _ = std::fs::remove_dir_all(cache);
    }
}
