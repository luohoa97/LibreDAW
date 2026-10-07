<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
# LibreDAW Specification

Status: APPROVED (draft 2, post adversarial review), 2026-10-07.
Owner decisions are recorded at the end.

LibreDAW is a native Linux desktop DAW with a pattern-based workflow (step
sequencer, piano roll, mixer). Behavior is inspired by pattern-based DAWs.
No proprietary names, assets, icons, or UI layouts are copied.

---

## 0. Risks (read first)

1. **Plugin GUIs cannot be embedded under GTK4.** GTK4 has no GtkSocket and
   no foreign-window embedding. Most Linux CLAP plugins only support the X11
   GUI API. Plugin GUIs are separate X11 windows (through XWayland on Wayland
   sessions). On Wayland we cannot position them, make them transient for
   the main window, or coordinate focus and stacking (section 9.5). This is
   permanent for the GTK4 stack.
2. **The MVP plugin runs in-process, unsandboxed.** A crashing plugin takes
   down the DAW. A plugin that allocates or locks inside `process()` can
   cause xruns that our real-time rules cannot prevent (section 3.3).
   Mitigation in MVP: autosave that includes plugin state (section 7.6) and
   per-plugin timing measurements. Real isolation is the sandbox (post-MVP).
3. **Sandboxed plugins need one syscall on the audio thread.** Cross-process
   audio needs a wakeup. Section 9.4 needs at most one non-blocking
   `futex(FUTEX_WAKE)` per block per sandboxed plugin, and adds one block of
   latency. This is an exception to the "no syscalls" rule and needs approval.
4. **Sandboxed plugins may not get real-time scheduling.** The host must
   promote the runner's audio thread from outside the sandbox (section 9.4).
   If promotion fails, we fall back to two blocks of latency.
5. **Audio backend.** PipeWire is the primary backend, through cpal's
   native PipeWire host (cpal >= 0.18, `pipewire` feature). ALSA (through
   `pipewire-alsa`) and JACK (through `pipewire-jack`) remain as fallbacks
   and comparisons. Phase 1 measures all three and reports numbers before
   Phase 2. cpal's PipeWire host is new; if it is worse than JACK on the
   numbers, we say so.
6. **Licensing of the plugin boundary is a gray area.** Section 12.3 proposes
   a GPLv3 section 7 additional permission. It only works if every
   contributor grants it, so it must be decided before the first outside
   contribution. This spec is not legal advice.
7. **Scripting depends on an external `deno` binary.** LibreDAW never ships
   it (section 10). Users without `deno` on PATH get no scripting.
8. **Conflict between the MVP cap and the Phase 2 team plan.** Scripting and
   sandboxing are outside the MVP cap but have Phase 2 owners. Section 13.5.
9. **Scope grew (Amendment 2).** The goal is now making hard-hitting beats
   with zero prior skill: sampler, 808, drum kits, groove lanes, built-in
   effects, playlist, templates, start screen (section 13). This is several
   times the original MVP. It is staged in three milestones so something
   usable exists early. Sections 15.x are not yet adversarially reviewed.

---

## 1. Goals and non-goals

Goals:
- Pattern-based composition: step sequencer and piano roll editing the same
  per-channel note data.
- Hard real-time safe audio engine (for our own code; see 3.3 for plugins).
- Project files that are human-readable, versioned, and diff well in git.
- Fully GPLv3-compatible dependency tree; every asset has a recorded license.

Non-goals (MVP and possibly forever):
- Windows/macOS support.
- VST2/VST3/LV2/AU hosting (CLAP only; LV2 may be revisited post-MVP).
- Audio recording, MIDI input, sample editing, time stretching.
- Embedded plugin GUIs (risk 1).

---

## 2. Workspace layout

```
LibreDAW/
  Cargo.toml              workspace; every member has publish = false
  LICENSE                 GPL-3.0 full text
  deny.toml               cargo-deny policy (committed)
  THIRD_PARTY.md          every crate + license (generated) and system libraries
  ASSETS.md               every non-code asset + license + source
  SPEC.md
  tools/                  Fish scripts: SPDX check, THIRD_PARTY.md generation
  crates/
    protocol/             shared types; owned by the orchestrator only
    engine/               audio thread, mixer, transport, synth, sequencer playback, offline render
    plugin-host/          CLAP loading; later the sandbox runner binary
    ui/                   GTK4 app shell, widgets, document + undo; binary `libredaw`
    script/               Deno child-process bridge
    mcp/                  libredaw-mcp binary (MCP server for agents, section 16)
```

Dependency direction (no cycles):
`protocol` <- `plugin-host` <- `engine` <- `ui`; `protocol` <- `script` <- `ui`.

SPDX: every tracked file carries `SPDX-License-Identifier: GPL-3.0-or-later`
(comment syntax per file type: `//` for Rust, `#` for TOML/Fish, `<!-- -->`
for Markdown/XML/SVG). Files that cannot hold a comment (binary assets) are
listed with their license in `ASSETS.md`. `tools/check-spdx.fish` checks
every file from `git ls-files` except `LICENSE`, and CI fails on any miss.
(The REUSE tool is not used because it is written in Python.)

---

## 3. Threading model

### 3.1 Threads

| Thread | Priority | May block | May allocate | Owns |
|---|---|---|---|---|
| Audio (cpal callback) | RT (backend / rtkit) | never | never | `Runtime`, current `Box<Compiled>` |
| GTK main thread | normal | no long blocks | yes | `Document`, undo stack, plugin registry, all CLAP main-thread calls |
| Compiler thread | normal | yes | yes | builds `Compiled` from `Document` |
| Disposal thread | low | yes | frees | drops objects retired by the audio thread |
| Export thread | normal | yes | yes | offline render with its own `Runtime` and plugin instances |
| Supervisor thread (post-MVP) | normal | yes | yes | spawns and services all sandbox runners |
| Script I/O thread | normal | yes | yes | stdio pipe to the `deno` child |

### 3.2 Rules for our code on the audio thread

- It only touches its `Runtime`, its current `Compiled`, SPSC rings
  (wait-free), and atomics.
- No `Mutex`, `RwLock`, `Arc` drop, `Vec` growth, `Box` alloc or free,
  logging, or printing.
- Debug builds install our own `#[global_allocator]` wrapper (about 30 lines,
  no crate) that aborts when a thread-local "RT" flag is set. The audio
  callback and the export render loop set the flag. Plugin calls are not
  covered by it (Rust's allocator does not see C/C++ `malloc`).
- Floating point: `enter_rt_fp_mode()` sets flush-to-zero and
  denormals-are-zero (x86_64: MXCSR FTZ|DAZ; aarch64: FPCR.FZ). The audio
  callback calls it at the top of every callback. The export thread calls it
  once at render start and restores the previous mode after. Live and export
  output therefore match. Built-in DSP also adds a 1e-18 offset in feedback
  paths (filter, envelopes) as a portable backstop.
- The audio thread never waits on another thread. All cross-thread traffic
  is SPSC rings with one producer and one consumer, or atomics. So our code
  cannot cause priority inversion.
- `Instant::now()` (vDSO `clock_gettime`, not a syscall on x86_64 or aarch64
  Linux) is allowed for measurement.

### 3.3 What the rules do not cover

An in-process plugin's `process()` runs on our audio thread and may call
`malloc`, take locks, or log. We cannot prevent that. We measure it:
- Per plugin instance, the audio thread records `process()` duration into a
  preallocated histogram (atomics). The UI shows p99 and max per plugin and
  flags plugins whose max exceeds 50% of the block period.
- The Phase 2 validator has an optional `LD_PRELOAD` diagnostic shim
  (separate cdylib in `tools/`, never shipped) that counts `malloc`, `free`,
  `pthread_mutex_lock`, and `write` calls made on the audio thread while
  inside a plugin call, and reports them per plugin.

---

## 4. Engine / UI boundary

### 4.1 Compiled state versus runtime state

The engine state is split in two:

- `Compiled`: immutable, built off the audio thread from a `Document`.
  Contains sorted note arrays per pattern and channel, routing, instrument
  kinds, structural synth parameters, and a `slot_gen` per channel slot and
  mixer slot. No values that the user can change continuously.
- `Runtime`: mutable, allocated once at stream start, sized to fixed maxima,
  never reallocated while the stream runs. Contains synth voices, filter and
  envelope state, the active-note table, gain and pan smoothers, meters,
  transport anchor, and the plugin slot table.

Fixed maxima (MVP): `MAX_CHANNELS = 64`, `MAX_TRACKS = 32` (plus master),
`MAX_INSERTS = 8` per track, 16 voices per synth channel. Edits beyond these
are rejected by `apply()` with an error the UI shows.

Each channel and mixer track has a stable slot index for its lifetime. When
the audio thread installs a `Compiled` whose `slot_gen` for a slot differs
from the one in `Runtime`, it resets that slot's runtime state (bounded copy
of fixed-size arrays, no allocation) and sends note-offs for its active
notes first. Edits that do not change slot generations (for example toggling
a step) leave all voices, envelopes, and smoothers untouched, so sounding
notes continue.

### 4.2 Swap protocol

```
Document (GTK thread) -- revision r --> Compiler thread: compile -> Box<Compiled>
   | state ring (SPSC, capacity S = 2)
   v
Audio thread: at a sub-block boundary
   | retire ring (SPSC, capacity R = S + 2)
   v
Disposal thread: drops
```

- Audio thread, at a sub-block boundary: if the state ring is non-empty,
  check that the retire ring has at least `state_ring.len() + 1` free slots
  (`Producer::slots()`). If not, do nothing this sub-block (retry next).
  If yes, pop all pending states, keep the newest, push the current and every
  skipped state into the retire ring. A popped state is therefore never
  without a home, and nothing is freed on the audio thread.
- Invariant: `R >= S + 1`. Asserted at construction.
- Compiler thread coalesces: it keeps only the latest pending `Document`
  revision. It compiles when the state ring has space, and it never discards
  the latest revision. If the ring is full it waits (it is a normal thread)
  and then compiles the newest revision.

### 4.3 Control values (fast path)

(Amended by 17.1: native instrument parameters use a `ParamTable` of the
same kind.)

Continuously changing values do not live in `Compiled`:
volume, pan, mute, solo (per channel and track), metronome gain, tempo
(tempo as `f64` bits in an `AtomicU64`; `f32` moves the grid at 133.33 BPM).
They live in a `ControlTable`: a fixed array of `AtomicU32` (f32 bits),
indexed by slot, shared by the GTK thread (only writer) and the audio thread
(reader, once per sub-block).

- Every UI change writes the atomic immediately and records the value in
  `Document`.
- After any `Document` replacement (edit, undo, redo, load, script batch),
  the GTK thread writes every control value of the new document into the
  table. This is a few hundred stores and is cheap.
- So the table always holds the current document's values. A compiled state
  can never revert a fader, because it does not carry fader values.

Plugin parameter values are not in the table. They are CLAP events sent to
the plugin through a `PluginEventRing` (SPSC, fixed-size records) and the
plugin keeps its own values.

### 4.4 Engine to UI

- Playhead position, transport state, xrun count, meters, per-plugin timing
  histograms: atomics written by the audio thread.
  The UI reads them in widget tick callbacks (frame clock). These reads are
  cosmetic. When the window is hidden and the frame clock stops, nothing
  breaks.
