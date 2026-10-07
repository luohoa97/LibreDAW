// SPDX-License-Identifier: GPL-3.0-or-later
//! The control socket speaks MCP (SPEC 18.3): JSON-RPC 2.0, one message per
//! line, the revision pinned in [`PROTOCOL_VERSION`].
//!
//! A connection is MCP when its first line is a JSON-RPC message (the
//! client's `initialize`); a connection whose first line is the hello object
//! stays on the line protocol of 17.1 (Deno scripts). One socket serves both
//! so the DAW has one listener, one lock file, and one permission check, and
//! `libredaw-mcp` stays a plain byte relay.
//!
//! Tool calls become `RequestBody` values that go to the UI through the same
//! `poll()`/`reply()` ticket path as script requests, so the UI cannot tell
//! an MCP agent from the old bridge.

pub mod build;
pub mod compose;
pub(crate) mod exec;
pub mod grid;
pub mod ids;
pub mod notes;
pub(crate) mod prompts;
pub(crate) mod resources;
pub(crate) mod sanitize;
pub(crate) mod serve;
pub(crate) mod session;
pub mod summary;
pub mod tools;

pub(crate) use session::Session;

/// The one MCP protocol revision this server speaks. A client that asks for
/// another gets this one back in the `initialize` result and decides
/// whether it can continue (as the specification requires).
pub const PROTOCOL_VERSION: &str = "2025-06-18";
