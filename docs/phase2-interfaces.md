<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
# Phase 2 interfaces (Milestone A)

Owner: orchestrator. Teammates read this file and never edit it. To change
an interface, message the orchestrator. `crates/protocol` holds the shared
types; this file says which crate provides which functions to which other
crate. Section numbers refer to `SPEC.md`.

Dependency direction: `protocol` <- `plugin-host` <- `engine` <- `ui`;
`protocol` <- `script` <- `ui`; `protocol` <- `mcp` (separate binary).

## Ownership

| Crate | Owner | Provides |
|---|---|---|
| `crates/protocol` | orchestrator | types, limits, file format, layouts |
| `crates/plugin-host` | plugin-host teammate | CLAP scan, load, main-thread API, audio-thread API, GUI windows |
| `crates/engine` | engine teammate | audio stream, `Compiled`/`Runtime`, compiler, synth, mixer, metronome, transport, offline render, WAV writer |
| `crates/ui` | ui teammate | binary `libredaw`, GTK app, `Document`, `apply()`, undo, bundle save/load, autosave, slot allocation, compiler thread, control socket server |
| `crates/script` | script teammate | Deno bridge (10) |
| `crates/mcp` | script teammate | binary `libredaw-mcp` (16, 17.1) |

Root files (`Cargo.toml`, `deny.toml`, `tools/`, `SPEC.md`, `docs/`) belong
to the orchestrator. If you need a workspace change (new member, new
`deny.toml` exception), ask. You may add dependencies to your own crate's
`Cargo.toml`; each needs a one-line justification sent to the orchestrator
(who adds it to `tools/third-party.fish`) and must pass `cargo deny check`.

## plugin-host -> engine (audio thread)

Module `plugin_host::rt`. Every function is real-time safe on the host
side (no allocation, no locks, no logging); what the plugin does inside is
outside our control (3.3).

```rust
pub struct RtNote { pub frame: u32, pub key: u8, pub vel: u8, pub on: bool, pub note_id: u32 }
pub struct RtBlock<'a> {
    pub frames: u32,                        // <= protocol::consts::MAX_BLOCK
    pub steady_time: u64,                   // sample counter
    pub inputs: [&'a [f32]; 2],             // stereo in (effects); silence for instruments
    pub outputs: [&'a mut [f32]; 2],        // stereo out
    pub notes: &'a [RtNote],                // sorted by frame
    pub params: &'a [protocol::engine::PluginEvent], // host -> plugin values for this block
}
pub enum RtStatus { Ok, Error }
/// Output events (param changes, gestures) are written into this sink and
/// forwarded by the engine to the EventRing.
pub struct RtEventSink<'a> { /* fixed-capacity slice owned by the engine */ }

pub unsafe fn start_processing(h: PluginHandle) -> bool;
pub unsafe fn stop_processing(h: PluginHandle);
pub unsafe fn process(h: PluginHandle, block: &mut RtBlock, out: &mut RtEventSink) -> RtStatus;
```

Stereo only in Milestone A (plugins with other port layouts are rejected at
load with a clear error).

## plugin-host -> ui (GTK main thread)

Module `plugin_host::host`. All calls on the GTK main thread only.

```rust
pub struct PluginDesc { pub id: String, pub name: String, pub vendor: String,
                        pub version: String, pub path: PathBuf, pub instrument: bool, pub effect: bool }
pub fn scan() -> Vec<PluginDesc>;                       // standard CLAP paths only (CLAP_PATH, ~/.clap, /usr/lib/clap, /usr/lib64/clap)
pub struct Instance;                                    // owns the plugin; not Send
impl Instance {
    pub fn create(desc: &PluginDesc) -> Result<Instance, HostError>;
    pub fn activate(&mut self, sample_rate: f64, max_frames: u32) -> Result<(), HostError>;
    pub fn deactivate(&mut self);
    pub fn handle(&self) -> PluginHandle;               // for EngineCommand::AttachPlugin
    pub fn save_state(&mut self) -> Result<Vec<u8>, HostError>;
    pub fn load_state(&mut self, bytes: &[u8]) -> Result<(), HostError>;
    pub fn params(&mut self) -> Vec<ParamInfo>;
    pub fn param_value(&mut self, id: u32) -> Option<f64>;
    pub fn flush_params(&mut self, values: &[protocol::engine::PluginEvent]); // when not processing
    pub fn show_gui(&mut self, title: &str) -> Result<(), HostError>;      // 9.2
    pub fn hide_gui(&mut self);
    pub fn gui_open(&self) -> bool;
    pub fn poll_main_thread(&mut self);                 // call from the 10 ms source (4.4, 9.1)
}
impl Drop for Instance { /* destroy on the GTK thread; must be detached first */ }
```

