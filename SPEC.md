# LibreDAW Specification

Status: DRAFT 1 (pre-review). Phase 0. No code exists yet.
License of this document: GPL-3.0-or-later, same as the project.

LibreDAW is a native Linux desktop DAW with a pattern-based workflow (step
sequencer, piano roll, mixer). Behavior is inspired by pattern-based DAWs.
No proprietary names, assets, icons, or UI layouts are copied.

---

## 0. Risks (read first)

These are the known risks, stated before anything else.

1. **Plugin GUIs cannot be embedded under GTK4.** GTK4 removed GtkSocket and
   has no foreign-window embedding. Nearly all Linux CLAP plugins only
   support the X11 GUI API. Plugin GUIs will be separate floating windows
   (through XWayland on Wayland sessions). We cannot position them relative
   to the host window on Wayland. This is permanent for the GTK4 stack, not
   an MVP shortcut.
2. **The MVP plugin runs in-process, unsandboxed.** A crashing or misbehaving
   plugin takes down the whole DAW, including unsaved work. Mitigation in MVP:
   autosave (section 7.6). Real mitigation is the sandbox (post-MVP).
3. **Sandboxed plugins conflict with the "no syscalls on the audio thread"
   rule.** Cross-process audio needs a wakeup. Section 9.4 picks a design
   that needs exactly one non-blocking `futex(FUTEX_WAKE)` per block and
   adds one block of latency. This is an explicit, documented exception
   that needs approval.
4. **cpal on Linux.** cpal's ALSA backend through `pipewire-alsa` adds a
   buffering layer and gives weaker scheduling than a native graph client.
   The JACK backend (through `pipewire-jack`) gives proper RT scheduling.
   We prefer JACK when present. Phase 1 measures both; if the numbers are bad
   we say so before Phase 2.
5. **Licensing of the plugin boundary is a gray area.** See section 12.3.
   We propose a GPLv3 section 7 additional permission for CLAP plugins. That
   is a legal decision for the project owner, and this spec is not legal advice.
6. **Deno embedding cost.** Embedding V8 (`deno_core`) pulls in a prebuilt
   static V8 binary and a very large dependency tree. Section 10 instead runs
   the `deno` CLI as a child process. This requires the user to have `deno`
   installed (or us to ship it).
7. **Conflict between the MVP cap and the Phase 2 team plan.** The MVP cap
   lists neither scripting nor sandboxing, yet Phase 2 assigns a `script`
   teammate and asks `plugin-host` for bwrap sandboxing. Section 13.3
   proposes how to resolve this; it needs the owner's decision.
8. **No arrangement / playlist in MVP.** The MVP cap does not include a song
   arrangement view. MVP plays and exports one pattern (looped N times).
   This makes the MVP a pattern sketchpad, not a song tool.

---

## 1. Goals and non-goals

Goals:
- Pattern-based composition: step sequencer and piano roll editing the same
  per-channel note data.
- Hard real-time safe audio engine.
- Project files that are human-readable, versioned, and diff well in git.
- Fully GPLv3-compatible dependency tree; every asset has a recorded license.

Non-goals (MVP and possibly forever):
- Windows/macOS support.
- VST2/VST3/LV2/AU hosting (CLAP only; LV2 may be revisited post-MVP).
- Audio recording, MIDI input, sample editing, time stretching.
- Embedded plugin GUIs (see risk 1).

---

## 2. Workspace layout

```
LibreDAW/
  Cargo.toml              workspace
  LICENSE                 GPL-3.0 full text
  deny.toml               cargo-deny policy (committed)
  THIRD_PARTY.md          every dependency + license
  ASSETS.md               every non-code asset + license + source
  SPEC.md
  crates/
    protocol/             shared types; owned by the orchestrator only
    engine/               audio thread, mixer, transport, synth, sequencer playback, offline render
    plugin-host/          CLAP loading; later the sandbox runner binary
    ui/                   GTK4 app shell, widgets, document + undo; binary `libredaw`
    script/               Deno child-process bridge
```

Dependency direction (no cycles):
`protocol` <- `plugin-host` <- `engine` <- `ui`; `protocol` <- `script` <- `ui`.