- Discrete events (plugin-initiated parameter change, gesture begin/end,
  plugin restart request, acks): SPSC `EventRing`, fixed-size records.
  It is drained by a `glib::timeout_add_local` source every 10 ms on the GTK
  thread, not by the frame clock, so it keeps running while minimized.
- Overflow: if the `EventRing` is full the audio thread drops the event and
  increments an overflow counter. When the GTK thread sees the counter change,
  it re-reads all parameter values from each plugin (`params.get_value`, main
  thread) and records them in `Document` as one non-undoable sync.

### 4.5 Block processing

- The engine processes sub-blocks of at most `MAX_BLOCK = 256` frames.
  Callbacks of any size are split. Scratch buffers are sized at stream start.
- Swaps, control reads, and command handling happen only at sub-block
  boundaries. Note events are sample-accurate inside a sub-block.

### 4.6 Timebase and transport

- Musical time: ticks, `PPQ = 960`, `u64` on the audio thread. Document
  positions are `u32` (validated `<= 2^31`), about 300 hours at 120 BPM.
- The transport is an anchor: `(anchor_sample: u64, anchor_tick: i64,
  samples_per_tick: f64)`. The absolute sample of tick `T` is
  `anchor_sample + round((T - anchor_tick) * samples_per_tick)`.
  Positions are computed from the anchor, never accumulated block by block.
- Tempo change (detected when the tempo control value changes at a sub-block
  boundary): set the anchor to the current exact position (in f64 ticks,
  carried as `anchor_tick` plus a fractional tick part), then replace
  `samples_per_tick`.
- Loop wrap: the loop is a range of ticks. At wrap, `anchor_tick` moves back
  by the loop length and `anchor_sample` stays continuous. The fractional
  sample position carries across the wrap; it is never rounded per loop.
  Pending note-offs for notes that cross the loop end are sent at the end.
- Tempo is constant within a project except for live changes; there is no
  tempo automation in MVP.

### 4.7 Device, rate, and buffer changes

Any change of device, sample rate, or maximum buffer size (from the user,
or from the backend reporting a new rate):
1. GTK thread stops the cpal stream and waits for it to be dropped.
   (Dropping the stream joins the audio thread; the `Runtime` and `Compiled`
   held by the callback are then freed on the GTK thread, not on an audio
   thread.)
2. GTK thread deactivates all plugin instances.
3. Builds a new `Runtime` for the new rate, recompiles, reactivates plugins
   with the new rate and max frame count.
4. Starts a new stream.
Audio stops for the duration. That is acceptable for a device change.

---

## 5. Domain model

### 5.1 Entities (MVP)

- `Project`: tempo, time signature (numerator 1..16, denominator 4),
  channels, patterns, mixer, metronome settings.
- `Channel`: id, name, instrument (`Synth(SynthParams)` or
  `Clap(ClapInstanceRef)`), root key (step entry key), target mixer track.
- `Pattern`: id, name, length in steps (1..64), step length in ticks
  (default 240 = 1/16), and `notes: Vec<ChannelNotes>` sorted by channel id,
  where `ChannelNotes { channel: ChannelId, notes: Vec<Note> }`.
  (A `Vec` of entries, not a map with integer keys, so the TOML form is
  simple.)
- `Note`: id, start tick, length ticks (>= 1), key 0..127, velocity 1..127.
- `MixerTrack`: id, name, volume dB, pan -1..1, mute, solo, up to
  `MAX_INSERTS` insert slots (`ClapInstanceRef`), output = master.
  Master is track 0.
- `ClapInstanceRef`: plugin id, plugin version at save time, and
  `state: Option<Arc<[u8]>>` (the last captured state blob, section 7.5).

### 5.2 Step sequencer and piano roll share data

A note is a **step note** for a channel in a pattern if and only if:
`start % step_len == 0 && len == step_len && key == channel.root_key &&
start < pattern_len_steps * step_len`. Velocity is free.

- Toggling step `i` on adds a step note at `i * step_len`. Toggling off
  removes every note with `start == i * step_len` and `key == root_key`.
- If any note of that channel in that pattern is not a step note, the step
  row shows a "piano roll data" marker and is read-only for that channel in
  that pattern only.
- `SetRootKey` is one `Edit` that also rewrites the key of every step note
  of that channel in all patterns, so step rows stay editable.
- `SetStepLength` rewrites start and length of step notes in that pattern.
- `SetPatternLength` to a shorter length removes notes that start at or
  after the new end, as part of the same edit (undoable).
- Test: set steps, change root key, change step length, the row stays
  editable and the same steps are on.

### 5.3 IDs

All entity IDs are `u32`. The counter is not part of the undoable
`Project` (section 6). IDs are never reused within a project, including
after undo.

---

## 6. Undo / redo

Model: persistent snapshots with structural sharing, not inverse commands.

- `Document { project: Arc<Project>, next_id: u32, revision: u64 }`.
  `Project` is a tree of `Arc`s: `Vec<Arc<Pattern>>`, `Vec<Arc<Channel>>`,
  `Arc<Mixer>`.
- An edit is a pure function
  `fn apply(doc: &Document, edit: &Edit) -> Result<Document, EditError>`
  that clones only the touched path (`Arc::make_mut`). It validates ranges
  (ticks, lengths, keys, counts against fixed maxima), so every document it
  accepts can be saved.
- The undo stack stores `Arc<Project>` roots only. Undo and redo replace
  `project` and keep `next_id` (it only grows). So IDs are never reused.
- `Edit` is one `enum` covering every mutation. Scripts and UI produce the
  same `Edit` values.
- Gesture groups: continuous gestures (fader drag, note drag, CLAP
  `begin_gesture`/`end_gesture`) open a group on press and close it on
  release. Intermediate states replace the top entry.
- While a gesture group is open: Undo and Redo are disabled, and script
  edit batches are queued FIFO on the GTK thread. They are applied as
  separate groups after the gesture closes. A queued batch that fails
  validation returns `EditError` to the script.
- Plugin state in undo: `ClapInstanceRef.state` is an `Arc<[u8]>`, so
  snapshots share blobs. The GTK thread captures fresh state
  (`state.save`) before any edit that removes or replaces a plugin, so undo
  of a removal restores the instance with its patch. Plugin-internal
  changes that do not go through parameters are not undoable on their own.
- Limit: 200 entries or 256 MiB, whichever comes first. The size estimate
  counts note arrays and every distinct state blob once.
- Not undoable: transport state, view state (scroll, zoom, selection).
- The compiler reuses compiled per-pattern data when `Arc::ptr_eq` shows the
  pattern did not change.

---

## 7. Project format

### 7.1 Container

A project is a directory bundle `Name.ldaw/`:
```
Name.ldaw/
  project.toml
  plugin-state/
    <instance-id>-<generation>.bin    opaque CLAP state blobs, immutable
  .autosave/
```
Blob files are never overwritten. A new capture gets a new generation number.
`project.toml` records the exact blob file name for each instance.

### 7.2 Encoding

- Load: `toml` (>= 1.0) into a `toml::Table`, run migrations, then
  deserialize with `serde`.
- Save: a hand-written emitter in `protocol` (estimated 300 lines) produces
  one canonical text. The `toml` serializer is not used for saving, because
  it writes `Vec<struct>` as arrays of tables, which breaks the
  "one note per line" goal.
- Canonical form: keys in fixed order, collections sorted by id (notes by
  `(start, key, id)`), one note per line as an inline table:
  `{ id = 41, start = 960, len = 240, key = 60, vel = 100 }`.
- Floats: stored values are `f64` written with Rust's shortest round-trip
  formatting. `-0.0` becomes `0.0`. NaN and infinity are rejected by
  `apply()` so they never reach the file.
- Test: `emit(load(emit(p))) == emit(p)` byte for byte, on generated
  projects, and `toml` parses everything the emitter writes.

### 7.3 Versioning and migration

- First line: `format_version = 1`. Any change to the schema bumps it.
- Loading version N < current runs `v1_to_v2`, ... on the `toml::Table`,
  then deserializes.
- `format_version` > current: refused with "written by a newer LibreDAW".
- Unknown keys are rejected (`deny_unknown_fields`) after the version check.

### 7.4 Save (crash-safe for the whole bundle)

1. Write each new blob to `plugin-state/<id>-<gen>.bin.tmp`, `fsync`,
   rename to its final name. (New names; nothing old is touched.)
2. `fsync` the `plugin-state` directory.
3. Write `project.toml.tmp`, `fsync`, rename over `project.toml`,
   `fsync` the bundle directory.
4. Delete blobs that the new `project.toml` does not reference.
A crash at any point leaves either the complete old project or the
complete new one, plus possibly orphan blobs that step 4 of the next save
removes.

### 7.5 Plugin state capture

The GTK thread (the CLAP main thread) calls `state.save` on every live
instance:
(a) before an edit removes or replaces the instance,
(b) before a save,
(c) in the autosave timer, before handing the document to the writer.
Each capture with different bytes from the previous one gets a new
generation. A missing plugin on load becomes a placeholder that keeps its
blob and re-saves it unchanged.

### 7.6 Autosave (Amendment 10)

Goal: a crash, a SIGKILL from the OOM killer or systemd-oomd, or a power
loss loses at most a few seconds of work. SIGKILL cannot be caught, so the
only defence is having already written the work.

- Document autosave: 3 seconds after the last edit, and at least every 15
  seconds while edits keep coming, the GTK thread clones the `Document`
  (cheap `Arc` clone) and hands it to the autosave worker, which writes it
  to `Name.ldaw/.autosave/` with the 7.4 procedure. The text emitter is
  fast (a typical project is a few hundred KB of TOML); the write happens
  off the GTK thread. Never-saved projects autosave to
  `~/.local/share/libredaw/recovery/<id>.ldaw/`.
- Plugin state: captured (7.5 (c)) every 60 seconds if any plugin reported
  a change (`take_dirty`), and on every document autosave that follows a
  plugin parameter gesture. State capture is a main-thread call; Phase 2
  measures its cost with the test plugins.
- Catchable termination: SIGTERM, SIGHUP, and SIGINT are handled (through
  a GLib Unix signal source, not a raw handler) exactly like closing the
  window: save, then exit (11).
- Recovery on launch: if `.autosave/` (or a recovery bundle) is newer than
  the saved project, LibreDAW opens the autosaved version as the current
  document, keeps the saved one untouched, and shows a toast: "Recovered
  unsaved work from <time>. Undo to go back to the last save." Recovery is
  one undoable step.
- When the persisted history of 15.11 lands, every commit is written
  within 2 seconds, which tightens this further; autosave stays as the
  Milestone A mechanism.
- Test: the validator kills the app with SIGKILL during editing and checks
  that relaunch recovers every edit older than 15 seconds and the last
  edit older than 3 seconds.

---

## 8. Audio engine (MVP features)

- Transport: play, stop, loop current pattern, tempo 20..999 BPM.
- Metronome: synthesized click (short sine burst, accent on beat 1), no
  sample assets. Own gain, on/off, routed to master.
- Sequencer playback: per channel, sorted note arrays in `Compiled`, cursor
  found by binary search after a swap or seek. Pending note-offs live in the
  active-note table in `Runtime` (`MAX_CHANNELS x 128` keys), independent of
  pattern data, so deleting a sounding note never leaves it hanging. Stop
  and loop wrap send pending note-offs.
- Built-in synth: 16 voices per channel, oldest-voice stealing,
  2 oscillators (sine, saw, square, triangle; polyBLEP), state-variable
  lowpass filter, amp ADSR and filter ADSR. Voices live in `Runtime`.
