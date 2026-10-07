// SPDX-License-Identifier: GPL-3.0-or-later
//! Entity ids (5.3). All ids in a project come from one counter, so an id
//! is unique across entity kinds and never reused, including after undo.

use serde::{Deserialize, Serialize};

macro_rules! id_type {
    ($($(#[$m:meta])* $name:ident),* $(,)?) => {$(
        $(#[$m])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub u32);

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                self.0.fmt(f)
            }
        }
    )*};
}

id_type!(
    /// A channel (instrument row).
    ChannelId,
    /// A pattern.
    PatternId,
    /// A note inside a pattern.
    NoteId,
    /// A mixer track. `TrackId::MASTER` is the master track.
    TrackId,
    /// A plugin or built-in effect instance (channel instrument or track insert).
    InstanceId,
    /// A playlist track (15.6).
    PlaylistTrackId,
    /// A pattern clip on the playlist (15.6).
    ClipId,
);

impl TrackId {
    /// The master track always has id 0; the id counter starts at 1.
    pub const MASTER: TrackId = TrackId(0);
}

/// First id handed out by a new project's counter.
pub const FIRST_ID: u32 = 1;
