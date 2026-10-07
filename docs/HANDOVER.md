<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
# Handover: Oto (repository LibreDAW)

Written 2026-10-07 at the end of the first long build session, for the
next Claude Code session. Read this file first, then `SPEC.md` (the source
of truth) and `docs/next-session.md` (open work).

## 1. What this is

Oto is a GPL-3.0-or-later digital audio workstation for Linux in Rust,
with GTK4 and libadwaita. The goal is "as good as FL Studio, easier, and
GNOME-native". It targets beat makers (phonk, trap, lo-fi) who have never
used a DAW. AI agents drive it through MCP as a first-class frontend.

- Repository: https://github.com/luohoa97/LibreDAW (public).
- Branch: `main`. Every commit on main passes `tools/ci.fish`.
- App ID: `io.github.luohoa97.Oto`.
- Binaries: `oto` (the app) and `oto-mcp` (the MCP relay). Crate names stay
  `libredaw-*`.
- Project files are `.oto` bundles (folders). `.ldaw` still opens. Projects
  live in `~/Music/Oto`.
- Run a release build: `cargo build --release -p libredaw-ui`, then
  `target/release/oto`.

## 2. The owner

- They decide product and UX, and they test by using the app and sending
  screenshots.
- They are direct and impatient with glitches. Show working results, not
  plans.
- They want GNOME HIG compliance ("look like GNOME made it, simple") and
  plain language (SPEC 20.6).
- Their usage budget is limited. Keep scope tight, prefer Sonnet workers,
  and avoid long-lived agents with huge contexts.
- Git identity is the GitHub noreply address (set globally). Commits end
  with the attribution lines given by the session.

## 3. Rules that bind the work (from the owner and the SPEC)

- **Stack:** Rust, GTK4 and libadwaita 1.5, cpal on native PipeWire, CLAP
  plugins. No Python anywhere, including tests and tooling. Shell scripts
  are Fish.
- **Real-time audio:** no allocation, locks or syscalls on the audio
  thread. Tests use the RT allocator guard.
- **Dependencies:** minimal. Each direct dependency has a one-line
  justification in `tools/third-party.fish`, and `THIRD_PARTY.md` is
  regenerated. Run cargo-deny and SPDX headers on every file
  (`tools/check-spdx.fish`; non-comment files go in `ASSETS.md`).
- **One model** (SPEC 20): instruments are rows, music lives in clips, and
  there is no channel rack or pattern mode. Patterns (20.7) are groups of
  clips across rows.
- **Plain language** (20.6): `crates/ui/src/vocabulary.rs` fails the build
  on jargon, for example "preset", which is shown as "Style".
- **MCP is a frontend** (Amendment 19): every user action is a named bridge
  operation that both the UI and MCP call. `crates/control/PARITY.md`
  pairs each user action with its MCP tool.
- **Privacy:** only the human starts the microphone (21.2). Agents can
  prepare a recording but never open the mic. Agents never turn on reading
  the user's FL Studio files.
- **Licensing:** FL Studio content is read in place from the user's own
  install, as `local_only` samples. It is never copied into bundles or the
  repository. Provenance wording states facts only and makes no copyright
  claims (SPEC 22).
- **Disk:** about 50 GB free. A full workspace build is about 12 GB. Never
  empty the Trash, and ask before large downloads.

## 4. Crates

| Crate | What it is |
|---|---|
| protocol | Shared types: model, edits, engine commands, control socket requests and replies, file format (canonical TOML, version 4), validation. The orchestrator owns it. |
| engine | Real-time audio: sequencer, samplers, built-in effects, loudness tap, mic capture, offline render. |
| doc | The document: apply(), undo and the change tree, branches and versions, the bundle store, persistence and recovery, the starter project. |
| ui | The GTK app: timeline, Grid and Piano editor, mixer and effect panels, Sounds pane, Home, Hum, agent glow and presence, the control bridge (`control_bridge.rs`). |
| control | The control socket that speaks MCP natively. Holds the tool definitions (`src/mcp/`), song analysis (`song_map.rs`), BRIDGE.md and PARITY.md. |
| mcp | `oto-mcp`, a byte relay plus `setup`. The setup still writes the old `libredaw` server name; see the open work. |
| plugin-host | CLAP hosting, plugin GUIs on X11, Flatpak plugin-extension scan, preset loading, `presets/instruments.toml` (the Surge XT sounds). |
| library | Finds and indexes the user's FL Studio install (kits, sounds by role, multisamples, licensing guard). |
| audiofile | Decoders for WAV (including Vorbis-in-WAV), FLAC, Ogg, MP3 and WavPack. |
| transcribe | Hum to notes: Spotify Basic Pitch ONNX on tract-onnx, key detection, quantize. |
| script | Deno scripting bridge, which is refused inside Flatpak. |

## 5. State at handover

**Working on main:**
- The timeline model, Grid and Piano editors, mixer, effect panels with
  styles, Drive, Duck to Kick, and Loudness with a live LUFS reading.
