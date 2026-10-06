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
5. **cpal on Linux.** The ALSA backend through `pipewire-alsa` adds a
   buffering layer. The JACK backend through `pipewire-jack` gives proper RT
   scheduling. We prefer JACK when present. Phase 1 measures both and
   reports numbers before Phase 2.
6. **Licensing of the plugin boundary is a gray area.** Section 12.3 proposes
   a GPLv3 section 7 additional permission. It only works if every
   contributor grants it, so it must be decided before the first outside
   contribution. This spec is not legal advice.
7. **Scripting depends on an external `deno` binary.** LibreDAW never ships
   it (section 10). Users without `deno` on PATH get no scripting.
8. **Conflict between the MVP cap and the Phase 2 team plan.** Scripting and
   sandboxing are outside the MVP cap but have Phase 2 owners. Section 13.3.
9. **No arrangement / playlist in MVP.** MVP plays and exports one pattern,
   looped N times. It is a pattern sketchpad, not a song tool.

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

Continuously changing values do not live in `Compiled`:
volume, pan, mute, solo (per channel and track), metronome gain, tempo.
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
- The transport is an anchor: `(anchor_sample: u64, anchor_tick: u64,
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

### 7.6 Autosave

Every 2 minutes, if dirty: the GTK thread captures plugin state (7.5 (c)),
clones the `Document` (cheap `Arc` clone), and hands it to a worker thread,
which writes it to `Name.ldaw/.autosave/` with the same procedure as 7.4.
State capture is a main-thread call; for typical plugins it takes under a
few milliseconds. Phase 2 measures it with the test plugins.

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
  Launched with `deno run --no-prompt --deny-all` plus `--allow-read` of the
  script file only. Protocol: newline-delimited JSON over stdin/stdout
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
  libadwaita, GLib, libasound, libjack). LGPL libraries are only ever linked
  dynamically and never bundled.
- Every new direct crate needs a one-line justification in `THIRD_PARTY.md`.

### 12.2 Direct dependencies

| Crate | License | Why |
|---|---|---|
| cpal | Apache-2.0 | audio I/O (fixed stack) |
| jack (via cpal feature) | MIT (libjack LGPL) | JACK/PipeWire-JACK backend |
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

### 13.1 MVP (hard cap)

1. Transport (play/stop/loop pattern, tempo).
2. Metronome.
3. Step sequencer.
4. Basic piano roll (add, delete, move, resize notes; velocity; snap).
5. One built-in synth.
6. Mixer: volume, pan, mute, solo, master.
7. WAV export.
8. One CLAP plugin hosted in-process, unsandboxed (with its GUI).

Plus what these need to function: project save/load, undo/redo, autosave,
audio device selection, keyboard editing path.

### 13.2 Cut from MVP

Playlist/arrangement, tempo automation, automation lanes, sampler, audio
clips, recording, MIDI input, plugin delay compensation, sandboxing,
scripting, LV2/VST, swing, plugin GUI embedding.

### 13.3 Conflict with the Phase 2 plan (owner decision)

Phase 2 assigns `script` and bwrap sandboxing to teammates, but both are
outside the MVP cap. Proposal: Phase 2 builds the MVP first. The `script`
teammate builds only the bridge plus `project.get` and `edit` (no events).
The `plugin-host` teammate starts sandboxing only after in-process hosting
passes validation. Alternative: drop the `script` teammate from Phase 2.

---

## 14. Phase 1 acceptance (metronome)

- Workspace, LICENSE, deny.toml (with `[licenses.private] ignore = true`),
  THIRD_PARTY.md, SPDX headers, `tools/check-spdx.fish`, CI script, and
  `cargo deny check` passing.
- cpal metronome, no UI, 120 BPM, 4/4.
- Measurements reported as numbers, per backend (ALSA, JACK), per buffer
  size (64, 128, 256, 512), 10-minute runs:
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

## Owner decisions (approved 2026-10-07)

1. Approved: the futex syscall exception for sandboxed plugins (risk 3, 9.4).
2. Approved: the GPLv3 section 7 plugin permission and the DCO rule (12.3).
3. Approved: the 13.3 proposal. Phase 2 builds the MVP first; `script`
   builds only the bridge plus `project.get` and `edit`; sandboxing starts
   only after in-process hosting passes validation.

---

## Changelog

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
