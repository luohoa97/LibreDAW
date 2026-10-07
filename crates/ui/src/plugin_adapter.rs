// SPDX-License-Identifier: GPL-3.0-or-later
//! The only module that names the `plugin-host` crate (docs/phase2-interfaces.md,
//! "plugin-host -> ui"). All calls are main-thread calls: the GTK thread.

pub use plugin_host::host::{HostError, Instance, ParamInfo, PluginDesc, clap_paths, scan_paths};
use plugin_host::rt::RtOutKind;

/// Scans the standard CLAP paths. Blocks while libraries load, so the
/// caller runs it where a short pause is acceptable (startup, the add
/// dialog), never inside a gesture.
pub fn scan() -> Vec<PluginDesc> {
    plugin_host::host::scan()
}

/// What a plugin reported outside processing (or through the event ring).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PluginOut {
    Param { id: u32, value: f64 },
    GestureBegin { id: u32 },
    GestureEnd { id: u32 },
}

pub fn take_changes(i: &mut Instance) -> Vec<PluginOut> {
    i.take_param_changes()
        .into_iter()
        .map(|e| match e.kind {
            RtOutKind::ParamValue => PluginOut::Param {
                id: e.param_id,
                value: e.value,
            },
            RtOutKind::GestureBegin => PluginOut::GestureBegin { id: e.param_id },
            RtOutKind::GestureEnd => PluginOut::GestureEnd { id: e.param_id },
        })
        .collect()
}