- Mixer: per track volume (dB, smoothed per sample over 10 ms), pan (-3 dB
  constant-power law), mute, solo (solo-in-place), insert slots, master.
  Peak meters per track.
- WAV export: renders `loops x pattern length` (+ optional tail) on the
  export thread with its own `Compiled` and `Runtime`. Output 16/24-bit PCM
  with TPDF dither, or 32-bit float. Hand-written WAV writer (about 100
  lines).
- Export and CLAP plugins: export never touches live plugin instances.
  The GTK thread captures each live instance's state, creates a second
  instance per plugin, loads the state, and activates it at the export
  sample rate and block size, with the `render` extension set to offline if
  offered. The export thread is that instance's only audio thread. After the
  render, or on cancel or error, the GTK thread destroys the export
  instances. Live playback is unaffected. If a plugin fails to create a
  second instance or to load its state, export stops with an error that
  names the plugin.

---

## 9. Plugin hosting

### 9.1 MVP: in-process CLAP

- Load with `libloading`, raw bindings from `clap-sys`.
- Thread contract: every CLAP `[main-thread]` call happens on the GTK main
  thread. Host callbacks that a plugin may call from any thread
  (`request_callback`, `request_restart`, `request_process`) only set an
  atomic flag. The 10 ms GLib timeout source (4.4) checks the flags and
  calls `on_main_thread` or restarts the plugin.
- `thread-check`: main thread = GTK thread. Audio thread = the live
  callback thread for live instances, the export thread for export
  instances. Each instance has exactly one audio thread for its lifetime.
- Ownership: live instances live in a plugin registry owned by the GTK
  thread. The audio thread sees a plugin only through a slot in
  `Runtime`'s plugin table, filled and cleared by commands on a command
  ring (`AttachPlugin { slot, handle }`, `DetachPlugin { slot }`).
- Lifecycle: create, init, activate on the GTK thread, then `AttachPlugin`.
  The audio thread calls `start_processing` on first use. To remove: GTK
  thread sends `DetachPlugin`; audio thread calls `stop_processing`, clears
  the slot, and acks through the `EventRing`; on the ack the GTK thread
  calls `deactivate` and `destroy`. The GTK thread never blocks waiting for
  the ack.
- Parameters: `params` extension. Host-to-plugin changes go through the
  `PluginEventRing` into the plugin's input event list. Plugin-originated
  changes come back through its output event list into the `EventRing`.
  When the plugin is not processing, `params.flush` is called from the GTK
  thread.
- Host extensions provided in MVP: `audio-ports`, `note-ports`, `params`,
  `state`, `gui`, `latency` (reported, not compensated), `thread-check`,
  `render`, `log`, `timer-support`, `posix-fd-support`.
  - `timer-support`: each `register_timer` becomes a
    `glib::timeout_add_local` source that calls `on_timer` on the GTK thread.
  - `posix-fd-support`: each `register_fd` becomes a
    `glib::unix_fd_add_local` source that calls `on_fd` on the GTK thread.
  - All sources of an instance are removed before `destroy`.
  - `log` from the audio thread writes to a preallocated ring; the GTK
    thread prints it.
- Test plugins (not bundled): Surge XT (GPL-3.0-or-later) and Airwindows
  Consolidated (MIT). The Phase 2 acceptance test opens Surge XT's GUI and
  checks that it repaints and that a GUI knob change reaches `Document`.

### 9.2 Plugin GUI windows

- Preferred: if the plugin supports floating windows
  (`is_api_supported(X11, true)`), use that mode.
- Otherwise LibreDAW creates an X11 top-level window with `x11rb` and passes
  it to `set_parent`. The `x11rb` connection's fd is added to the GTK main
  context with `glib::unix_fd_add_local` and drained on the GTK thread.
  The host handles `WM_DELETE_WINDOW` (calls `gui.hide`, then
  `gui.destroy`) and `ConfigureNotify` (calls `adjust_size` and `set_size`
  if `can_resize`).
- `clap_host_gui` callbacks are implemented: `request_resize` (resize the
  X11 window and call `set_size`), `request_show`, `request_hide`,
  `resize_hints_changed`, and `closed` (updates the "GUI open" toggle).
  Because these may be called from other threads, they set flags handled by
  the 10 ms source.
- Scale: under a Wayland session, call `set_scale(1.0)` by default (the
  compositor scales XWayland windows). Under an X11 session, use
  `Xft.dpi / 96`. A per-plugin override is available in settings. This
  default must be verified at 1.5x scale on GNOME and KDE in Phase 2.

### 9.3 Sandboxed plugins (post-MVP)

- Each plugin instance runs in `libredaw-plugin-runner` (our binary, from
  the `plugin-host` crate) under `bwrap`:
  `--unshare-all --die-with-parent --new-session`, read-only bind `/usr`
  and the plugin bundle, tmpfs `$HOME`, a writable bind of a per-plugin
  state dir, no network, X11 socket only if the GUI is opened.
- All runners are spawned from the one long-lived supervisor thread.
  `--die-with-parent` uses `PR_SET_PDEATHSIG`, which fires when the
  spawning thread exits; a long-lived spawner prevents spurious kills.
- The host keeps a `pidfd` per runner and polls it on the supervisor thread
  in addition to the control socket.
- Known hole: X11 access lets a plugin read input and the screen.
  Documented, not fixed.

### 9.4 Sandbox IPC

Control channel:
- Unix `SOCK_SEQPACKET` socket pair, serviced only by the supervisor thread
  with non-blocking I/O.
- Each request has a timeout (default 2 s, 10 s for state load). A timeout
  kills the runner.
- The host sends exactly one fd (the memfd) at handshake. It never accepts
  fds from the runner: any received control message with ancillary data or
  `MSG_CTRUNC` is a protocol violation, and the runner is killed.
- Limits enforced before allocation: control message 1 MiB, state blob
  64 MiB. A new state blob replaces the stored one only after it passes the
  size check and a full read; otherwise the previous good state is kept.

Shared memory:
- One `memfd` per instance, sized, then sealed with
  `F_SEAL_SHRINK | F_SEAL_GROW | F_SEAL_SEAL`.
- Layout:
```
Header  { magic: u64, version: u32, max_block: u32, in_ch: u32, out_ch: u32,
          sample_rate: f64 }
Sync    { host_seq: AtomicU64, plugin_seq: AtomicU64,
          plugin_sleeping: AtomicU32 }     each on its own 64-byte line
Slot[2] { frames: u32, in: [f32; in_ch*max_block], out: [f32; out_ch*max_block],
          events_in: EventRing, events_out: EventRing }
```
- The runner is untrusted. The host copies `Header` into private memory at
  map time and never reads it again. Every value the host reads from shared
  memory is validated before use:
  - `plugin_seq` is only accepted if `<= host_seq`.
  - `events_out` consumer indices are host-private. The runner's write index
    is read once per block, masked with `capacity - 1`, and the event count
    is clamped to the capacity.
  - Event records are checked: parameter ids in range, values finite.
  - The plugin's audio output is summed into the track bus and the bus is
    sanitized once per sub-block: non-finite becomes 0, values clamp to
    +/-16.0.
  - Any violation marks the instance dead (silence) and the supervisor
    kills the runner.

Audio sync (needs approval: syscall exception):
- Counters are `u64`, so wraparound cannot happen in practice.
- Block `N`: the host may write slot `N % 2` only if `plugin_seq >= N - 2`
  (the runner finished with that slot). If not, the host does not touch the
  slot, outputs silence for this plugin, and counts an overrun.
- Otherwise the host writes inputs into slot `N % 2`, then
  `host_seq.store(N, Release)`, then `fence(SeqCst)`, then reads
  `plugin_sleeping` (Relaxed) and calls `futex_wake` on `host_seq` only if
  it is 1. It reads block `N-1`'s output from the other slot if
  `plugin_seq >= N - 1` (Acquire), else silence and an overrun.
- Runner: processes slots in order and never skips. To sleep it does
  `plugin_sleeping.store(1)`, `fence(SeqCst)`, re-checks `host_seq`; if
  it changed, clears the flag and continues, else `futex_wait` with a
  timeout of one block period.
- Both sides use non-private futex operations (`FUTEX_WAIT`/`FUTEX_WAKE`
  without `FUTEX_PRIVATE_FLAG`), because the mapping is shared between
  processes.
- Latency: one block per sandboxed plugin, shown to the user. No plugin
  delay compensation in MVP.
- Watchdog: after 64 consecutive overruns with no `plugin_seq` progress the
  audio thread marks the instance dead (an atomic). The supervisor sees it,
  kills the runner process group through the `pidfd`, and the UI offers a
  restart from the last captured state.
- Real-time priority: the runner cannot reach rtkit from inside the
  sandbox. The host obtains the runner's host-namespace pid from the spawn
  (`pidfd`), finds the audio thread's tid in `/proc/<pid>/task` (the runner
  reports which thread by its namespace tid and thread name), and asks rtkit
  (`MakeThreadRealtimeWithPID`) or calls `sched_setscheduler` from outside.
  If promotion fails, the instance uses a pipeline depth of 2 blocks
  (3 slots) and the UI reports the extra latency.

### 9.5 GUI limits under Wayland

- `gui.set_transient` needs an X11 handle for the host window. A native
  Wayland GTK4 window has none, so plugin windows have no parent relation
  to the main window. Clicking the main window can hide a plugin window
  behind it.
- Each insert slot has a "show plugin window" toggle. Activating it while
  the window is open re-raises it through the `x11rb` connection
  (`_NET_ACTIVE_WINDOW`). `gui.suggest_title` is called with the track and
  plugin name so it is findable in the shell's window list.
- Keyboard shortcuts are not forwarded between the plugin window and the
  main window. This is documented in the user-facing limits.

---

## 10. Scripting (Deno, control rate only)

- Runtime: the `deno` CLI as a child process, found on `PATH`, minimum
  version pinned in the code. LibreDAW never ships or downloads `deno`.
  Launched with no permissions except read of the script file: `deno run
  --no-prompt --deny-net --deny-env --deny-run --deny-write --deny-sys
  --deny-ffi --deny-import --allow-read=<script> --no-remote --no-npm
  --no-config --no-lock` plus an import map for `libredaw` (deno 2.x has no
  `--deny-all`). Protocol: newline-delimited JSON over stdin/stdout
  (`serde_json`).
- Why not embedded `deno_core`: no prebuilt V8 static library and hundreds
  of crates in our build; a hung or crashed script cannot freeze the UI;
  Deno's permission model sandboxes it. Cost: requires `deno` installed;
  calls have about 0.1 to 1 ms IPC latency, fine for control rate.
- No DSP in JS, ever. Scripts never see audio buffers. The fastest event a
  script can receive is a transport or beat event, at most every 10 ms,
  delivered late (not sample-accurate).
- API surface (TypeScript, version `1`):
  - `project.get()`: read-only JSON snapshot of the document.
  - `edit(fn)`: batch of `Edit` values applied as one undo group (queued if
    a gesture is open, section 6). Ops: `addNote`, `removeNotes`,
    `setStep`, `setVolume`, `setPan`, `setMute`, `setSolo`, `setTempo`.
  - `transport.play()`, `transport.stop()`, `transport.state()`.
  - `on("beat" | "patternLoop" | "transport", handler)`.
  - `ui.selection()`: currently selected note ids.
- Every request carries an id; replies are matched by id. A script that
  does not answer a request within 2 s is killed.