Every `.rs` file starts with:
```
// SPDX-License-Identifier: GPL-3.0-or-later
```
CI fails if any `.rs`, `.toml`, `.md` (except LICENSE), or `.ts` file in the
repo lacks an SPDX line. Markdown uses an HTML comment form.

---

## 3. Threading model

| Thread | Priority | May block | May allocate | Owns |
|---|---|---|---|---|
| Audio (cpal callback) | RT (SCHED_FIFO via backend / rtkit) | never | never | `EngineState` |
| GTK main thread | normal | yes | yes | `Document`, undo stack, CLAP main-thread calls |
| Compiler thread | normal | yes | yes | builds `EngineState` snapshots from `Document` |
| Disposal thread | low | yes | frees | drops objects retired by the audio thread |
| Export thread | normal | yes | yes | offline render (own engine instance) |
| Script I/O thread | normal | yes | yes | stdio pipe to the `deno` child |

Rules:
- The audio thread only touches: its own preallocated `EngineState`, SPSC
  ring buffers (wait-free), and atomics. Nothing else.
- No `Mutex`, `RwLock`, `Arc` drop, `Vec` growth, `Box` alloc/free, logging,
  or `println!` on the audio thread. Enforced in debug builds by wrapping the
  callback with `assert_no_alloc` (aborts on allocation).
- Denormals: the audio thread sets FTZ/DAZ (x86_64 MXCSR) at the top of
  every callback.
- The audio thread never waits on a lower-priority thread, so priority
  inversion cannot occur by construction. All cross-thread traffic is SPSC
  with a single producer and single consumer per ring.
- Reading `CLOCK_MONOTONIC` via vDSO (`Instant::now()`) is permitted on the
  audio thread for measurement only; it is not a syscall on x86_64 Linux.

---

## 4. Engine / UI boundary

### 4.1 Data flow

```
Document (Arc-shared, GTK thread)
   | edit -> new Document root
   v
Compiler thread: compile(&Document, &prev) -> Box<EngineState>
   | SPSC "state ring" (capacity 4, pointers)
   v
Audio thread: swaps at block boundary; pushes old Box<EngineState> to
   | SPSC "retire ring" (capacity 8)
   v
Disposal thread: drops it
```

- `EngineState` is plain data: preallocated `Vec`s sized at compile time,
  never resized on the audio thread.
- If the retire ring is full, the audio thread keeps the old state and does
  not swap this block (it retries next block). It never drops on its own thread.
- Fast parameter path: volume, pan, mute, solo, tempo, and plugin parameter
  values go through a separate SPSC `ParamRing` of fixed-size
  `ParamChange { target: ParamTarget, value: f32 }` records, so a fader drag
  does not recompile. The UI also applies the same change to `Document`.
  The next compiled state carries the same values, so the two paths converge.

### 4.2 Engine to UI

- Playhead position (ticks), transport state, xrun count, per-track peak
  meters: written by the audio thread into atomics (`AtomicU64`, `AtomicU32`
  holding `f32` bits). The UI reads them in a GTK tick callback (frame clock,
  ~60 Hz). No queue needed; last value wins.
- Discrete events (plugin requested restart, plugin parameter changed by its
  own GUI, render progress): SPSC `EventRing` from audio to GTK thread,
  fixed-size records, drained each frame.

### 4.3 Block processing

- The engine processes in sub-blocks of at most `MAX_BLOCK = 256` frames.
  A cpal callback of any size is split into sub-blocks. All scratch buffers
  are sized for `MAX_BLOCK` at stream start.
- State swaps, param changes, and transport commands are applied only at
  sub-block boundaries. Note events are sample-accurate inside a sub-block.

### 4.4 Timebase

- Musical time: ticks, `PPQ = 960`. Stored as `u64`.
- Tempo: one constant tempo per project in MVP (no tempo automation).
- Tick-to-sample conversion uses `f64` samples-per-tick computed once per
  state; the transport keeps an integer tick counter plus an `f64`
  fractional-sample accumulator. Event positions are computed from the
  absolute tick, not by accumulating per-block rounding, so drift is zero
  by construction (verified by a test in Phase 1).

---

## 5. Domain model

