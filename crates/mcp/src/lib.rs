// SPDX-License-Identifier: GPL-3.0-or-later
//! `libredaw-mcp`: the stdio side of LibreDAW's agent control (SPEC 16,
//! 18.3).
//!
//! The DAW's control socket speaks MCP itself (`crates/control`), so this
//! binary is a byte relay between an AI client's stdio and that socket
//! ([`relay`]), plus the `setup` subcommand that registers it with the
//! clients found on the machine ([`setup`]).

pub mod relay;
pub mod setup;

/// The MCP protocol revision the DAW speaks (defined in `control`).
pub use control::PROTOCOL_VERSION;