---

## 11. UI (GTK4 + libadwaita)

- App shell: `AdwApplicationWindow` with header bar transport controls,
  a channel list with step rows, a piano roll editor, and a mixer panel.
  The layout is our own.
- Custom widgets (GtkWidget subclasses drawing with `snapshot()`): step
  grid, piano roll, meters. Only visible ranges are drawn.
- Keyboard path: step grid and piano roll are focusable. Arrow keys move a
  cursor cell, Space toggles a step or adds or removes a note, Shift+arrows
  resize, Ctrl+arrows move the selected notes. Every mouse edit has a
  keyboard equivalent.
- Accessibility: each custom widget sets an accessible role (`grid` for the
  step grid, `generic` with a label for the piano roll) and updates the
  accessible label and value of the cursor cell.
- GObject subclassing (`ObjectSubclass`) is the toolkit's mechanism and is
  exempt from the "no trait hierarchy" rule. Our logic stays in plain
  structs and functions called from thin widget wrappers.
- Icons: system Adwaita icon theme (not bundled). Custom icons we draw are
  licensed GPL-3.0-or-later, carry an SPDX comment in the SVG, and are
  listed in `ASSETS.md`.
- Fonts: system fonts only. Nothing bundled.
- Close saves (Amendment 9). Closing the window, quitting, or a session
  logout saves the project (7.4, plugin state captured first, 7.5) and then
  exits. There is no "save changes?" dialog. A project that was never
  saved is saved as `~/Music/LibreDAW/Untitled <n>.ldaw` (XDG music dir,
  `n` the next free number) and a toast on next launch says where. If the
  save fails (disk full, permission), the window stays open and shows the
  error; it never exits with unsaved work. Undo history persists, so
  "I did not want those changes" is answered by Undo or History, not by a
  discard prompt.
- Fast restart (Amendment 9). The app saves a small view state file
  (`.view.toml` in the bundle, not part of the project format: open
  project, selected pattern and channel, open panels, scroll, zoom) on
  close. On launch it reopens the last project with that view. A
  developer loop `tools/dev.fish` runs `cargo watch`, and on each rebuild
  asks the running app to close (which saves) and starts the new build, so
  a code change is visible in seconds at the same place in the project.
  No hot code swapping.

---

## 12. Licensing and dependency policy

### 12.1 Rules

- Project: GPL-3.0-or-later. SPDX identifier in every tracked file
  (section 2).
- Workspace crates set `publish = false`; `deny.toml` sets
  `[licenses.private] ignore = true`, so our own GPL crates are not checked
  against the allow list. GPL is not on the allow list, so any third-party
  GPL crate needs a named, reviewed exception.
- Allow list (SPDX ids as cargo-deny matches them literally): MIT,
  Apache-2.0, Apache-2.0 WITH LLVM-exception, BSD-1-Clause, BSD-2-Clause,
  BSD-3-Clause, ISC, Zlib, MPL-2.0, LGPL-2.1-or-later, LGPL-3.0-only,
  LGPL-3.0-or-later, Unicode-3.0, CC0-1.0. New ids are added only when
  `cargo deny list` shows a real dependency needs them, after review.
- CI runs `cargo deny check licenses bans sources advisories`.
- `THIRD_PARTY.md` is generated by `tools/third-party.fish` from
  `cargo metadata`; CI fails if the committed file differs. It also has a
  hand-maintained table of system libraries we link dynamically (GTK4,
  libadwaita, GLib, libpipewire, libasound, libjack). LGPL libraries are only ever linked
  dynamically and never bundled.
- Every new direct crate needs a one-line justification in `THIRD_PARTY.md`.

### 12.2 Direct dependencies

| Crate | License | Why |
|---|---|---|
| cpal | Apache-2.0 | audio I/O (fixed stack) |
| pipewire (via cpal feature) | MIT (libpipewire MIT) | native PipeWire backend (primary) |
| jack (via cpal feature) | MIT (libjack LGPL) | JACK fallback via pipewire-jack |
| rtrb | MIT OR Apache-2.0 | wait-free SPSC ring buffers with `peek` and `slots` |
| gtk4 | MIT | UI toolkit bindings (GTK LGPL-2.1-or-later) |
| libadwaita | MIT | app shell widgets (libadwaita LGPL-2.1-or-later) |
| clap-sys | MIT OR Apache-2.0 | raw CLAP ABI bindings |
| libloading | ISC | dlopen plugins |
| x11rb | MIT OR Apache-2.0 | plugin GUI parent window and raise |
| serde | MIT OR Apache-2.0 | deserializing the project file |
| toml (>= 1.0) | MIT OR Apache-2.0 | parsing the project file |
| serde_json | MIT OR Apache-2.0 | script bridge protocol |
| rustix (post-MVP) | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT | memfd, futex, pidfd, SCM_RIGHTS |

Removed: `assert_no_alloc` (replaced by our own allocator wrapper, 3.2).
Licenses above are as declared on crates.io and must be confirmed by
`cargo deny` in Phase 1.

