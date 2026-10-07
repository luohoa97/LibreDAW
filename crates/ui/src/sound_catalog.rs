// SPDX-License-Identifier: GPL-3.0-or-later
//! One catalogue of every sound Oto can add (SPEC 15.3, 18.4), for the
//! control API: the built-in drum kits, the Surge XT sounds, the user's FL
//! Studio kits, instruments and sounds (only when the user turned that on)
//! and the user's own sound folders. The Sounds pane lists the same data
//! and filters it with the same words; an agent searches here and adds by
//! id, never by file path. No GTK in this file.

use library::index::SoundEntry;
use plugin_host::sounds::{self, Sound};

use crate::fl_library::{self as fl, Loaded, Status};
use crate::soundlib::{self, Kit, Piece, Source};

pub const OTO: &str = "Oto Kit";
pub const FL: &str = "FL Studio";
pub const FOLDER: &str = "Your Folder";

pub const DRUM_KIT: &str = "drum kit";
pub const INSTRUMENT: &str = "instrument";
pub const SINGLE: &str = "single sound";

/// What the agent is told when the FL library is off.
pub const FL_OFF: &str = "The user's FL Studio sounds are not turned on. Ask them to click Add on 'Use Your FL Studio Sounds' in the Sounds pane.";
pub const FL_SCANNING: &str =
    "The user's FL Studio sounds are still being read. Search again in a few seconds.";
pub const FL_FAILED: &str = "The user's FL Studio folder could not be read. Ask them to open the Sounds pane and choose their FL Studio folder again.";
pub const FL_PARTIAL: &str =
    "FL Studio instruments are still being worked out; search again in a few seconds to see them.";

/// One thing that can be added.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub id: String,
    pub name: String,
    pub role: String,
    pub tags: Vec<String>,
    pub source: &'static str,
    pub kind: &'static str,
    /// The kit, pack or family it belongs to ("Boom Bap", "Drums"); what
    /// `genre` matches.
    pub family: String,
}

/// What a search asks for. Empty strings mean "any".
#[derive(Clone, Debug, Default)]
pub struct Query {
    pub text: String,
    pub role: String,
    pub source: String,
    pub genre: String,
    pub offset: usize,
    pub limit: usize,
}