### 5.1 Entities (MVP)

- `Project`: tempo, time signature (numerator 1..16, denominator 4 in MVP),
  channels, patterns, mixer, metronome settings, `next_id`.
- `Channel`: id, name, instrument (`Synth(SynthParams)` or
  `Clap(ClapInstanceRef)`), root key (for step entry), target mixer track.
- `Pattern`: id, name, length in steps (1..64), step length (default 1/16 =
  240 ticks), and `notes: BTreeMap<ChannelId, Vec<Note>>`.
- `Note`: id, start tick, length ticks, key 0..127, velocity 1..127.
- `MixerTrack`: id, name, volume dB, pan -1..1, mute, solo, up to 8 insert
  slots (`ClapInstanceRef`), output = master. Master is track 0.

### 5.2 Step sequencer and piano roll share data

A step is a note on the step grid with key = channel root key and length =
one step. Toggling a step adds or removes such a note. If a channel's notes
in a pattern contain anything that is not a pure step note, the step row
shows a "piano roll data" marker and becomes read-only for that channel.
This avoids two parallel data models and their sync bugs.

### 5.3 IDs

All entity IDs are `u32` from the project's monotonic `next_id`. IDs are
never reused within a project. They are stable across save/load, which
undo, scripting, and diffs rely on.

---

## 6. Undo / redo

Model: **persistent snapshots with structural sharing**, not inverse commands.

- `Document` is a tree of `Arc`s: `Arc<Project>` holding `Vec<Arc<Pattern>>`,
  `Vec<Arc<Channel>>`, `Arc<Mixer>`.
- An edit is a pure function `fn apply(doc: &Document, edit: &Edit) -> Result<Document, EditError>`
  that clones only the touched path (`Arc::make_mut` on a cloned root).
- The undo stack stores whole `Document` roots. Undo = restore previous
  root. There are no inverse operations to get wrong.
- `Edit` is one `enum` covering every mutation (add note, move notes, set
  step, set volume, ...). Scripts and UI produce the same `Edit` values.
- Grouping: continuous gestures (fader drag, note drag) open a group on
  press and close it on release; intermediate states replace the top entry
  instead of pushing. CLAP param gestures (`begin_gesture`/`end_gesture`)
  map to groups the same way.
- Limit: 200 entries or 256 MiB estimated, whichever first; oldest dropped.
- Not undoable: transport state, view state (scroll, zoom, selection),
  plugin-internal state not exposed as parameters (stated in the UI).
- The compiler reuses compiled per-pattern data when `Arc::ptr_eq` shows the
  pattern did not change, so undo/redo recompiles only what changed.

---

## 7. Project format

### 7.1 Container

A project is a directory bundle `Name.ldaw/`:
```
Name.ldaw/
  project.toml          the document (text)
  plugin-state/
    <channel-or-insert-id>.bin    opaque CLAP state blobs (binary)
```
Binary plugin state stays out of the text file so the text diffs cleanly.

### 7.2 Encoding

- TOML via `serde` + `toml`. First line: `format_version = 1`.
- Deterministic output: maps are `BTreeMap`, collections are sorted by id
  (notes by `(start, key, id)`), floats are written with Rust's shortest
  round-trip formatting, `-0.0` is normalized to `0.0`, NaN/inf are rejected
  on save and load.
- Notes are inline tables, one per line, so moving one note changes one line:
  `{ id = 41, start = 960, len = 240, key = 60, vel = 100 }`.

### 7.3 Versioning and migration

- `format_version` is an integer. Loading version N < current runs
  migrations `v1_to_v2`, ... on the untyped `toml::Table`, then deserializes.
- A file with `format_version` > current is refused with a clear error
  (never silently opened and re-saved with data loss).
- Unknown keys: rejected (`deny_unknown_fields`) so typos fail loudly.

### 7.4 Save

Atomic: write `project.toml.tmp`, `fsync`, `rename` over `project.toml`,
`fsync` the directory. Plugin state blobs are written the same way before
the TOML.

### 7.5 Plugin references

`ClapInstanceRef` stores the CLAP plugin id (e.g. `org.example.synth`), the
plugin version string seen at save time, and the state blob file name. A
missing plugin on load produces a placeholder that keeps the state blob and
re-saves it untouched.

