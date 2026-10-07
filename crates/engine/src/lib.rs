// SPDX-License-Identifier: GPL-3.0-or-later
//! Audio engine: transport, sequencer, synth, mixer, plugin slots, live
//! stream, offline render and WAV writer.

pub mod api;
pub mod compiled;
pub mod groove;
pub mod live;
pub mod metronome;
pub mod mixer;
pub mod plugins;
pub mod preview;
pub mod recorder;
pub mod render;
pub mod report;
pub mod rt;
pub mod runtime;
pub mod sequencer;
pub mod synth;
pub mod tables;
pub mod transport;
pub mod wav;

pub use api::{Engine, EngineConfig, EngineError, Host};
pub use compiled::{Compiled, Slots, SlotsFull, compile};
pub use metronome::{CallbackState, Controls, Metronome};
pub use plugins::PluginApi;
pub use recorder::Recorder;
pub use render::{RenderRequest, render_offline};
pub use runtime::{RtEnds, Runtime, Shared, UiEnds, rings};
pub use tables::{write_controls, write_synth_params};
pub use transport::{MAX_BLOCK, PPQ, Transport};
pub use wav::write_wav;
