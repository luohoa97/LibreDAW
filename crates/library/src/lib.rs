// SPDX-License-Identifier: GPL-3.0-or-later
//! User sound libraries, with the FL Studio install as the first source
//! (SPEC 15.3 "User libraries").
//!
//! Oto never redistributes FL Studio content. This crate only finds the
//! user's own install on their machine, reads file headers, and indexes the
//! files in place. Nothing is copied, mirrored or uploaded, and every entry
//! is `local_only` (see [`guard`]).

pub mod classify;
pub mod detect;
pub mod guard;
pub mod header;
pub mod index;
pub mod kits;
pub mod pitch;
pub mod zones;

pub use detect::{FlInstall, InstallSource, detect_installs, install_from_folder};
pub use index::{
    LibraryIndex, ScanResult, ScanStats, SoundEntry, Source, default_cache_dir, scan, scan_with,
};
pub use kits::{Kit, build_kits, default_kit};
pub use zones::{Instrument, Zone, build_instruments, unplaced_instruments};

#[cfg(test)]
pub(crate) mod testutil;
