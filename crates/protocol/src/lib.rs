// SPDX-License-Identifier: GPL-3.0-or-later
//! Shared types for LibreDAW.
//!
//! This crate is owned by the orchestrator. Other crates read it but never
//! edit it; interface changes go through the orchestrator. It holds only
//! data types, fixed limits, layouts shared between threads, validation, and
//! the project file format. No audio, UI, or plugin code lives here.
//!
//! Section numbers in comments refer to `SPEC.md`.

pub mod consts;
pub mod control;
pub mod edit;
pub mod engine;
pub mod format;
pub mod ids;
pub mod model;
pub mod validate;