### 7.6 Autosave

Every 2 minutes, if dirty, the GTK thread snapshots the current `Document`
root (an `Arc` clone, cheap) and a worker thread writes it to
`Name.ldaw/.autosave/`. Needed because the MVP plugin is in-process.

---

## 8. Audio engine (MVP features)

- Transport: play, stop, loop current pattern, tempo 20..999 BPM.
- Metronome: synthesized click (short sine burst, accent on beat 1), no
  sample assets. Own gain, on/off, routed to master.
- Sequencer playback: per channel, sorted note array per pattern, cursor
  found by binary search on swap/seek. Pending note-offs are kept in a
  preallocated active-note table (sized to channel count x 128 keys) independent of pattern data, so deleting a sounding note never
  leaves it hanging. Stop and loop wrap send all pending note-offs.
- Built-in synth: 16 voices, oldest-voice stealing, 2 oscillators
  (sine, saw, square, triangle; polyBLEP), state-variable lowpass filter,
  amp ADSR and filter ADSR. All voices preallocated.
- Mixer: per track volume (dB, smoothed over 10 ms per sample), pan
  (-3 dB constant-power law), mute, solo (solo-in-place), insert slots,
  master. Peak meters per track.
- WAV export: renders `loops x pattern length` (+ optional tail) on the
  export thread with its own `EngineState`. Output 16/24-bit PCM with TPDF
  dither or 32-bit float. Hand-written WAV writer (about 100 lines) instead
  of a crate.
- Export and live CLAP instances: the live stream is told to stop calling
  the plugin (ack via EventRing); the export thread then becomes the
  plugin's audio thread for the render; afterwards ownership returns.
  Plugins are put in offline mode via the CLAP `render` extension if offered.

---

## 9. Plugin hosting

### 9.1 MVP: in-process CLAP

- Load with `libloading`, raw bindings from `clap-sys`.
- Host thread contract: CLAP `[main-thread]` calls happen only on the GTK
  main thread. `request_callback` from any thread only sets an atomic flag;
  a GTK tick source polls the flag (calling into GLib from the audio thread
  is not allowed).
- `thread_check` extension: host reports main thread = GTK thread, audio
  thread = whichever thread currently owns processing (live or export).
- Lifecycle: create/init/activate on main thread; `start_processing` on
  audio thread at the first block. Deactivation: GTK thread posts
  "release plugin" via command ring, audio thread calls `stop_processing`
  and acks, then GTK thread calls `deactivate`.
- Parameters: `params` extension; flush via `params.flush` when not
  processing. Plugin-originated param changes come back through the output
  event list and go to the EventRing.
- Supported extensions in MVP: `audio-ports`, `note-ports`, `params`,
  `state`, `gui`, `latency` (reported, not compensated), `thread-check`,
  `render`, `log` (log calls are buffered to a ring, not printed on the
  audio thread).
- GUI: `gui` extension. Prefer `is_floating`; otherwise LibreDAW creates its
  own X11 top-level window with `x11rb` and passes it as the parent. On a
  Wayland session this window lives in XWayland. HiDPI via `set_scale`.
- Test plugins (not bundled): Surge XT (GPL-3.0) and Airwindows
  Consolidated (MIT).

### 9.2 Post-MVP: sandboxed plugins

- Each plugin instance runs in `libredaw-plugin-runner` (our binary, from
  the `plugin-host` crate) under `bwrap`:
  `--unshare-all --die-with-parent --new-session`, read-only bind `/usr`
  and the plugin bundle, tmpfs `$HOME`, a writable bind of a per-plugin
  state dir, no network, X11 socket only if the GUI is opened.
- Known hole: X11 access allows keylogging and screen reading by the plugin.
  Documented, not fixed.

### 9.3 IPC channels

- Control: Unix `SOCK_SEQPACKET` socket pair. Used for lifecycle, GUI,
  state save/load, parameter metadata, and to pass the shared memory fd via
  `SCM_RIGHTS`. Only non-RT threads use it.
- Audio: one `memfd` per instance, sealed with `F_SEAL_SHRINK | F_SEAL_GROW`
  after sizing, mapped by both sides. Layout:

