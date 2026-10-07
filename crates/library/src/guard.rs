// SPDX-License-Identifier: GPL-3.0-or-later
//! The licensing guard (SPEC 15.3, 17.2 fmt-4 and license-1).
//!
//! SPEC 15.3: "Samples from user libraries are marked `local_only` in
//! `project.toml`. 'Export project for sharing' leaves them out by default
//! and lists them, so sharing a project does not redistribute them by
//! accident." SPEC 17.2: "`local_only` samples are never copied into the
//! bundle: they are referenced by hash plus a per-machine path stored
//! outside the bundle."
//!
//! So a `local_only` sample may leave the machine in one form only: its
//! content hash. This module is the single place that decides it.

use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use crate::index::SoundEntry;

/// Where a sample is going.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Destination {
    /// The project's own bundle on this machine (`samples/`).
    Bundle,
    /// Export project for sharing, or any upload.
    SharedExport,
}

/// How the sample would travel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delivery {
    /// The audio bytes are copied.
    Bytes,
    /// Only the content hash, listed so the recipient can supply their own copy.
    HashOnly,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GuardError {
    /// A `local_only` sample may not be copied to this destination.
    LocalOnly {
        id: String,
        destination: Destination,
    },
    Io(String),
}

impl std::fmt::Display for GuardError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GuardError::LocalOnly { id, destination } => write!(
                f,
                "sample {id} is local only and cannot be copied into {}; reference it by hash",
                match destination {
                    Destination::Bundle => "the project bundle",
                    Destination::SharedExport => "a shared export",
                }
            ),
            GuardError::Io(m) => write!(f, "cannot read the sample: {m}"),
        }
    }
}

impl std::error::Error for GuardError {}

/// What may be written for a sample.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reference {
    /// Allowed to copy the bytes (not a `local_only` sample).
    Copy,
    /// Write only `{hash, orig_name, size, local_only = true}`.
    ByHash {
        hash: String,
        orig_name: String,
        size: u64,
    },
}

/// SHA-256 of the file, the same hash the bundle uses.
pub fn content_hash(path: &Path) -> io::Result<String> {
    let mut f = File::open(path)?;
    let mut h = doc::sha256::Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(doc::sha256::to_hex(&h.finalize()))
}

/// Decides how `entry` may be written to `dest`.
///
/// A `local_only` sample is refused with `Delivery::Bytes` for every
/// destination, and accepted with `Delivery::HashOnly` as a hash
/// reference. Any other sample is a plain copy.
pub fn authorize(
    entry: &SoundEntry,
    dest: Destination,
    delivery: Delivery,
) -> Result<Reference, GuardError> {
    if !entry.local_only {
        return Ok(Reference::Copy);
    }
    if delivery == Delivery::Bytes {
        return Err(GuardError::LocalOnly {
            id: entry.id.clone(),
            destination: dest,
        });
    }
    let hash = content_hash(&entry.path).map_err(|e| GuardError::Io(e.to_string()))?;
    Ok(Reference::ByHash {
        hash,
        orig_name: entry
            .rel
            .rsplit('/')
            .next()
            .unwrap_or(&entry.rel)
            .to_string(),
        size: entry.size,
    })
}

/// For "Export project for sharing": splits the used samples into those
/// that may be bundled and those that are left out and listed.
pub fn partition_for_export<'a>(
    used: &[&'a SoundEntry],
) -> (Vec<&'a SoundEntry>, Vec<&'a SoundEntry>) {
    used.iter().copied().partition(|e| !e.local_only)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::{Source, scan};
    use crate::testutil::{TempDir, fl_like_tree};

    fn entry() -> (TempDir, SoundEntry) {
        let t = TempDir::new("guard");
        fl_like_tree(t.path());
        let s = Source::user_folder(&t.path().join("Packs"));
        let r = scan(&s, &t.path().join("cache")).unwrap();
        let e = r
            .index
            .entries
            .into_iter()
            .find(|e| e.name == "909 Kick")
            .unwrap();
        (t, e)
    }

    #[test]
    fn bytes_are_refused_everywhere() {
        let (_t, e) = entry();
        for d in [Destination::Bundle, Destination::SharedExport] {
            assert_eq!(
                authorize(&e, d, Delivery::Bytes),
                Err(GuardError::LocalOnly {
                    id: e.id.clone(),
                    destination: d
                })
            );
        }
    }

    #[test]
    fn hash_reference_matches_the_file() {
        let (_t, e) = entry();
        let r = authorize(&e, Destination::SharedExport, Delivery::HashOnly).unwrap();
        let bytes = std::fs::read(&e.path).unwrap();
        assert_eq!(
            r,
            Reference::ByHash {
                hash: doc::sha256::sha256_hex(&bytes),
                orig_name: "909 Kick.wav".into(),
                size: bytes.len() as u64,
            }
        );
    }

    #[test]
    fn non_local_samples_may_be_copied_and_partition_splits() {
        let (_t, mut e) = entry();
        let local = e.clone();
        e.local_only = false;
        assert_eq!(
            authorize(&e, Destination::Bundle, Delivery::Bytes),
            Ok(Reference::Copy)
        );
        let (ok, left) = partition_for_export(&[&e, &local]);
        assert_eq!((ok.len(), left.len()), (1, 1));
    }
}
