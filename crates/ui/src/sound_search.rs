// SPDX-License-Identifier: GPL-3.0-or-later
//! Sound search and kit lookup for the control API (SPEC 18.4), as plain
//! functions over discovered kits (no GTK). Packs are the installed kits
//! (their directory name is the pack name); a folder the user added is the
//! pack "folder" with its directory name as the kit.

use protocol::control::{SoundInfo, agent_string};

use crate::soundlib::{self, Kit, Source};

/// The most results one search returns.
pub const MAX_RESULTS: u32 = 50;

/// The pack name a kit is listed under.
pub fn pack_of(kit: &Kit) -> String {
    match kit.source {
        Source::Pack => kit.id.clone(),
        Source::UserFolder => "folder".to_string(),
    }
}

fn eq(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

/// Whether `role` (as asked: "kick", "Drums", "808") matches a piece role
/// ("kick", "hat_closed", "808"): the exact role, its title ("Hat
/// Closed"), or its group ("Drums").
fn role_matches(want: &str, role: &str) -> bool {
    eq(want, role)
        || eq(want, &soundlib::role_title(role))
        || eq(want, soundlib::role_group(role))
        || role.split('_').next().is_some_and(|first| eq(want, first))
}

/// Searches every piece of `kits`. `genre` matches the kit's pack or title
/// ("phonk", "Boom Bap"); `tags` must all appear in the piece's name,
/// role, or kit title. Results keep the kit order, at most `limit`
/// (itself at most `MAX_RESULTS`).
pub fn search(
    kits: &[Kit],
    role: Option<&str>,
    genre: Option<&str>,
    tags: &[String],
    limit: u32,
) -> Vec<SoundInfo> {
    let limit = limit.min(MAX_RESULTS) as usize;
    let mut out = Vec::new();
    for kit in kits {
        if let Some(g) = genre.filter(|g| !g.trim().is_empty())
            && !(eq(g, &kit.id) || eq(g, &kit.title) || eq(&g.replace([' ', '_'], "-"), &kit.id))
        {
            continue;
        }
        for p in &kit.pieces {
            if out.len() >= limit {
                return out;
            }
            if let Some(r) = role.filter(|r| !r.trim().is_empty())
                && !role_matches(r, &p.role)
            {
                continue;
            }
            let hay = format!("{} {} {}", p.name, p.role, kit.title).to_lowercase();
            if !tags
                .iter()
                .all(|t| t.trim().is_empty() || hay.contains(&t.trim().to_lowercase()))
            {
                continue;
            }
            out.push(SoundInfo {
                id: agent_string(&format!("{}/{}/{}", pack_of(kit), kit.id, p.file)),
                name: agent_string(&p.name),
                role: agent_string(&p.role),
                genres: match kit.source {
                    Source::Pack => vec![agent_string(&kit.title)],
                    Source::UserFolder => Vec::new(),
                },
                tags: vec![agent_string(soundlib::role_group(&p.role))],
                pack: agent_string(&pack_of(kit)),
                kit: Some(agent_string(&kit.title)),
            });
        }
    }
    out
}

/// The kit `kit` of pack `pack`. Both match by id or title, without case.
pub fn find_kit<'a>(kits: &'a [Kit], pack: &str, kit: &str) -> Option<&'a Kit> {
    kits.iter().find(|k| {
        let pack_ok = eq(pack, &pack_of(k)) || eq(pack, &k.id) || eq(pack, &k.title);
        pack_ok && (eq(kit, &k.id) || eq(kit, &k.title))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::soundlib::Piece;
    use std::path::PathBuf;

    fn piece(name: &str, role: &str) -> Piece {
        Piece {
            file: format!("{}.wav", name.to_lowercase().replace(' ', "_")),
            name: name.into(),
            role: role.into(),
            root_key: None,
            choke_group: 0,
            gain_db: 0.0,
            pan: 0.0,
            length_ms: 0,
            path: PathBuf::from("/x"),
            sha256: None,
        }
    }

    fn kits() -> Vec<Kit> {
        vec![
            Kit {
                id: "boom-bap".into(),
                title: "Boom Bap".into(),
                description: String::new(),
                license: String::new(),
                dir: PathBuf::from("/p/boom-bap"),
                source: Source::Pack,
                pieces: vec![
                    piece("Kick Dusty", "kick"),
                    piece("Hat Closed", "hat_closed"),
                ],
            },
            Kit {
                id: "phonk".into(),
                title: "Phonk".into(),
                description: String::new(),
                license: String::new(),
                dir: PathBuf::from("/p/phonk"),
                source: Source::Pack,
                pieces: vec![piece("Cowbell", "perc"), piece("Kick Hard", "kick")],
            },
            Kit {
                id: "mine".into(),
                title: "Mine".into(),
                description: String::new(),
                license: String::new(),
                dir: PathBuf::from("/home/mine"),
                source: Source::UserFolder,
                pieces: vec![piece("Snare Ugly", "snare")],
            },
        ]
    }

    #[test]
    fn role_genre_and_tags_narrow_the_results() {
        let k = kits();
        let kicks = search(&k, Some("kick"), None, &[], 50);
        assert_eq!(kicks.len(), 2);
        let phonk = search(&k, Some("kick"), Some("phonk"), &[], 50);
        assert_eq!(phonk.len(), 1);
        assert_eq!(phonk[0].name, "Kick Hard");
        assert_eq!(phonk[0].pack, "phonk");
        let boom = search(&k, None, Some("Boom Bap"), &[], 50);
        assert_eq!(boom.len(), 2);
        let hat = search(&k, Some("hat"), None, &[], 50);
        assert_eq!(hat.len(), 1, "a role prefix matches hat_closed");
        let dusty = search(&k, None, None, &["dusty".into()], 50);
        assert_eq!(dusty.len(), 1);
        let folder = search(&k, Some("snare"), None, &[], 50);
        assert_eq!(folder[0].pack, "folder");
        assert!(folder[0].genres.is_empty());
    }

    #[test]
    fn limit_is_capped() {
        let k = kits();
        assert_eq!(search(&k, None, None, &[], 2).len(), 2);
        assert_eq!(search(&k, None, None, &[], 10_000).len(), 5);
    }

    #[test]
    fn kits_are_found_by_id_or_title() {
        let k = kits();
        assert_eq!(find_kit(&k, "boom-bap", "Boom Bap").unwrap().id, "boom-bap");
        assert_eq!(find_kit(&k, "PHONK", "phonk").unwrap().id, "phonk");
        assert_eq!(find_kit(&k, "folder", "mine").unwrap().id, "mine");
        assert!(find_kit(&k, "phonk", "boom-bap").is_none());
    }
}