```
offset 0   Header { magic: u64, version: u32, max_block: u32,
                    in_ch: u32, out_ch: u32, sample_rate: f64 }
           Sync   { host_seq: AtomicU32, plugin_seq: AtomicU32,
                    plugin_sleeping: AtomicU32 }   each on its own 64-byte line
           Slot[2] { frames: u32, in: [f32; in_ch*max_block],
                     out: [f32; out_ch*max_block],
                     events_in: EventRing, events_out: EventRing }
```

### 9.4 Audio sync (needs approval: syscall exception)

- Pipelined by one block: in block N the host writes inputs into slot
  `N % 2`, publishes `host_seq = N` (release), and reads outputs of block
  N-1 from the other slot if `plugin_seq >= N-1` (acquire).
- The host never waits. If the runner has not finished N-1, the host
  outputs silence for that plugin and counts a plugin overrun.
- Wakeup: the runner spins briefly, then sets `plugin_sleeping = 1` and
  `futex_wait`s on `host_seq`. The host calls `futex_wake` only when
  `plugin_sleeping == 1`. This is one non-blocking syscall per block in the
  common case. It is the exception to the no-syscall rule.
- Cost: +1 block latency per sandboxed plugin, reported to the user. No
  plugin delay compensation in MVP.
- Crash: runner death is detected by the control thread (socket HUP); the
  slot is marked dead via an atomic and the host outputs silence.

---

## 10. Scripting (Deno, control rate only)

- Runtime: the `deno` CLI as a child process, launched with
  `deno run --no-prompt --deny-all` plus `--allow-read` of the script file
  only. Protocol: newline-delimited JSON over stdin/stdout (`serde_json`).
- Why not embedded `deno_core`: avoids a prebuilt V8 static library and
  hundreds of crates; a hung or crashed script cannot freeze the UI; Deno's
  permission model sandboxes it for free. Cost: requires `deno` installed;
  calls have ~0.1-1 ms IPC latency, fine for control rate.
- No DSP in JS, ever. Scripts never see audio buffers. The fastest event a
  script can receive is a transport/beat event at most every 10 ms, delivered
  late (not sample-accurate).
- API surface (TypeScript, version `1`):
  - `project.get()`: read-only JSON snapshot of the document.
  - `edit(fn)`: batch of `Edit` values applied as one undo group.
    Example ops: `addNote`, `removeNotes`, `setStep`, `setVolume`, `setPan`,
    `setMute`, `setSolo`, `setTempo`.
  - `transport.play()`, `transport.stop()`, `transport.state()`.
  - `on("beat" | "patternLoop" | "transport", handler)`.
  - `ui.selection()`: currently selected note ids.
- Every request carries an id; replies are matched by id. A script that
  does not answer a request within 2 s is killed.

---

## 11. UI (GTK4 + libadwaita)

- App shell: `AdwApplicationWindow` with header bar transport controls,
  a channel rack / step sequencer area, a piano roll editor, and a mixer
  panel. Layout is our own; no imitation of another product's layout.
- Custom widgets (GtkWidget subclasses drawing with `snapshot()` and GSK
  render nodes): step grid, piano roll, mixer strip meter. Only visible
  ranges are drawn.
- GObject subclassing (`ObjectSubclass`) is the toolkit's mechanism and is
  exempt from the "no trait hierarchy" rule; our own logic stays in plain
  structs and functions called from thin widget wrappers.
- Icons: system Adwaita icon theme (not bundled) plus any custom icons we
  draw ourselves, licensed CC-BY-SA-4.0 and listed in `ASSETS.md`.
- Fonts: system fonts only. Nothing bundled.

---

## 12. Licensing and dependency policy

### 12.1 Rules

- Project: GPL-3.0-or-later. SPDX header in every source file.
- Allowed dependency licenses: MIT, Apache-2.0, Apache-2.0 WITH
  LLVM-exception, BSD-2-Clause, BSD-3-Clause, ISC, Zlib, MPL-2.0,
  LGPL-2.1-or-later, LGPL-3.0, Unicode-3.0, CC0-1.0. Anything else
  (GPL-2.0-only, proprietary, unknown, missing) fails `cargo deny check`.