/// What an id names.
pub enum Target<'a> {
    OtoKit(&'a Kit),
    Piece(&'a Kit, &'a Piece),
    Surge(&'static Sound),
    FlSound(&'a SoundEntry),
    FlKit(&'a library::Kit),
    FlInstrument(&'a library::Instrument),
}

fn eq(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

/// "kicks" and "kick" are the same word.
fn stem(s: &str) -> String {
    let s = s.trim().to_lowercase().replace([' ', '-', '_'], "");
    s.strip_suffix('s').map(str::to_string).unwrap_or(s)
}

/// Whether `want` ("kick", "Drums", "808", "Hat Closed") names the role.
fn role_matches(want: &str, role: &str, extra: &[&str], group: bool) -> bool {
    let w = stem(want);
    let mut names = vec![role.to_string(), soundlib::role_title(role)];
    if group {
        names.push(soundlib::role_group(role).to_string());
        names.push(role.split('_').next().unwrap_or("").to_string());
    }
    names
        .iter()
        .map(String::as_str)
        .chain(extra.iter().copied())
        .any(|r| stem(r) == w)
}

fn text_matches(e: &Entry, text: &str) -> bool {
    let hay = format!(
        "{} {} {} {} {} {}",
        e.name,
        e.role,
        soundlib::role_title(&e.role),
        e.tags.join(" "),
        e.source,
        e.kind
    )
    .to_lowercase();
    text.split_whitespace()
        .all(|w| hay.contains(&w.to_lowercase()))
}

fn source_matches(want: &str, source: &str) -> bool {
    let w = want.trim().to_lowercase();
    let s = source.to_lowercase();
    w.is_empty() || s.contains(&w) || w.contains(&s) || (w == "oto" && s == OTO.to_lowercase())
}

/// The id of a kit piece: `oto:boom-bap/kick_dusty.wav`.
fn piece_id(kit: &Kit, piece: &Piece) -> String {
    format!("{}:{}/{}", prefix(kit.source), kit.id, piece.file)
}

fn kit_id(kit: &Kit) -> String {
    format!("{}:kit:{}", prefix(kit.source), kit.id)
}

fn prefix(s: Source) -> &'static str {
    match s {
        Source::Pack => "oto",
        Source::UserFolder => "folder",
    }
}

fn source_of(s: Source) -> &'static str {
    match s {
        Source::Pack => OTO,
        Source::UserFolder => FOLDER,
    }
}

pub fn surge_id(s: &Sound) -> Option<String> {
    Some(format!("surge:{}/{}", sounds::plugin_of(s)?.key, s.preset))
}

/// The sounds the Surge XT list gives; `installed` says whether the plugin
/// was found (they are listed either way, as the pane does).
fn surge_entries(installed: &dyn Fn(&Sound) -> bool, out: &mut Vec<Entry>) {
    for s in sounds::sounds() {
        let (Some(p), Some(id)) = (sounds::plugin_of(s), surge_id(s)) else {
            continue;
        };
        let mut tags = vec![p.name.to_lowercase()];
        if !installed(s) {
            tags.push("not installed".into());
        }
        out.push(Entry {
            id,
            name: s.label(),
            role: s.role.clone(),
            tags,
            source: SURGE,
            kind: INSTRUMENT,
            family: s.role.clone(),
        });
    }
}

/// The plugin's name as the pane shows it.
pub const SURGE: &str = "Surge XT";

/// Everything, in pane order: built-in kits, Surge XT, FL Studio, then the
/// user's folders.
pub fn build(
    kits: &[Kit],
    installed: &dyn Fn(&Sound) -> bool,
    fl_loaded: Option<&Loaded>,
) -> Vec<Entry> {
    let mut out = Vec::new();
    let kit_entries = |source: Source, out: &mut Vec<Entry>| {
        for k in kits.iter().filter(|k| k.source == source) {
            out.push(Entry {
                id: kit_id(k),
                name: k.title.clone(),
                role: "drums".into(),
                tags: vec!["kit".into(), "drums".into()],
                source: source_of(source),
                kind: DRUM_KIT,
                family: k.title.clone(),
            });
            for p in &k.pieces {
                out.push(Entry {
                    id: piece_id(k, p),
                    name: p.name.clone(),
                    role: p.role.clone(),
                    tags: vec![soundlib::role_group(&p.role).to_lowercase()],
                    source: source_of(source),
                    kind: SINGLE,
                    family: k.title.clone(),
                });
            }
        }
    };
    surge_entries(installed, &mut out);
    if let Some(l) = fl_loaded {
        for k in &l.kits {
            out.push(Entry {
                id: format!("fl:kit:{}", k.id),
                name: k.name.clone(),
                role: "drums".into(),
                tags: vec!["kit".into(), "drums".into()],
                source: FL,
                kind: DRUM_KIT,
                family: fl::pack_title(&k.pack).to_string(),
            });
        }
        for i in &l.instruments {
            out.push(Entry {
                id: format!("fl:instrument:{}", i.id),
                name: i.name.clone(),
                role: i.role.clone(),
                tags: Vec::new(),
                source: FL,
                kind: INSTRUMENT,
                family: fl::pack_title(&i.pack).to_string(),
            });
        }
        for e in &l.index.entries {
            out.push(Entry {
                id: format!("fl:sound:{}", e.id),
                name: e.name.clone(),
                role: e.role.clone(),
                tags: e.tags.clone(),
                source: FL,
                kind: SINGLE,
                family: fl::pack_title(&e.pack).to_string(),
            });
        }
    }
    kit_entries(Source::UserFolder, &mut out);
    out
}

/// Entries that answer `q`, `limit` of them from `offset`, and how many
/// answered in all.
pub fn search(entries: &[Entry], q: &Query) -> (Vec<Entry>, usize) {
    let hits: Vec<&Entry> = entries
        .iter()
        .filter(|e| source_matches(&q.source, e.source))
        .filter(|e| {
            q.role.trim().is_empty() || {
                let extra: Vec<&str> = fl_roles(e);
                role_matches(
                    &q.role,
                    &e.role,
                    &extra,
                    e.source == OTO || e.source == FOLDER,
                )
            }
        })
        .filter(|e| q.genre.trim().is_empty() || eq(&q.genre, &e.family))
        .filter(|e| text_matches(e, &q.text))
        .collect();
    let total = hits.len();
    let page = hits
        .into_iter()
        .skip(q.offset)
        .take(q.limit)
        .cloned()
        .collect();
    (page, total)
}

/// The pane's role filter words an entry also answers to (a kick tagged
/// "808" shows under 808).
fn fl_roles(e: &Entry) -> Vec<&'static str> {
    if e.source != FL {
        return Vec::new();
    }
    if e.kind == DRUM_KIT {
        return vec!["Drum", "kit"];
    }
    let probe = SoundEntry {
        role: e.role.clone(),
        tags: e.tags.clone(),
        ..fl::blank_entry()
    };
    fl::filter_roles(&probe)
}

/// What to tell the agent about the FL library, if it cannot be searched.
pub fn fl_note(status: &Status, remembered: bool) -> Option<&'static str> {
    match status {
        Status::Off if remembered => Some(FL_SCANNING),
        Status::Off => Some(FL_OFF),
        Status::Scanning => Some(FL_SCANNING),
        Status::Failed => Some(FL_FAILED),
        Status::Ready(l) if !l.complete => Some(FL_PARTIAL),
        Status::Ready(_) => None,
    }
}

/// Finds what `id` names.
pub fn resolve<'a>(id: &str, kits: &'a [Kit], fl_loaded: Option<&'a Loaded>) -> Option<Target<'a>> {
    let (kind, rest) = id.split_once(':')?;
    match kind {
        "oto" | "folder" => {
            let source = if kind == "oto" {
                Source::Pack
            } else {
                Source::UserFolder
            };
            let of = |kid: &str| kits.iter().find(|k| k.source == source && k.id == kid);
            if let Some(kid) = rest.strip_prefix("kit:") {
                return of(kid).map(Target::OtoKit);
            }
            let (kid, file) = rest.split_once('/')?;
            let k = of(kid)?;
            k.pieces
                .iter()
                .find(|p| p.file == file)
                .map(|p| Target::Piece(k, p))
        }
        "surge" => {
            let (key, preset) = rest.split_once('/')?;
            sounds::sounds().iter().find_map(|s| {
                let p = sounds::plugin_of(s)?;
                (p.key == key && s.preset == preset).then_some(Target::Surge(s))
            })
        }
        "fl" => {
            let l = fl_loaded?;
            let (what, key) = rest.split_once(':')?;
            match what {
                "sound" => l.index.find(key).map(Target::FlSound),
                "kit" => l.kits.iter().find(|k| k.id == key).map(Target::FlKit),
                "instrument" => l
                    .instruments
                    .iter()
                    .find(|i| i.id == key)
                    .map(Target::FlInstrument),
                _ => None,
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(text: &str, role: &str, source: &str) -> Query {
        Query {
            text: text.into(),
            role: role.into(),
            source: source.into(),
            limit: 50,
            ..Query::default()
        }
    }

    fn entry(id: &str, name: &str, role: &str, source: &'static str, kind: &'static str) -> Entry {
        Entry {
            id: id.into(),
            name: name.into(),
            role: role.into(),
            tags: Vec::new(),
            source,
            kind,
            family: String::new(),
        }
    }

    #[test]
    fn surge_sounds_are_listed_and_found_again() {
        let mut all = Vec::new();
        surge_entries(&|_| true, &mut all);
        assert!(!all.is_empty());
        let e = &all[0];
        assert_eq!(e.source, SURGE);
        match resolve(&e.id, &[], None) {
            Some(Target::Surge(s)) => assert_eq!(s.label(), e.name),
            _ => panic!("{} does not resolve", e.id),
        }
        let (bass, _) = search(&all, &q("", "bass", "surge"));
        assert!(!bass.is_empty() && bass.iter().all(|b| b.role == "Bass"));
    }

    #[test]
    fn roles_sources_text_and_paging() {
        let all = vec![
            entry("a", "909 Kick", "kick", FL, SINGLE),
            entry("b", "Dusty Kick", "kick", OTO, SINGLE),
            entry("c", "Tight Snare", "snare", FL, SINGLE),
            entry("d", "Trap Kit", "drums", FL, DRUM_KIT),
        ];
        assert_eq!(search(&all, &q("kick", "", "FL Studio")).0.len(), 1);
        assert_eq!(search(&all, &q("", "kicks", "")).0.len(), 2);
        assert_eq!(search(&all, &q("", "drums", "")).0.len(), 4, "group Drums");
        let mut paged = q("", "", "FL");
        paged.limit = 2;
        paged.offset = 1;
        let (page, total) = search(&all, &paged);
        assert_eq!((page.len(), total), (2, 3));
        assert_eq!(page[0].id, "c");
    }

    #[test]
    fn fl_is_explained_when_it_is_off() {
        assert_eq!(fl_note(&Status::Off, false), Some(FL_OFF));
        assert_eq!(fl_note(&Status::Failed, true), Some(FL_FAILED));
        assert!(resolve("fl:sound:x", &[], None).is_none());
        assert!(resolve("nonsense", &[], None).is_none());
    }
}
