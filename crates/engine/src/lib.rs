// SPDX-License-Identifier: GPL-3.0-or-later
//! Audio engine: transport, sequencer, synth, mixer, plugin slots, live
//! stream, offline render and WAV writer.

pub mod api;
pub mod bass808;
pub mod compiled;
pub mod fx;
pub mod groove;
pub mod live;
pub mod loadproject;
pub mod metronome;
pub mod mixer;
pub mod plugins;
pub mod preview;
pub mod recorder;
pub mod render;
pub mod report;
pub mod rt;
pub mod runtime;
pub mod sampler;
pub mod samples;
pub mod sequencer;
pub mod synth;
pub mod tables;
#[cfg(test)]
mod testutil;
pub mod transport;
pub mod wav;
pub mod wavread;

pub use api::{CallbackProbe, Engine, EngineConfig, EngineError, Host};
pub use compiled::{Compiled, Slots, SlotsFull, compile, compile_with};
pub use metronome::{CallbackState, Controls, Metronome};
pub use plugins::PluginApi;
pub use recorder::Recorder;
pub use render::{RenderRequest, Rendered, SongRequest, render_offline, render_song};
pub use runtime::{RtEnds, Runtime, Shared, UiEnds, rings};
pub use samples::{SampleData, SampleStore};
pub use tables::{
    write_bass808_params, write_controls, write_fx_params, write_sampler_params, write_synth_params,
};
pub use transport::{MAX_BLOCK, PPQ, Transport};
pub use wav::write_wav;