- Home page, save-first project switching, and autosave and recovery.
- Agent glow, the pill and the Escape stop.
- The Sounds pane:
  - Surge XT sounds by role
  - the user's FL Studio library, with kits and preview
  - "Install Sounds…" through GNOME Software
- Hum to notes, as a button and as the `hum_prepare` tool.
- MCP:
  - sound search and add by id, kits
  - analyze with per-bar levels and sections, seek
  - versions and branches
  - effects, duck and loudness tools
- The plugin GUI starvation fix: JUCE fd watches were starving GTK.

**Format v4 is merged** (main e3cb90d; CI green):
- **Audio rows and clips:** dropping a file on empty space makes an Audio
  row with one full-length clip and its waveform, in one undo step.
  - Edges trim, corner dots set the fades, and the inspector shows the
    clip's Gain.
- **Patterns lane** under the loop strip:
  - Ctrl+G makes a pattern. Drag, Ctrl+D and Delete act on all its clips.
  - The block menu has Rename and Place at Playhead.
- **Shapes (automation)** for Volume, Left/Right, Pitch, Filter and
  built-in effect settings.
  - Curve types are smooth, linear, hold, stairs, pulse and wave. The
    formulas are in `crates/engine/src/shapes.rs`.
  - Presets: Fade In, Fade Out, Swell, Drop, Pump, Wobble and Tape Stop.
- **Effect bypass:** the On switch sends SetInsertBypass.
- **MCP:** audio_clip_add and audio_clip_set; pattern_make, pattern_place
  and pattern_list; shape_add, shape_set and shape_remove; fx_bypass.
- **Verify first next session:**
  - After deleting a shape, does its last automated value stick? The
    engine overwrites the control tables, and the UI must rewrite them
    from the document after a shape edit.
  - Do audio clips play on screen and through speakers in the real app?
    So far only tests and screenshots cover this.
- **Not done:**
  - A multi-row Grid on double-click (it opens the first member).
  - Pattern resize, Ungroup and New Pattern in the lane menu.
  - Dragging Surge sounds from the pane.
  - A declick on stop or seek inside audio clips.
  - Time-stretch on tempo change.
  - Pitch shapes on CLAP plugins and on audio rows.
- **Stray remote branch:** `fx-wave` exists on GitHub because a teammate
  pushed it against the rules. It is merged, so delete it once the owner
  agrees.

**Known gaps:**
- The MCP Oto rename is unfinished.
- Recording and vocals (21.2, 21.8) are not built. The mic capture path
  exists in `engine/src/capture.rs`.
- Lyrics, provenance export (22), the memory gate (23), templates and the
  Piano tools (24.3) are not built.
- 808 Slide and Time FX (24.2) are not built.
- The Flatpak release (19.3) is not done.
- Hum accuracy on real voices is unmeasured: 70% on hard synthetic
  melodies, against an 85% target.

All of this is in `docs/next-session.md`.

## 6. How to work in this repository

- **CI:** `fish tools/ci.fish` runs fmt, clippy with `-D warnings`, the
  whole workspace test suite, cargo-deny, SPDX and third-party. It must be
  green before anything is merged to main.
- **Shared build folders go stale.** Git worktrees that share one
  `CARGO_TARGET_DIR` reuse each other's compiled `libredaw-protocol` and
  report wrong errors. They also delete each other's artifacts in the
  middle of a run. Give each worktree its own target, or run
  `cargo clean -p libredaw-protocol` before building, and run final CI
  alone.
- **Never `pkill -f` with a pattern that also matches your own shell
  command.** It kills the shell; use pids.
- **Headless UI QA:** the QA harness runs a private
  `mutter --headless` with a RemoteDesktop driver (`rdctl.c`). See
  `/tmp/claude-1001/uxqa/HARNESS.md` if it still exists. It never touches
  the owner's session.
- **Real-plugin tests** are `#[ignore]` (Surge XT, Odin2 and Dexed are
  Flatpak LinuxAudio extensions, branch 25.08). Run them with
  `--ignored`. The GUI tests need an X server.
- **Real FL tests** are `#[ignore]`. The install is at
  `~/.var/app/com.usebottles.bottles/data/bottles/bottles/FL-Studio/drive_c/Program Files/Image-Line/FL Studio 2026/Data/Patches/Packs`.
  Set `OTO_FL_PACKS` to that path.
- **Delegation that worked:** one short-lived Sonnet teammate per concrete
  task, each in its own worktree and branch. It never pushes and never
  edits protocol or SPEC. The orchestrator merges, runs CI and pushes.

## 7. Owner preferences seen this session

- They want things merged and runnable fast, and they test the real app
  immediately.
- They reject duplicate UI: one way to do each thing.
- Agents must never open plugin windows by themselves.
- They prefer libre CLAP plugins (Surge XT) and their own FL samples over
  the built-in synths, which they call junk and which are now hidden.
- They want an agent to be able to do everything the UI can, visibly, with
  the orange glow.