External programs (not linked, not shipped): `deno` (MIT; its own binary
bundles V8, ICU, and many crates under their own licenses, which are its
packager's concern since we never ship it), `bwrap` (LGPL-2.0-or-later).

### 12.3 Plugin licensing boundary

- LibreDAW distributes no third-party plugins. Loading a plugin on your own
  machine is private use, which GPLv3 does not restrict.
- In-process (MVP) and sandboxed (post-MVP) hosting both put the plugin in
  a process running our GPL code (the main binary or
  `libredaw-plugin-runner`). The sandbox isolates the main DAW, not the
  runner. So the licensing question is the same for both.
- Proposal (owner decision): a GPLv3 section 7 additional permission in
  `LICENSE-EXCEPTION.md`, referenced from `LICENSE` and the README:

  > Additional permission under GNU GPL version 3 section 7: If you modify
  > this Program, or any covered work, by linking or combining it with a
  > plugin that is loaded only through the CLAP plugin ABI (the `clap_entry`
  > symbol and the interfaces reachable from it), either in the LibreDAW
  > process or in the libredaw-plugin-runner process, the licensors of this
  > Program grant you additional permission to convey the resulting work.
  > This permission does not cover any other code linked into the Program.

- It only binds code whose authors granted it. So from the first outside
  contribution, `CONTRIBUTING.md` requires a DCO sign-off that states the
  contribution is licensed GPL-3.0-or-later with this permission.
- Third-party GPL crates (if any are ever approved) do not carry this
  permission and would undo it for the process they are linked into. This
  is a second reason GPL crates need a reviewed exception in `deny.toml`.
- The SPDX header stays `GPL-3.0-or-later` as required. The permission is
  documented in the license files, not in each header.
- The CLAP headers are MIT; `clap-sys` is MIT/Apache. No conflict.

---

## 13. Scope

Amendment 2 (owner, 2026-10-07) replaced the original 8-item MVP cap with
the goal "make hard-hitting beats (phonk, trap, and new compositions) with
zero prior skill". The original cap could not do that: it had no sampler,
no drums, no swing, no arrangement, and no built-in effects. The new scope
is delivered in three milestones. Each milestone must be usable and
validated before the next starts.

### 13.1 Milestone A: core engine (the original MVP)

1. Transport (play/stop/loop pattern, tempo).
2. Metronome.
3. Step sequencer.
4. Basic piano roll (add, delete, move, resize notes; velocity; snap).
5. One built-in synth.
6. Mixer: volume, pan, mute, solo, master.
7. WAV export.
8. One CLAP plugin hosted in-process, unsandboxed (with its GUI).

Plus what these need: project save/load, undo/redo, autosave, audio device
selection, keyboard editing path.

### 13.2 Milestone B: beats (section 15.1 to 15.5)

1. Sampler channel (one-shot and pitched modes, choke groups).
2. 808 instrument: pitched sub with glide/slide and saturation.
3. Built-in drum kit generated by our own code (no third-party samples
   required), plus user sample import (WAV only).
4. Step sequencer lanes: per-step velocity, pitch, and ratchet (note
   repeat, for hi-hat rolls); per-pattern swing.
5. Built-in effects: EQ, compressor (with sidechain input), saturator /
   distortion, reverb, delay, limiter on master.
6. Mixer sends to effect return tracks.
7. Playlist (moved from C by Amendment 11): pattern clips placed on tracks
   along a timeline; song mode transport; full-song export (15.6).
8. Note preview: clicking a step, a piano-roll key, or a channel name plays
   the sound (`EngineCommand::Preview`, Amendment 11).
9. GNOME HIG compliant layout for every view (Amendment 11; the layout
   document is `docs/ui-design.md`).

### 13.3 Milestone C: songs and zero-skill UX (section 15.6 to 15.8)

1. Audio clips on the playlist (moved in by Amendment 11): samples placed
   on the timeline with waveform display. Automation stays after C.
2. Start screen: recent projects, "new from template", demo projects.
3. Templates and presets: genre templates (phonk, trap, boom bap, lo-fi,
   house), synth and 808 presets, drum kits.
4. Drag and drop from a sound browser onto channels and steps.

### 13.3a Milestone D: instrument suite (section 15.9 to 15.10)

Hybrid: libre CLAP instruments plus native drum pad, loop slicer, acid
bass, and kick synth, all behind one preset browser and macro layer.

### 13.4 Not in scope (cut, stated plainly)

- Copies of a commercial DAW's instruments. Milestone D covers every
  instrument family with libre or native equivalents (15.9), under our own
  names, not clones. Sampled acoustic instruments need recorded sample sets
  with compatible licenses and a license review each.
- Parity with commercial DAWs. They have more than 20 years of features.
  We target the beat-making workflow, not feature parity.
- Audio recording, MIDI input, automation lanes, tempo automation, time
  stretching, plugin delay compensation, LV2/VST, plugin GUI embedding.
- Sandboxing and scripting until Milestone A passes validation (13.5).

### 13.5 Phase 2 team plan (owner decision, approved)

Phase 2 builds Milestone A first. The `script` teammate builds only the
bridge plus `project.get` and `edit`. The `plugin-host` teammate starts
sandboxing only after in-process hosting passes validation. Milestones B
and C are planned after Milestone A is approved, with their own review.

---

## 14. Phase 1 acceptance (metronome)

- Workspace, LICENSE, deny.toml (with `[licenses.private] ignore = true`),
  THIRD_PARTY.md, SPDX headers, `tools/check-spdx.fish`, CI script, and
  `cargo deny check` passing.
- cpal metronome, no UI, 120 BPM, 4/4.
- Measurements reported as numbers, per backend (PipeWire native: required;
  ALSA and JACK: comparison), per buffer
  size (64, 128, 256, 512), 120-second runs (Amendment 4):
  - xrun count (backend-reported, plus callback gaps > 1.5x period)
  - callback interval jitter: mean, p99, p99.9, max (us)
  - click onset drift against the ideal grid after 10 minutes (samples)
- Offline tests (deterministic, run in CI):
  - 1 hour at 44.1 kHz and 48 kHz at 60, 120, 133.33, 999 BPM: every click
    onset equals the closed-form ideal sample index, rounded. Zero drift.
  - Loop wrap: a 1-bar loop at 133.33 BPM, 44.1 kHz, for 10 minutes; every
    onset matches the closed-form grid.
  - Tempo change mid-run: onsets before and after match the piecewise
    closed-form grid.
- Debug build with the RT allocator wrapper: zero allocations in the
  callback over the 10-minute run.

---

## 15. Beat-making features (Milestones B, C, and D)

(Amended by 17.2 to 17.5.)

Status: approved scope, design not yet adversarially reviewed. These
sections get the same review process as sections 3 to 12 before
Milestone B starts.

### 15.1 Sampler

- Channel instrument `Sampler(SamplerParams)`. Sample data is loaded and
  decoded off the audio thread into an immutable `Arc<[f32]>` held by the
  `Compiled` state and retired through the normal retire ring.
- Modes: one-shot (plays to the end, ignores note-off) and pitched
  (key-tracked from a root key, linear or cubic interpolation).
- Per channel: start/end trim, amp ADSR, pitch in semitones and cents,
  reverse, choke group (a note on a channel in group N stops voices of other
  channels in group N; used for open and closed hats).
- 16 voices per channel, preallocated in `Runtime`.
- Import: WAV (8/16/24/32-bit int and 32-bit float), decoded by our own
  reader (same code family as the WAV writer). Other formats are out of
  scope. Imported files are copied into `Name.ldaw/samples/` and referenced
  by relative path, so a project is self-contained.

### 15.2 808 instrument

- Built-in instrument `Bass808(Bass808Params)`, not a sample: a sine
  oscillator with a pitch envelope (click and drop), amp envelope with long
  decay, and a saturation stage (soft clip and drive, with tone filter).
- Monophonic with glide: a new note while one is sounding slides pitch
  over a set glide time (the "808 slide"). Polyphonic mode is available.
- Presets: clean sub, distorted (phonk), long decay, short punch.

### 15.3 Sound content (separate repository)

- Sounds live outside this repository, in a separate content repository
  `libredaw-sounds`, under its own license: CC0-1.0 by default. The DAW
  never links or embeds it. It finds sound packs at runtime in
  `$XDG_DATA_DIRS/libredaw/sounds/` and `~/.local/share/libredaw/sounds/`.
- Content in `libredaw-sounds` must be ours or under a license that allows
  redistribution (CC0-1.0, CC-BY-4.0 with attribution). Each file has its
  license and source recorded in that repository's manifest. Sounds
  extracted from commercial products are never accepted, whatever
  repository they are in: a separate repository does not change copyright.
- The drum sounds are synthesized by a Rust generator in `libredaw-sounds`
  (`kitgen`), so every sound is original.
- Kit pieces: kick (several), snare, clap, rim, closed hat, open hat,
  cowbell, toms, crash, ride, shaker, snap, perc.
- Kits: phonk, trap, boom bap, lo-fi, house. Each is a set of samples plus
  default channel settings (a small TOML manifest per kit).
- User libraries: the sound browser can add any local folder of WAV files
  the user owns (bought sample packs, or the sample folders of another DAW
  the user has installed, including installs inside a Wine prefix). The
  browser offers to scan common install locations. LibreDAW only reads
  these files on the user's machine; it never uploads, mirrors, or bundles
  them in any LibreDAW repository or package. Whether a vendor's license
  allows its samples to be used outside its own product is the user's
  responsibility; the browser says so when adding a folder.
- Only plain audio files (WAV) can be imported. Another DAW's built-in
  instruments are code (plugins), not files, and their presets use
  proprietary formats; they cannot be imported.
- Project bundles copy used samples in (15.1). Samples from user libraries
  are marked `local_only` in `project.toml`. "Export project for sharing"
  leaves them out by default and lists them, so sharing a project does not
  redistribute them by accident.
- If no sound pack is installed, the DAW still runs: the synth and 808 are
  built in.

### 15.4 Step sequencer lanes and groove

- Per-step lanes under each step row: velocity, pitch offset (semitones),
  ratchet (1, 2, 3, 4, 6, 8 repeats in the step: hi-hat rolls).
  Probability lanes are cut.
- These are stored on the step note (velocity and key already exist;
  ratchet is a new `Note` field `repeat: u8`, default 1). Changing them
  keeps the note a step note (5.2 predicate is extended: key may differ
  from root key by the pitch lane offset). Exact predicate change is part
  of the 15.x review.
- Swing per pattern, 0 to 75 percent, delays every second step. Applied at
  compile time to tick positions, so playback, export, and the grid agree.

### 15.5 Built-in effects and routing

- Effects are built-in insert types, the same slot type as CLAP inserts:
  `Insert::Builtin(BuiltinFx)` or `Insert::Clap(ClapInstanceRef)`. An enum,
  not a trait.
- `BuiltinFx`: 4-band EQ, compressor (with sidechain input from another
  track, for 808 and kick ducking), saturator / distortion, reverb
  (algorithmic, Freeverb-class, our own code), delay (tempo-synced),
  limiter (default on the master).
- Sends: each track has up to 4 sends to return tracks, with pre/post
  fader switch. The routing graph must stay acyclic; `apply()` rejects
  edits that create a cycle.
- All effect state is preallocated in `Runtime`. Reverb and delay buffers
  are sized for the maximum sample rate at stream start.

### 15.6 Playlist (song arrangement)

- `Project` gains `playlist: Vec<PlaylistTrack>`; each track holds clips
  `{ id, pattern: PatternId, start_tick, length_ticks }`.
- Transport modes: pattern mode (loop current pattern, as in Milestone A)
  and song mode (play the playlist, loop region optional).
- Export in song mode renders the whole playlist plus tail.
- This changes 4.6 (song position instead of pattern loop) and 5.1; those
  sections are updated during the 15.x review.

### 15.7 Start screen and projects

- On launch: a start screen with "New project" (from a template or empty),
  recent projects with name, date, and tempo, and demo projects.
- "New project" never asks technical questions. Audio device setup has a
  working default (PipeWire default sink) and lives in preferences.
- Projects are saved under `~/Music/LibreDAW/` by default
  (`XDG_MUSIC_DIR`).

### 15.8 Zero-skill UX rules

These are acceptance criteria, tested by the validator:
1. From launch to hearing a beat from a template: 2 clicks.
2. From an empty project to a 4-on-the-floor kick with hats: under 30
   seconds with the mouse only.
3. Every control has a tooltip; every built-in sound previews on click in
   the sound browser.
4. Drag a sound from the browser onto the channel list to make a channel.
5. Undo works for everything a user can click.
6. No modal dialogs during normal editing; errors appear as toasts
   (`AdwToast`).
7. Default sounds are mixed to sound good together at default settings
   (no clipping on the master with the default template).

The layout and look are our own. We do not copy another product's layout,
icons, names, or color scheme.


### 15.9 Instrument suite (Milestone D, hybrid)

Goal: an instrument for every synthesis family a commercial pattern DAW
ships, at comparable quality, all libre. Approach (owner decision): use
existing libre instruments that are already professional quality, and
build natively only where nothing good exists.

| Family | Source | Status |
|---|---|---|
| Subtractive, wavetable, FM | Surge XT (GPL-3.0-or-later, has CLAP) | candidate |
| Other libre synths (FM, additive, physical modeling, kick synth) | each one checked for license, CLAP on Linux, and quality before listing | to research |
| Sampler / multisample | native (15.1) | Milestone B |
| 808 | native (15.2) | Milestone B |
| Drum pad (16 pads, layers, velocity) | native, built on the sampler | Milestone D |
| Loop slicer (slice a loop to steps) | native, built on the sampler | Milestone D |
| Acid bass (303-style, slide and accent) | native | Milestone D |
| Kick synth | native or libre plugin, after research | Milestone D |
| Piano and other acoustic | libre sample sets (for example CC-BY) in `libredaw-sounds` | after license review |

- Third-party libre instruments are separate CLAP plugins, not linked into
  our binary. They are installed as a companion package (or from distro
  packages), never vendored into this repository.
- "Just as good" is judged by the owner by ear on a fixed set of reference
  presets per family, not claimed by us.

### 15.10 Presets: why ours are easier

An instrument is an engine. Beginners struggle with engines that each have
their own GUI and hundreds of controls. LibreDAW puts one consistent layer
on top of every instrument, native or plugin:

1. **Sound-first browser.** Presets are tagged by role and genre ("808",
   "phonk cowbell", "dark bell", "trap hat", "pad"), not by which synth
   makes them. Search and filter by role. The engine name is secondary.
2. **Audition in context.** Clicking a preset plays a short phrase in the
   project's key and tempo, through the channel's mixer track.
3. **Eight macro knobs.** Every preset maps up to eight plainly named knobs
   (for example Tone, Punch, Grit, Length, Space, Wobble, Width, Brightness)
   onto the engine's parameters, with safe ranges. They appear in our own
   UI, the same place for every instrument. The plugin's own GUI is behind
   an "Advanced" button.
4. **Level-matched.** Presets are normalized to a common loudness, so
   switching presets never jumps in volume or clips the master.
5. **Key and scale lock.** The project has a key. The piano roll and step
   pitch lanes highlight or snap to it.
6. **Preset chains.** A preset can carry its mixer effects (for example an
   808 with its saturator and EQ), applied when the preset is loaded.
7. **Variations.** "Similar sounds" and "vary" (random changes within the
   macro ranges) for fast exploration; always undoable.

A preset is a small TOML file: engine id, plugin state or native
parameters, macro mappings, tags, loudness. User libraries from other
installed DAWs (15.3) appear as local-only packs in the same browser.

Imported sounds get the same layer. A user library (15.3) is analyzed once
when added, on a worker thread, and the results cached in
`~/.cache/libredaw/`:
- loudness, for level matching;
- pitch and key for tonal one-shots (808s, bass, melodic samples), so they
  play in key;
- tempo and length for loops, so they sync to the project;
- role guess (kick, snare, hat, 808, loop, vocal, fx) from the file name,
  folder name, and simple audio features, editable by the user.
Imported samples then load into the sampler, whose macros (Pitch, Tone,
Punch, Length, Grit, Space) work the same as for built-in sounds. The
built-in kits are still needed so the DAW works out of the box for users
with no other libraries; they are not needed for quality.



### 15.11 History tree (persisted, git-like)

Undo (section 6) already stores immutable snapshots with shared structure,
so each edit costs only the nodes it changed. The whole tree is persisted
in the project, content-addressed like git:

- Objects: each node of the project tree (project root, pattern, channel,
  mixer, playlist) is written in the canonical text form (7.2) to
  `Name.ldaw/history/objects/<hash>.toml`, where `<hash>` is the hash of
  that text. Unchanged nodes are shared between commits by hash, on disk as
  in memory. Plugin state blobs and samples are already immutable files and
  are referenced by name.
- Commits: `history/commits/<hash>.toml` with parent commit, root object
  hash, time, author (`user`, `script`, or `agent:<name>`), and a short
  description generated from the edit ("move 4 notes in Pattern 2").
  Every undo group (gesture, script batch, MCP request) is one commit.
  Undo, redo, and branching move a `HEAD` pointer; nothing is discarded.
- Named versions: `history/refs/<slug>` points at a commit
  ("darker-808"). Beginners see Undo/Redo and a History panel of named
  versions; the full tree is one click away.
- Writing: commits are written by the save worker thread, append-only,
  batched (at most every 2 seconds and on save or close), with the same
  write-temp, fsync, rename procedure as 7.4. A crash loses at most the
  last 2 seconds of history, never the saved project.
- Memory: only recent snapshots stay in RAM (the section 6 limit now bounds
  the in-memory cache, not the history). Older states load from disk on
  demand.
- Garbage collection: 7.4 step 4 must keep every blob and sample that any
  commit references, not only the current `project.toml`. History is only
  pruned by an explicit "Compact history" command, which keeps named
  versions and a chosen time window.
- Git: the history directory is plain text plus immutable binaries. Users
  may commit it or add it to `.gitignore`; "Export project for sharing"
  leaves it out by default.
- Hash: needs one hash crate (`blake3`, CC0-1.0 OR Apache-2.0, to be
  confirmed by `cargo deny`). Not cryptographic security, only identity.
- Agents use the tree through MCP: `version_save`, `version_list`,
  `version_restore`, `history_tree`, `history_diff`. An agent can try
  several variations on branches and the user picks one.
- No merge between branches (merging music edits is not well defined) and
  no remote sync.

## 16. Agent control (LibreDAW MCP)

(Amended by 17.1: trust model, settings, concurrency, jobs, startup. Where
this section says `confirm: true`, read "PRIVILEGED, human approval".)

Status: approved scope (Amendment 6), not yet adversarially reviewed.

Goal: AI agents can operate the DAW: change settings, install packs, add
plugins, edit everything a user can edit, and make beats.

### 16.1 One control API

- `protocol` defines one `ControlRequest` / `ControlReply` enum pair. The
  Deno script bridge (section 10) and the MCP bridge both use it. A control
  request becomes `Edit` values (section 6) or a non-edit action (transport,
  save, export, scan).
- Every request that changes the document is one undo group, applied on
  the GTK thread through the same path as a UI edit (queued while a gesture
  is open, section 6). The user can undo anything an agent did.
- Control rate only. No audio passes through this API.

### 16.2 Transport between DAW and agent

- The running DAW listens on a Unix socket
  `$XDG_RUNTIME_DIR/libredaw/control.sock`, mode 0600, same user only, no
  network listener. Newline-delimited JSON (`serde_json`).
- `libredaw-mcp` is a separate small binary (stdio MCP server) that an agent
  host launches. It connects to the socket, or starts LibreDAW if it is not
  running. MCP protocol: the official Rust SDK `rmcp` if it passes
  `cargo deny`, else a hand-written JSON-RPC 2.0 layer over `serde_json`.
- The control socket is off by default. Preferences: "Allow agents to
  control LibreDAW". While an agent is connected, the header bar shows an
  indicator, and an "Agent activity" panel lists each request.

### 16.3 MCP tools (grouped)

- Projects: `project_list`, `project_new` (from template), `project_open`,
  `project_save`, `project_info`.
- Transport: `play`, `stop`, `set_tempo`, `set_key`, `set_loop`.
- Channels and sounds: `sound_search` (by role, genre, tags),
  `channel_add` (preset or instrument), `channel_remove`,
  `channel_set_macro`, `preset_load`.
- Patterns and notes: `pattern_new`, `steps_set` (row of steps with
  velocity, pitch, ratchet lanes), `notes_add`, `notes_remove`,
  `notes_list`, `swing_set`.
- Mixer: `track_set` (volume, pan, mute, solo), `fx_add`, `fx_set`,
  `send_set`, `sidechain_set`.
- Playlist: `clip_add`, `clip_move`, `clip_remove`.
- Packs: `pack_list`, `pack_install` (from the `libredaw-sounds` index
  only), `library_add_folder` (local folder, marked local-only, 15.3).
- Plugins: `plugin_scan`, `plugin_list`, `plugin_add` (only plugins found
  by the scan of standard CLAP paths, never an arbitrary file path),
  `plugin_param_set`.
- Settings: `settings_get`, `settings_set` (audio device, buffer size,
  theme, and so on; the setting that enables the control socket itself is
  not settable over the socket).
- Output: `export_wav` (pattern or song), returns the file path.
- Listening: an agent cannot hear. `analyze` renders offline and returns
  numbers: integrated loudness (LUFS), true peak, clipping count, per-track
  peak and RMS, low/mid/high energy balance, and kick-to-808 overlap. The
  agent uses these to judge and fix a mix.
- Undo: `undo`, `redo`, `history`.

### 16.4 Safety

- Destructive actions (delete a project, overwrite an existing file,
  remove a pack) require `confirm: true` and are shown in the activity
  panel. Projects are never deleted, only moved to the trash.
- Loading a plugin runs native code. Agents can only load plugins already
  installed in standard CLAP paths, and the first load of any plugin by an
  agent asks the user in the UI.
- No network access through the API except `pack_install`, which fetches
  only from the configured `libredaw-sounds` index URL.

### 16.5 Milestones and ownership

- The MCP surface grows with the milestones: Milestone A tools first
  (projects, transport, channels with the built-in synth, steps, notes,
  mixer, export, undo), then B, C, and D tools as those features land.
- Phase 2 ownership: the `script` teammate owns both the Deno bridge and a
  new crate `crates/mcp` (binary `libredaw-mcp`). The control socket server
  lives in `ui` (it runs on the GTK thread side). The team stays at 5
  sessions.
- Validator test: an agent session with only the MCP tools builds a
  4-bar beat (kick, snare or clap, hats with a roll, 808 line) from an empty
  project and exports it, with `analyze` reporting no clipping.

## 17. Review 2 resolutions (binding)

The adversarial review of sections 13, 15, and 16 (see changelog, Review 2)
produced the rules below. Where a rule conflicts with earlier text in
sections 4, 8, 15, or 16, this section wins. 17.1 applies to Milestone A;
17.2 to 17.5 apply when the named milestone starts.

### 17.1 Milestone A

- **Instrument parameters (rt-1).** Native instrument parameters (synth
  now; sampler, 808, and built-in effects later) live in a `ParamTable`
  next to the `ControlTable` (4.3): a fixed array of `AtomicU32` (f32
  bits) indexed by (channel slot, parameter index), `MAX_PARAMS_PER_SLOT =
  64`, written only by the GTK thread and rewritten from `Document` after
  every document replacement, read once per sub-block. `Compiled` holds
  only structure (instrument kind, waveform choices, voice count, sample
  references). Moving a knob never recompiles. CLAP parameter values: the
  `Document` keeps the last known value per (instance, param id) (the
  record from 4.4); undo replays the differences as `PluginEventRing`
  parameter events, never `state.load` while processing.
- **Active notes (rt-2).** The active-note table entry is keyed by
  (channel slot, key) and stores the id of the note that owns it. A
  note-off only releases the voice if its note id matches the owner, so an
  overlapping same-key note is never cut by an older note's note-off. On a
  same-key retrigger the old voice is released first. Offline test:
  overlapping same-key notes produce the closed-form gate lengths.
- **IDs across history (fmt-2).** `next_id` is written as a top-level key
  in `project.toml` and in every history commit. On open:
  `next_id = max(project.toml, all commit headers, max id seen + 1)`. On
  any restore: `next_id = max(current, max id in restored root + 1)`.
  Property test: random edits, save, reopen, restore, edit; all ids unique.
- **Control request handling (mcp-7, rt-8).** A control I/O thread (added
  to 3.1) reads requests with a 1 MiB line limit and at most 10,000 edits
  per request, parses them, and hands validated `Vec<Edit>` to the GTK
  thread over a GLib channel. Fixed maxima added to 4.1:
  `MAX_NOTES_PER_PATTERN = 100_000`, `MAX_NOTES_PER_PROJECT = 500_000`,
  checked in `apply()` before any clone. `export_wav` and `analyze` are
  jobs: the call returns `{job_id, revision}` at once, renders a pinned
  `Document` revision on the export thread (one job at a time, others
  queued), and supports `job_status`, `job_result`, `job_cancel`. Plugin
  instantiation for a job is sliced across GLib idle callbacks, one plugin
  per callback.
- **Trust model (mcp-1, ux-1).** The agent never supplies consent:
  `confirm` is removed. A closed set of PRIVILEGED requests needs a human
  click in the LibreDAW window: `plugin_add` (first load of a plugin by
  agents), `library_add_folder`, `pack_install`, `pack_remove`,
  `project_open` or `project_new` while the document has unsaved changes,
  any write outside the projects and exports folders, `version_restore`,
  and project deletion. The approval UI is a non-modal banner plus a row in
  the Agent activity panel. The request waits up to 60 s and then returns
  a typed `needs_user_approval` or `denied` error. Rule 15.8.6 now reads
  "no modal dialogs except these approvals". Each transport has a
  capability mask enforced in the DAW (the Deno bridge gets only the
  section 10 list). Agent control is enabled per session, not
  permanently.
- **Settings (mcp-2).** `settings_set` takes a closed Rust enum of keys:
  audio device (from the enumerated list), buffer size in {64, 128, 256,
  512, 1024}, theme, metronome. Anything that touches a path, URL, or
  executable is not settable through the control API.
- **Concurrency with the human (mcp-5).** Every request that reads or
  writes the document carries `base_revision`; if the touched pattern,
  channel, or track changed since, the reply is `Stale { current }`. A
  request queued behind an open gesture waits at most 10 s and then
  returns `Busy`. Every commit records its author (15.11); MCP `undo` and
  `redo` only move over that agent session's own commits and return an
  error otherwise.
- **Startup and socket (mcp-8).** If no DAW is running, `libredaw-mcp`
  starts it with `--agent-request`; the DAW shows a per-session "An agent
  wants to control LibreDAW" banner; `libredaw-mcp` retries for 30 s and
  then returns "waiting for the user in LibreDAW". The socket directory is
  created with mode 0700 and guarded by an `flock`ed lock file so only one
  DAW serves it; a stale socket is detected through the lock. Never
  `/tmp`. Untrusted text (names, tags from packs and presets) is returned
  only in structured fields, capped at 64 characters, control characters
  removed; prompt injection is contained by the PRIVILEGED rule, not by
  sanitizing. One MCP protocol revision is pinned in `crates/mcp`.
- **Dirty document (fmt-8).** `project_open` and `project_new` with
  unsaved changes are PRIVILEGED and autosave first.

### 17.2 Milestone B

- **Ratchets and 808 (rt-2, rt-3).** Ratchet sub-notes are generated by the
  compiler as ordinary notes at `start + floor(i * step_len / repeat)`,
  length `min(len, step_len / repeat)` minus 10 percent. `apply()` rejects
  `repeat` values that do not divide `step_len`. 808 mono mode: last-note
  priority; a new note while the gate is open is legato (no envelope
  retrigger, glide); a note-off for a non-current note is ignored.
  Offline tests for ratchet 8 and overlapping 808 notes.
- **Swing (rt-3).** Swing is a `Pattern` field stored as an integer in
  1/1000 units, applied by the compiler with integer rounding through one
  pure function that the grid UI also calls. Slider drags are coalesced
  by the compiler (4.2). Only step notes on odd steps are swung.
- **Step notes with pitch (fmt-3).** `Note` gets `off: i8` (-24..24, 0 =
  none) for step notes. A step note is `start % step_len == 0 && len ==
  step_len && key == root_key + off && repeat` divides `step_len`.
  Toggle-off removes step notes at that step regardless of `off`.
  `SetRootKey` rewrites `key = new_root + off`. Piano-roll move or resize
  of a step note clears `off` and makes it a piano-roll note.
- **Choke (rt-4).** The sub-block is split at choke trigger frames; choked
  voices fade out over 1.5 ms. Choke group ids are in `Compiled` and
  copied to `Runtime` at swap. Offline test: a closed hat at frame 100
  silences the open hat by frame 100 + fade.
- **Samples in memory (rt-5).** A GTK-owned `SampleStore` keyed by sample
  hash holds decoded `Arc<[f32]>`, filled by a dedicated loader thread
  (not the compiler), with a byte budget (default 1 GiB, shown in the UI).
  Samples are resampled once at load to the stream rate, and again on a
  4.7 rate change. A sample not yet decoded compiles as silence and the
  loader requests a recompile when done.
- **Samples on disk (fmt-4, license-1).** Bundle samples are
  content-addressed and immutable: `samples/<hash>.wav`, written with the
  7.4 procedure, never overwritten; `project.toml` stores
  `{hash, orig_name, size, local_only}`. Autosave and history reference the
  same files. `samples/` and `history/` are in the 7.4 GC mark set.
  `local_only` samples are never copied into the bundle: they are
  referenced by hash plus a per-machine path stored outside the bundle
  (`~/.local/share/libredaw/local-samples.toml`), and the bundle gets a
  generated `.gitignore`. A missing sample plays silence, shows a toast,
  and the project still loads and saves. Validator test: Save, history
  commits, and Export for sharing contain no byte of a local_only file.
  15.1 "self-contained project" excludes local_only samples.
- **Routing (rt-6).** One edge list in `Document` covers sends and
  sidechains; `apply()` rejects cycles over all edges plus the implicit
  track-to-master edges. `Compiled` stores the topological order.
  Sidechain taps are pre-fader and ignore mute and solo. Return tracks
  count against `MAX_TRACKS`. The built-in limiter has zero latency.
  Sidechain from a track with CLAP inserts is marked "latency not
  compensated" in the UI.
- **Effect memory (rt-7).** Built-in effects use typed pools in `Runtime`
  with fixed counts (16 delays, 16 reverbs, 32 EQs, 32 compressors, 32
  saturators), sized for the current stream rate; delay maximum 4 s.
  `apply()` rejects edits beyond the pool counts. A newly assigned pool
  entry is cleared on the audio thread in 256-frame chunks while its
  input is faded in; delay-time changes are smoothed.
- **Preset chains (rt-8, fmt-7).** `LoadPreset` is one atomic `Edit`: it
  checks insert counts before creating anything; plugin instances are
  created and attached before the swap that references them.
- **Measurable UX rules (ux-4).** Each 15.8 rule is tagged with its
  milestone. Click budgets are counted UI actions in an action log, run by
  a scripted test driver; "hearing" means the master meter is above -40
  dBFS. Rule 7 is measured before the limiter. Milestone B includes a
  minimal "Add channel from kit" menu and one built-in default template so
  it can be tested without Milestone C.
- **Audition (ux-5).** A dedicated preview slot in `Compiled` with its own
  preallocated voices, fed by an SPSC preview queue, never an `Edit`.
  Default output: master, before the limiter; routing to the channel's
  track is optional. Debounce 150 ms; 200 rapid row changes cause at most
  2 swaps.

### 17.3 Milestone D presets

- **Macros on CLAP plugins (ux-2, fmt-7).** Macros are ordinary channel
  values written to their targets through an `Edit`. Each target records
  plugin id, plugin version, param id, param name, and range; on load,
  targets are checked against `clap_param_info`, and mismatched macros are
  disabled with a toast. Preset state blobs are referenced by hash, never
  embedded in TOML.
- **Generic controls (ux-3, minor; split verdict, kept).** A GTK parameter
  panel built from `clap_params` (search plus sliders) is the "More
  controls" view and the fallback when no X11 connection is possible. The
  plugin's own GUI is the "Expert" button.
- **Level matching (ux-6).** Per role: a reference note at velocity 100
  rendered offline; short-term K-weighted loudness for tonal roles, peak
  and RMS for percussive one-shots; stored as `trim_db`; tolerance +/-2
  dB; reference presets tested in CI.
- **Tonal or percussive (ux-7).** Channels have `pitch_role: Tonal |
  Percussive` derived from the role tag. Key lock and key-based audition
  apply only to tonal channels. Detected key is `Option<Key>` with a
  confidence threshold; transposing to the project key is a suggestion,
  never automatic.
- **Analysis cache (fmt-8).** Keyed by (path, size, mtime); user role
  edits stored in `~/.local/share/libredaw/user-metadata.toml`.

### 17.4 Packs, libraries, and licensing

- **pack_install (mcp-3).** HTTPS only, system trust store, compiled-in
  index URL. The index lists name, version, size, SHA-256, and license
  per pack. Caps: index 1 MiB, pack 512 MiB unpacked, 20,000 files. Our
  own flat-archive reader rejects symlinks, hard links, absolute paths,
  and `..`, and allows only `.wav` and `.toml`. Hash verified before
  extracting into a temp directory, then renamed into place. Index
  signing is optional later hardening (needs a justified dependency).
- **library_add_folder (mcp-4, license-6).** The folder is chosen by the
  human in a portal file chooser, with the vendor-terms notice as a
  per-folder consent stored in settings. `$HOME` and `/` are rejected;
  depth at most 8, at most 50,000 files, symlinks not followed. Agents get
  pack ids, role counts, and sample ids, not file names, unless the user
  allows it. Analysis runs on a worker thread with a per-file time budget.
  Scanning common install locations is a user-initiated suggestion.
- **Companion plugin package (license-2).** 12.3 is amended: this
  repository and its release artifacts contain no third-party plugins. A
  companion package of libre plugins is a separate artifact with its own
  GPL source-offer checklist; pointing users at distro packages is
  preferred.
- **Presets with plugin state (license-3).** A preset that contains a
  plugin state blob records its origin and license in the
  `libredaw-sounds` manifest; CI rejects one without. Our own presets are
  authored from the plugin's init state.
- **Attribution (license-4).** The built-in browser offers CC0 content
  only, so beginners owe nothing. Other accepted licenses (CC-BY-4.0, and
  others only after review; never ShareAlike or NonCommercial) carry
  source, author, and license per sample into project data, and "Export"
  can generate a credits text.
- **libredaw-sounds licensing (license-7).** Tools (including `kitgen`)
  are GPL-3.0-or-later; content is CC0-1.0 or per file as recorded in the
  manifest; SPDX headers; contributor sign-off. Generated WAVs are
  program output, not derived works of `kitgen`.

### 17.5 History files (fmt-6)

History objects and commits carry `format_version`; they are migrated in
memory when read and never rewritten. Named versions are refs whose file
name is an opaque counter; the human name is a string inside the file.
Restore loads through the 7.3 migrate and validate path and `apply()`
range checks.
---

## Owner decisions (approved 2026-10-07)

1. Approved: the futex syscall exception for sandboxed plugins (risk 3, 9.4).
2. Approved: the GPLv3 section 7 plugin permission and the DCO rule (12.3).
3. Approved: the 13.5 proposal. Phase 2 builds the MVP first; `script`
   builds only the bridge plus `project.get` and `edit`; sandboxing starts
   only after in-process hosting passes validation.

---

## Changelog

### Amendment 11 (2026-10-07, owner)

Playlist moved from Milestone C to B; audio clips added to C; automation
stays after C. Milestone B work starts now, in parallel with finishing A
(owner decision). Note preview on click (`EngineCommand::Preview`, a
simple form of the 17.2 audition). The whole UI must follow the GNOME HIG;
custom widgets are drawn with GtkSnapshot and GSK render nodes, with cached
static layers. Built-in sounds live in the separate `libredaw-sounds`
repository (15.3).

### Amendment 10 (2026-10-07, owner)

Autosave for crashes and OOM kills (7.6): 3 s after the last edit and at
least every 15 s while editing; plugin state every 60 s when changed;
SIGTERM, SIGHUP, SIGINT save then exit; automatic recovery on launch as one
undoable step; validator SIGKILL test.

### Amendment 9 (2026-10-07, owner)

Closing LibreDAW saves the project and exits, with no prompt; never-saved
projects go to `~/Music/LibreDAW/Untitled <n>.ldaw`; a failed save keeps
the window open. Fast restart: view state saved on close and restored on
launch, plus a `tools/dev.fish` rebuild-and-relaunch loop (11). No hot code
swapping.

### Review 2 (2026-10-07): sections 13, 15, 16

Process as in draft 2: four area reviewers plus a license auditor, each
finding judged by two reviewers from other areas. 39 findings; 5 refuted
by both judges and dropped; 1 split verdict kept by the orchestrator;
34 fixed. All fixes are in section 17.

| ID | Sev | Found | Resolution |
|---|---|---|---|
| rt-1 | major | Macros and native instrument knobs had no engine path. | ParamTable (17.1); CLAP undo replays param events. |
| rt-2 | major | Per-key active-note table broke ratchets, glide, overlapping notes. | Owner note id per entry (17.1); ratchet and 808 rules (17.2). |
| rt-3 | minor | Swing at compile time vs live knob; ratchet rounding undefined. | Integer swing, coalesced compile, ratchet divisors (17.2). |
| rt-4 | major | Choke not sample-accurate, no fade. | Sub-block split at choke frames, 1.5 ms fade (17.2). |
| rt-5 | major | Sample decode on compile path, no memory budget. | SampleStore with loader thread and budget (17.2). |
| rt-6 | major | Sidechain edges missing from cycle check; solo silenced key. | One edge list, pre-fader key, zero-latency limiter (17.2). |
| rt-7 | major | Effect buffers sized for max rate; clearing on audio thread. | Typed pools, chunked clearing behind fade (17.2). |
| rt-8 | major | Long operations on GTK thread; analyze plugin instances undefined. | Jobs on export thread, idle-sliced instantiation (17.1). |
| mcp-1 | major | Any same-user process trusted; agent supplied its own confirm. | PRIVILEGED set with human approval, capability masks (17.1). |
| mcp-2 | critical | settings_set could redirect URLs, paths, executables. | Closed enum of settings (17.1). |
| mcp-3 | major | pack_install had no integrity or archive safety. | HTTPS, SHA-256, caps, safe flat reader (17.4). |
| mcp-4 | major | Agent could scan ~ or / and send file names to a remote LLM. | Human folder choice, limits, ids not names (17.4). |
| mcp-5 | major | Agent undo and restore interleaved with human edits. | base_revision, Stale, Busy, author-scoped undo (17.1). |
| mcp-7 | major | Huge requests and long tools blocked the GTK thread. | Control I/O thread, caps, jobs (17.1). |
| mcp-8 | major | Startup contradiction, socket races, untrusted text. | --agent-request banner, flock, structured fields (17.1). |
| ux-1 | major | "No modal dialogs" contradicted approvals; no timeout. | Non-modal approval banner, 60 s typed errors (17.1). |
| ux-2 | major | Macros broke silently on plugin updates. | Targets validated against param info (17.3). |
| ux-3 | minor | Plugin GUI a dead end without XWayland. (Split verdict.) | Generic parameter panel (17.3). |
| ux-4 | major | UX rules not measurable; B depended on C. | Action-count driver, meter threshold, minimal B menu (17.2). |
| ux-5 | major | Audition had no engine path. | Preview slot and queue (17.2). |
| ux-6 | major | Level matching undefined for tonal and velocity-dependent sounds. | Per-role measurement, trim_db, CI (17.3). |
| ux-7 | minor | Key lock ignored drums and atonal samples. | Tonal/percussive role, key suggestion only (17.3). |
| fmt-2 | critical | next_id not persisted across history and restore. | Written in project.toml and commits, max rule (17.1). |
| fmt-3 | major | Step-note predicate with pitch offset ambiguous. | Explicit `off: i8` on notes (17.2). |
| fmt-4 | major | Sample name collisions, unloadable projects, non-atomic copies. | Content-addressed immutable samples (17.2). |
| fmt-6 | major | Version files outside migration and path-safety rules. | Versioned objects, opaque ref names (17.5). |
| fmt-7 | major | Preset blobs and macro mappings had two sources of truth. | Blob by hash, atomic LoadPreset (17.2, 17.3). |
| fmt-8 | minor | Analysis cache unkeyed; project_open could discard work. | Cache key, PRIVILEGED open with autosave (17.1, 17.3). |
| license-1 | major | local_only samples leaked into bundles and git. | Never copied; per-machine path; .gitignore; test (17.2). |
| license-2 | major | 12.3 "no third-party plugins" vs Surge XT companion package. | 12.3 amended; separate artifact with source offer (17.4). |
| license-3 | major | Presets embedding plugin state cannot be CC0 by default. | Origin and license per preset, CI check (17.4). |
| license-4 | major | No CC-BY attribution mechanism. | CC0-only built-in browser; credits for others (17.4). |
| license-6 | major | Vendor-terms warning weak; agent could bypass. | Per-folder human consent (17.4). |
| license-7 | minor | Tools and content licensing mixed in the sounds repo. | Split licenses, sign-off (17.4). |

Dropped (refuted by both judges): mcp-6, fmt-1 (version GC hole) and ux-8,
fmt-5 (history memory bounds), all already resolved by Amendment 8's
persisted, content-addressed history; license-5 (trademark naming), since
nominative use in a folder-scan feature does not copy a product name.

### Amendment 8 (2026-10-07, owner)

The whole history tree is persisted in the project, content-addressed like
git (15.11). Blob GC in 7.4 must keep anything a commit references. The
section 6 memory limit now bounds only the in-memory cache.

### Amendment 7 (2026-10-07, owner)

Imported sounds get the same preset layer via one-time analysis (15.10).
Undo history becomes a tree with named, persisted versions (15.11),
usable by agents through MCP.

### Amendment 6 (2026-10-07, owner)

Agents can control the DAW through MCP (section 16): one control API shared
with scripting, a same-user Unix socket (off by default), a separate
`libredaw-mcp` binary, tools for projects, sounds, patterns, mixer,
playlist, packs, plugins, settings, export, and offline `analyze` so agents
can judge a mix by numbers. Owned by the `script` teammate.

### Amendment 5 (2026-10-07, owner)

Goal: every instrument family of a commercial pattern DAW, libre. Hybrid
approach chosen: existing libre CLAP instruments plus native gaps
(Milestone D, 15.9). Preset and macro layer defined (15.10).

### Amendment 4 (2026-10-07, owner)

User libraries: the browser can scan local installs of other DAWs (also in
Wine prefixes) for WAV samples the user owns. Files are only read locally,
marked `local_only` in projects, and left out of shared project exports by
default (15.3). Phase 1 live matrix cut to PipeWire only, 4 x 120 s (14).

### Amendment 3 (2026-10-07, owner)

Sound content moved to a separate repository, `libredaw-sounds`, under its
own license (CC0-1.0 by default), found by the DAW at runtime (15.3).
Recorded explicitly: content from commercial products is not accepted in
either repository.

### Amendment 2 (2026-10-07, owner)

Goal changed to "make hard-hitting beats (phonk, trap, new compositions)
with zero prior skill". Owner added to scope: sampler and drum kits, 808,
swing and step lanes (velocity, pitch, ratchet), built-in effects with
sidechain and sends, playlist, start screen, templates, zero-skill UX
rules. Staged as Milestones A (original MVP), B (beats), C (songs and UX)
in section 13. New section 15. Cut and stated: "every single instrument"
(we ship a defined set; the rest comes from CLAP plugins) and parity with
commercial DAWs. Built-in drum sounds are synthesized by our own tool, so
no third-party sample licenses are needed. Section 15 still needs the
adversarial review before Milestone B.

### Amendment 1 (2026-10-07, owner)

Phase 1 must run on PipeWire natively. PipeWire through cpal's native host
is now the primary backend; ALSA and JACK are fallbacks and comparisons
(risk 5, 12.1, 12.2, 14). miniaudio was considered and rejected: cpal is
the fixed stack, cpal already has a native PipeWire host, and miniaudio
would add a C library and FFI layer.

### Draft 2 (adversarial review)

Process: four area reviewers and one license auditor (Sonnet) attacked
draft 1 and returned 35 findings. Each finding was then judged by two
reviewers from other areas. Findings refuted by both judges were dropped.
One split verdict was decided by the orchestrator. Duplicates were merged.
Result: 31 findings fixed, 2 dropped, 2 merged into other findings
(rt-8, ipc-7).

| ID | Sev | Found | Changed |
|---|---|---|---|
| rt-1 | major | A fader move during a compile is reverted by the swapped-in state and stays wrong. | Control values moved out of compiled state into an atomic `ControlTable`, rewritten after every document change (4.3). |
| rt-2 | major | Swapping the whole engine state kills sounding voices and loses the active-note table; plugin handle ownership across swaps undefined. | Split into immutable `Compiled` and persistent `Runtime` with fixed maxima and slot generations; plugins in a GTK-owned registry, attached by command (4.1, 9.1). |
| rt-3 | minor | Retire ring full: popped state has no home; compiler backpressure undefined. | Check retire capacity before popping, `R >= S + 1`, compiler coalesces and never drops the latest revision (4.2). |
| rt-4 | major | "No priority inversion by construction" is false for in-process plugins; allocation guard cannot see C `malloc`. | Guarantee narrowed to our code; per-plugin timing histogram and LD_PRELOAD diagnostic shim (3.3). |
| rt-5 | major | Export reused the live plugin: wrong sample rate, live channel silenced, stranded on cancel. | Export uses separate plugin instances loaded from captured state (8). |
| rt-6 | major | Timebase undefined for tempo change and loop wrap. | Anchor-based transport, fraction carried across wraps; new Phase 1 tests (4.6, 14). |
| rt-7 | minor | No device or rate change protocol; FTZ x86-only and missing on export thread. | Device change procedure (4.7); `enter_rt_fp_mode` for x86_64 and aarch64, also on export (3.2). |
| rt-8 | major | Same as ipc-1, ipc-2, ipc-5. | Merged into those. |
| ipc-1 | critical | Host could overwrite a slot the runner is still processing; u32 sequence wraparound. | Write slot only if `plugin_seq >= N - 2`; u64 counters (9.4). |
| ipc-2 | major | Lost-wakeup race (store-load reordering); private futex would not cross processes. | SeqCst fences on both sides, non-private futex, runner wait timeout (9.4). |
| ipc-3 | critical | Host trusted shared-memory content from an untrusted runner (NaN, header, indices). | Private header copy, validated sequence and indices, bus sanitizing, `F_SEAL_SEAL` (9.4). |
| ipc-4 | major | Hung plugin never detected; `--die-with-parent` tied to spawning thread. | Single supervisor thread spawns runners; overrun watchdog; pidfd (9.3, 9.4). |
| ipc-5 | major | Sandboxed runner gets no RT scheduling; namespace tid confusion. | Host promotes runner thread from outside using host-namespace ids; 2-block fallback (9.4, risk 4). |
| ipc-6 | major | Control socket had no bounds: blocking reads, fd flood, unbounded blobs. | Non-blocking supervisor, timeouts, no fds accepted, size limits, keep last good state (9.4). |
| ipc-7 | minor | Section 7 permission cannot cover contributor code without a grant. | Merged with license-4. |
| gtk-1 | major | No `timer-support`/`posix-fd-support`: Surge XT's GUI would be dead. | Both added, backed by GLib sources on the GTK thread (9.1). |
| gtk-2 | major | Nobody pumped the x11rb window; host GUI callbacks unspecified. | x11rb fd on the GLib main context, WM_DELETE_WINDOW, ConfigureNotify, all `clap_host_gui` callbacks (9.2). |
| gtk-3 | major | EventRing and handshakes drained by frame clock, which stops when minimized. | Protocol work on a 10 ms GLib timeout; overflow counter and parameter resync (4.4). |
| gtk-4 | minor | Plugin GUI scale source undefined on XWayland. | Scale policy (1.0 on Wayland, Xft.dpi on X11, override), to verify in Phase 2 (9.2). |
| gtk-5 | minor | No transient parent on Wayland: stacking and focus break. | Documented limits, show/raise toggle, window titles (9.5). |
| gtk-6 | minor | Custom widgets had no keyboard path or accessible roles. (Split verdict; the redraw-cost half was dropped as unsupported.) | Keyboard path and accessible roles (11). |
| fmt-1 | critical | `next_id` inside the undoable project: undo reuses IDs. | `next_id` moved to `Document`, outside undo (5.3, 6). |
| fmt-2 | critical | Plugin state not in the document: undo of removal and autosave lose the patch. | `state: Option<Arc<[u8]>>` in `ClapInstanceRef`; capture points on the GTK thread (5.1, 6, 7.5, 7.6). |
| fmt-3 | major | Bundle save not atomic: crash pairs old TOML with new blobs; orphans. | Immutable generation-named blobs, TOML renamed last, GC after (7.1, 7.4). |
| fmt-4 | major | `toml` serializer cannot produce one-note-per-line; u64 and integer-key issues. | Hand-written canonical emitter, `Vec` instead of integer-keyed maps, range validation in `apply()`, round-trip test (5.1, 6, 7.2). |
| fmt-5 | major | Changing root key or step length made step rows read-only. | Exact step-note predicate; root key and step length edits rewrite step notes (5.2). |
| fmt-6 | major | Script edits and Undo raced with open gesture groups. | Undo/redo disabled and script batches queued while a gesture is open (6). |
| license-1 | major | `assert_no_alloc` is BSD-1-Clause, not BSD-2-Clause. | Crate removed; own allocator wrapper. BSD-1-Clause also added to allow list (3.2, 12.1). |
| license-2 | minor | Deprecated `LGPL-3.0` id; cargo-deny matches literally. | Replaced with `LGPL-3.0-only` and `LGPL-3.0-or-later`; ids added only on real need (12.1). |
| license-3 | minor | cargo-deny would reject our own GPL crates. | `publish = false` and `[licenses.private] ignore = true`; GPL kept off the allow list (2, 12.1). |
| license-4 | major | Section 7 permission needs contributor grants, exact text, and fails with GPL deps. | Exact text, DCO rule, GPL crates need reviewed exceptions (12.3). |
| license-5 | minor | Shipping `deno` would need a full notice set. | `deno` is never shipped; found on PATH (10, 12.2). |
| license-7 | minor | SPDX check skipped many file types; CC-BY-SA icons add attribution duties to the binary. | Check covers every tracked file via a Fish script (not REUSE, which is Python); icons GPL-3.0-or-later (2, 11). |

Dropped (refuted by both judges):
- fmt-7: `deny_unknown_fields` blocks additive fields. Refuted: the spec
  already requires a version bump for any schema change, and newer versions
  get a clear error.
- license-6: dependency table incomplete. Refuted: 12.2 is a direct-
  dependency table, and transitive crates are covered by the generated
  `THIRD_PARTY.md`. (The system-library table was still added to 12.1 as a
  small clarification.)

Other changes in draft 2: the `assert_no_alloc` dependency was removed, the
MVP now includes the keyboard editing path and the plugin GUI, and
"Owner decisions needed" was added.

### Draft 1

Initial draft, pre-review.
