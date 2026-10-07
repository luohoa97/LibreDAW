// SPDX-License-Identifier: GPL-3.0-or-later
//! `libredaw-mcp`: a stdio MCP server that lets an agent control a running
//! LibreDAW through its control socket (SPEC 16, 17.1).
//!
//! Hand-written JSON-RPC 2.0 over newline-delimited stdio. Pinned MCP
//! protocol revision: [`PROTOCOL_VERSION`].

pub mod conn;
pub mod sanitize;
pub mod server;
pub mod setup;
pub mod tools;

/// The one MCP protocol revision this server speaks. A client that asks for
/// another revision gets a JSON-RPC error naming this one.
pub const PROTOCOL_VERSION: &str = "2025-06-18";