`timer-support`, `posix-fd-support`, and the x11rb connection register
GLib sources themselves (plugin-host may depend on `glib`).

## engine -> ui

Module `engine` (crate `libredaw-engine`).

```rust
pub struct Slots { /* ChannelId -> ChannelSlot, TrackId -> TrackSlot, with generations */ }
pub fn compile(project: &protocol::model::Project, slots: &Slots, sample_rate: f64) -> Box<Compiled>;
pub struct EngineConfig { pub host: Host /* PipeWire, Alsa, Jack */, pub device: Option<String>,
                          pub buffer_frames: u32, pub sample_rate: Option<u32> }
pub struct Engine {             // GTK thread side
    pub controls: Arc<ControlTable>, pub params: Arc<ParamTable>, pub status: Arc<EngineStatus>,
    /* producers and consumers of the rings */
}
impl Engine {
    pub fn start(cfg: &EngineConfig, first: Box<Compiled>) -> Result<Engine, EngineError>;
    pub fn stop(self);                                   // 4.7 step 1
    pub fn sample_rate(&self) -> f64;
    pub fn submit(&mut self, c: Box<Compiled>) -> Result<(), Box<Compiled>>; // state ring; Err = full, retry
    pub fn command(&mut self, c: EngineCommand) -> Result<(), EngineCommand>;
    pub fn plugin_event(&mut self, e: PluginEvent) -> Result<(), PluginEvent>;
    pub fn drain_events(&mut self, f: impl FnMut(EngineEvent));
    pub fn devices(host: Host) -> Vec<String>;
}
pub struct RenderRequest { pub project: Arc<Project>, pub pattern: PatternId, pub loops: u32,
                           pub tail_seconds: f64, pub sample_rate: u32 }
/// Offline render on the calling (export) thread with its own Runtime.
/// Export plugin instances are created by ui and passed as handles (8).
pub fn render_offline(req: &RenderRequest, slots: &Slots, plugins: &[(PluginSlot, PluginHandle)],
                      progress: &AtomicU32, cancel: &AtomicBool) -> Result<Vec<[f32; 2]>, EngineError>;
pub fn write_wav(path: &Path, frames: &[[f32; 2]], rate: u32, fmt: protocol::control::WavFormat) -> io::Result<()>;
```

The disposal thread is internal to `engine`.

## ui -> script, ui -> mcp

The `ui` crate runs the control socket server (`$XDG_RUNTIME_DIR/libredaw/control.sock`,
17.1). Wire format: one `protocol::control::Request` per line in, one
`protocol::control::Reply` per line out, JSON. The first line from a
client is a hello: `{"hello": {"transport": "agent" | "script", "client": "<name>"}}`
(ui replies `{"hello_ok": {"protocol": 1}}`). A refused hello gets one line
`{"hello_err": {"reason": "agents_disabled" | "not_allowed" | "bad_hello" | "busy_owner"}}`
and the server closes the connection. `agents_disabled` means the user has not
enabled agent control for this session; clients show "enable agent control in
LibreDAW" rather than a protocol error. The `script` crate runs the
Deno child and translates its stdio JSON to these requests; `mcp` maps MCP
tools to these requests.

## Milestone A done means

A user can: launch `libredaw`, make a pattern with a synth channel in the
step grid and piano roll, change mixer volume/pan/mute/solo, play with the
metronome, load one CLAP plugin (Surge XT) as instrument or insert and open
its GUI, undo/redo everything, save, reopen, and export WAV. An agent can do
the same through `libredaw-mcp`. `tools/ci.fish` passes.
