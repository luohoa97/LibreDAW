// SPDX-License-Identifier: GPL-3.0-or-later
//! Control socket server for scripts and agents (SPEC 16, 17.1).
//!
//! Owns the Unix socket, client threads, hello and capability checks,
//! line limits, approval bookkeeping, and request deadlines. It hands
//! validated requests to the GTK thread through `ControlServer::poll`; the
//! `ui` crate answers them with access to the document. See
//! `docs/phase2-interfaces.md`.
//!
//! No GTK dependency: `ui` calls `poll` from its 10 ms tick.

pub mod analysis;
#[cfg(test)]
mod analysis_tests;
mod client;
mod socket;
mod state;

pub use state::{
    ClientInfo, ControlConfig, ControlServer, ControlStartError, Incoming, Polled, Ticket, UiEvent,
};
