// SPDX-License-Identifier: GPL-3.0-or-later
//! Control-socket client (shared with `libredaw-mcp`) and the Deno
//! scripting bridge (SPEC 10, 16.2, 17.1).

pub mod control;
pub mod deno;

pub use control::{Client, ClientError};