- `deny.toml` is committed; CI runs `cargo deny check licenses bans sources advisories`.
- `THIRD_PARTY.md` lists every direct and transitive crate and its license,
  regenerated by a script (written in Rust or shell, never Python).
- Every new crate needs a one-line justification in `THIRD_PARTY.md`.

### 12.2 Proposed direct dependencies

| Crate | License | Why |
|---|---|---|
| cpal | Apache-2.0 | audio I/O (fixed stack) |
| jack (via cpal feature) | MIT (libjack LGPL) | JACK/PipeWire-JACK backend |
| rtrb | MIT OR Apache-2.0 | wait-free SPSC ring buffers |
| gtk4 | MIT | UI toolkit bindings (GTK itself LGPL-2.1+) |
| libadwaita | MIT | adaptive app shell (libadwaita LGPL-2.1+) |
| clap-sys | MIT OR Apache-2.0 | raw CLAP ABI bindings |
| libloading | ISC | dlopen plugins |
| x11rb | MIT OR Apache-2.0 | parent window for X11-only plugin GUIs |
| serde | MIT OR Apache-2.0 | (de)serialization |
| toml | MIT OR Apache-2.0 | project file format |
| serde_json | MIT OR Apache-2.0 | script bridge protocol |
| assert_no_alloc | BSD-2-Clause | debug-only audio-thread allocation guard |
| rustix (post-MVP) | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT | memfd, futex, SCM_RIGHTS |

External programs (not linked): `deno` (MIT, V8 BSD-3-Clause), `bwrap`
(LGPL-2.0-or-later).

### 12.3 Plugin licensing boundary

- LibreDAW distributes no third-party plugins. A user loading a plugin on
  their own machine is private use, which GPLv3 does not restrict.
- In-process (MVP): the plugin `.so` is loaded into our GPL process. The
  FSF position is that dynamically loaded plugins sharing data structures
  form a single combined program. To remove doubt for users and
  distributors, we propose adding a GPLv3 section 7 additional permission:
  "linking LibreDAW with plugins that communicate only through the CLAP
  interface, under any license". Decision for the owner.
- Sandboxed (post-MVP): the plugin runs in a separate process, but inside
  `libredaw-plugin-runner`, which is our GPL code. So the runner + plugin is
  still a combined work in that process. The process boundary isolates the
  main DAW, not the runner. The same section 7 permission covers the runner.
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
8. One CLAP plugin hosted in-process, unsandboxed.

Plus what these need to function: project save/load, undo/redo, audio
device selection.

### 13.2 Explicitly cut from MVP

Playlist/arrangement, multiple tempos, automation lanes, sampler, audio
clips, recording, MIDI input, plugin delay compensation, sandboxing,
scripting, LV2/VST, swing, plugin GUI embedding.

### 13.3 Conflict with the Phase 2 plan (owner decision needed)

Phase 2 assigns `script` and bwrap sandboxing to teammates, but both are
outside the MVP cap. Proposal: Phase 2 builds the MVP first; the `script`
teammate builds only the bridge plus `project.get` and `edit` (no events);
the `plugin-host` teammate starts sandboxing only after in-process hosting
passes validation. Alternative: drop the `script` teammate from Phase 2.

---

## 14. Phase 1 acceptance (metronome)

- Workspace, LICENSE, deny.toml, THIRD_PARTY.md, SPDX headers, CI script.
- cpal metronome, no UI, 120 BPM, 4/4.
- Measurements reported as numbers, per backend (ALSA, JACK), per buffer
  size (64, 128, 256, 512), 10-minute runs:
  - xrun count (backend-reported plus callback gap > 1.5x period)
  - callback interval jitter: mean, p99, p99.9, max (us)
  - click onset drift vs ideal tempo grid after 10 min (samples)
- Offline test: render 1 hour at 44.1 kHz and 48 kHz at several tempos
  (including 133.33 BPM); every click onset must land on the exact
  ideal sample index (rounded). Zero drift required.
- Test in debug mode under `assert_no_alloc`: zero allocations in callback.

---

## Changelog

- Draft 1: initial draft, pre-review.
