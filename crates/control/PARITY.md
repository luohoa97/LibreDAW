<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
# MCP parity with the window

MCP is a frontend like the GTK window: an agent must be able to do what a
user does, through the same session operations, with the same undo,
authorship and history (`BRIDGE.md`). This table lists every user action
in SPEC 20 and docs/ui-design.md (read in the timeline model: the old
Pattern and Song views are the Timeline and its clip editor) and the MCP
tool that does it. A row without a tool says why in the gap column.

Kinds of gap:
- **by design**: the action is reserved to the human (17.1 trust model,
  18.1 stop) or is pure view state an agent does not need.
- **protocol**: needs a request or field protocol v3 does not have; the
  proposed change is in the mcp-v3 report.
- **model**: the document has no such property yet.

Tools named `edit` take raw `protocol::edit::Edit` values (ticks, not bars).

## Projects and Home

| User action | MCP tool | Gap |
|---|---|---|
| Start a beat from a template card (Phonk, Trap, ...) or Empty | `project_new {template}` | not done yet: the `name` argument of `ProjectNew` is sent as `None` |
| Open a recent project / Open Project... | `project_list`, `project_open {path}` | the file chooser is by design: agents open paths inside the projects folder only (17.1) |
| Save (Ctrl+S) | `project_save` | |
| Save As... | none | by design: a path outside the projects folder is a PRIVILEGED write; not in the protocol |
| Unsaved Work: Open or Discard a recovery bundle | none | by design: Home page recovery is the user's decision |
| Close project, return to Home | none | not done yet: `ProjectClose` exists in protocol (f5e9677), the `project_close` tool (a global action, 18.6) is not wired |
| Project Properties: tempo, time signature | `song_set {tempo, beats_per_bar}` | |
| Project Properties: name | none | model: no project name edit (the name is the bundle name) |
| Project Properties: key | none | model: no project key yet (15.10.5) |
| Export Audio: range, format | `export_wav {start, end, tail_seconds, format}` | |
| Export Audio: Normalize Loudness | none | protocol: `ExportWav` has no normalize flag; `analyze` gives LUFS and `mix_set` adjusts |
| Demo projects | none | by design (opens a copy of a shipped file) |

## Transport

| User action | MCP tool | Gap |
|---|---|---|
| Play / Stop (Space) | `play`, `stop` | |
| Play from start (Shift+Space), Go to Start (Home), click the ruler | none | protocol: no `Seek` request (proposal P3) |
| Metronome on/off (Ctrl+M), metronome level | `song_set {metronome, metronome_db}` | |
| Loop on/off (Ctrl+L) | `loop_set {enabled}` | |
| Drag a loop region; "loop this clip" | `loop_set {start, end}` or `loop_set {clip}` | |
| Tempo entry, Tap tempo | `song_set {tempo}` | |
| Position display | `transport_state` | |
| Master meter, clip lamp | `analyze` (offline numbers) | by design: live meters mean nothing to an agent that cannot hear |

## Timeline: instruments (rows)

| User action | MCP tool | Gap |
|---|---|---|
| Add Instrument: synth, 808, sampler, plugin | `instruments_add` | |
| New instrument gets its own mixer track | `instruments_add` (`track` defaults to "new") | |
| Drag a sound from the browser below the rows (instrument with a clip) | `kit_add` for kits; `instruments_add` for built-in sounds | protocol: no request adds one pack sound (proposal P4 `SoundAdd`) |
| Drop a sound on an instrument header (replace its sound) | `instrument_set {sample}` for samples already in the project | protocol: pack sounds need P4 |
| Rename (F2) | `instrument_set {name}` | |
| Duplicate instrument (Ctrl+U) | `instruments_add {copy_of}` | plugin instruments: state cannot be copied (no state transfer request) |
| Remove instrument | `instruments_remove` | |
| Reorder rows (drag, Alt+Up/Down) | none | model: no reorder edit (rows follow id order) |
| Change color | none | model: instruments have no color field yet |
| Mute / solo an instrument | `mix_set {instrument, mute, solo}` | |
| Route to a mixer track | `instrument_set {track}` | |
| Root key, choke group | `instrument_set {root_key, choke_group}` | |

## Timeline: clips

| User action | MCP tool | Gap |
|---|---|---|
| Click an empty spot: a one-bar clip | `clips_add` | |
| Place a linked copy of existing content | `clips_add {content}` | |
| Move (drag), nudge | `clips_change {move_by}` / `{move_to}` | |
| Move a clip to another row | `clips_change {instrument}` | |
| Resize at the end or the start (trim), loop by dragging longer | `clips_change {length}` / `{resize_by, from_start}` | |
| Duplicate as linked copy (Ctrl+D), repeat | `clips_copy {times}` | |
| Copy (Alt/Ctrl+drag), copy and paste at a position | `clips_copy {linked: false, to}` | |
| Make Unique | `clips_change {make_unique}` | |
| Split at playhead (S) | `clips_split {at}` | |
| Join | none | protocol: no join edit |
| Mute clip (0) | `clips_change {muted}` | |
| Delete | `clips_remove` | |
| Select, zoom, scroll | none | by design: view state; tools take clip ids |

## Clip editor: Steps

| User action | MCP tool | Gap |
|---|---|---|
| Toggle or paint steps, clear steps | `beat_grid_set` | |
| Accent, ratchet | `beat_grid_set` (`X`, digits, `ratchet`) | |
| Velocity, pitch, ratchet lanes of single steps | `content_set {lanes}`, `beat_grid_set {vel}` | |
| Content length (steps), step length, swing, rename | `content_set {steps, step_length, swing, name}` | |
| Read the grid | `content_get`, `project_summary` | |
| Switch Steps/Notes presentation | none | by design: view state |

## Clip editor: Notes (piano roll)

| User action | MCP tool | Gap |
|---|---|---|
| Add notes (click, Return) | `notes_write` | |
| Delete notes (Delete, double-click) | `notes_edit {remove}` | |
| Move in time and pitch, octave | `notes_edit {move_by, transpose}` | |
| Resize | `notes_edit {resize_by}` | |
| Velocity (lane, + and -) | `notes_edit {velocity}` | |
| Quantize | `notes_edit {quantize}` | |
| Select all | `notes_edit {notes: "all"}` | |
| Copy, paste, duplicate notes | `content_get` then `notes_write` at the new positions | |
| Note ratchet | `edit` with `set_note_repeat` | |
| Click a key to audition it | none | protocol: no audition request (proposal P2) |
| Snap, Stay in Key, zoom | none | by design: view aids (Stay in Key also needs the project key) |

## Audio clips, patterns and shapes (SPEC 20.7, 21.1, 24.2-1)

| User action | MCP tool | Gap |
|---|---|---|
| Drop a sound file on the timeline (new Audio row, full-length clip) | `audio_clip_add {sample, start, length or seconds}` | the file must already be in the project (a hash from `inspect`); import by sound id or path and "full length by default" need a bridge request (see report) |
| Drop on an existing Audio row | `audio_clip_add {instrument}` | |
| Trim the edges, set gain, fades | `audio_clip_set {trim_start, trim_end, gain_db, fade_in, fade_out}` | |
| Move, copy, split, mute, delete an audio clip | `clips_change`, `clips_copy`, `clips_split`, `clips_remove` | |
| Make Pattern (Ctrl+G) | `pattern_make {clips, name}` | |
| Place a pattern at the playhead | `pattern_place {pattern, start}` | |
| See the patterns | `pattern_list`, `project_summary` | |
| Move or duplicate a pattern block | `clips_change`, `clips_copy` on its clips | |
| Rename a pattern | `edit` with `rename_group` | |
| Add a shape (Volume, Pan, Pitch, Filter, effect setting) | `shape_add {target, preset or points}` | |
| Shape presets (Fade In, Fade Out, Swell, Drop, Pump, Wobble, Tape Stop) | `shape_add {preset, start, end}` | |
| Edit shape points, curve types | `shape_set {points}` | |
| Remove a shape | `shape_remove` | |

## Mixer

| User action | MCP tool | Gap |
|---|---|---|
| Fader, pan, mute, solo (tracks and master) | `mix_set {track}` | |
| Reset fader | `mix_set {volume_db: 0}` | |
| Add track or return | `tracks_add` | |
| Rename track, remove track | `edit` (`rename_track`, `remove_track`) | |
| Add effect (EQ ... Limiter, Plugin...) | `fx_add` | plugin effects need the user's approval on first load (by design) |
| Effect controls | `fx_set {params, curve, ping_pong}` | |
| Sidechain input | `fx_set {sidechain}` | |
| Reorder effects | `fx_set {index}` | |
| Remove effect | `fx_set {remove}` | |
| Pick a ready-made setting for an effect (Drive: Warm, Crunch, Phonk 808, Phonk Cowbell, Hard Clip) | `fx_add {preset}`, `fx_set {preset}` | |
| Duck to Kick (instrument menu, mixer strip) | `duck_to_kick {row or track, amount}` | |
| Loudness on Main Output | `loudness {amount or preset}` | the window also shows the LUFS reading; `analyze` gives it for the whole song |
| Effect On switch (EQ ... Limiter) | `fx_bypass {insert, bypass}` | plugin effects have no switch here |
| Sends level, pre/post, remove | `send_set` | |
| Change color | none | model: tracks have no color field |

## Inspector: Sound page

| User action | MCP tool | Gap |
|---|---|---|
| Macro knobs, More Controls | `instrument_set {params}` (names from `inspect`) | |
| Waveform | `instrument_set {wave}` | |
| Sampler mode, reverse, sample | `instrument_set {mode, reverse, sample}` | |
| 808 mono, glide and the other 808 controls | `instrument_set {mono, params}` | |
| Plugin parameters | `instrument_set {params: {"<id>": v}}`, `fx_set` for inserts | |
| Previous / next sound, preset name (browse presets) | none | protocol: presets are not in the control API (agents set parameters directly) |
| Vary | none | by design: a random tweak; an agent chooses parameters itself |
| Expert Window (plugin GUI) | none | by design: a native window for the human |

## Sound browser

| User action | MCP tool | Gap |
|---|---|---|
| Search, role and style filters | `sound_search` | |
| Preview a sound | none | protocol: no audition request (proposal P2) |
| Add as instrument (Return, double-click) | `kit_add` for a whole kit | protocol: single sounds need P4 |
| Add Sound Folder..., install a pack | none | by design: PRIVILEGED (17.1) and not in protocol v3 |

## History and versions (15.11, 15.12)

| User action | MCP tool | Gap |
|---|---|---|
| Undo, redo | `undo`, `redo` | by design: an agent moves only over its own commits |
| History page: commits, authors | `history`, `libredaw://history` | |
| What changed | `history_diff` | |
| Save Version | `version_save` | |
| Restore a version | `version_restore` | PRIVILEGED for agents (17.1): the user approves |
| Versions panel: make versions | `branch_create {from}` | |
| Listen / A-B switch at the next bar | `branch_switch` (+ `play`) | |
| Use This Version | `branch_switch` | |
| Rename, archive a version | `branch_set {name, archive}` | |
| Bring back an archived version | `branch_switch` | |
| Compact History | none | by design: maintenance for the user |

## Agent presence (18)

| User action | MCP tool | Gap |
|---|---|---|
| See what the agent does (pill, glow) | `activity_set {text, focus}` | |
| Suggest button: fills, variations, basslines | `suggestion_submit`, `libredaw://suggestions_pending`, sampling | |
| Approve or deny, stop the agent (Esc), Undo Its Changes | none | by design: only the human |

## Preferences

| User action | MCP tool | Gap |
|---|---|---|
| Output device, buffer size, color scheme, metronome | `settings_get`, `settings_set` | |
| Sample memory | none | protocol: not in the `Setting` enum |
| Sound folders | none | by design: paths are not settable (17.1) |
| Allow agent control | none | by design |

## Plugins

| User action | MCP tool | Gap |
|---|---|---|
| Scan, list plugins | `plugins {scan}` | |
| Load a plugin instrument or effect | `instruments_add {kind: "plugin"}`, `fx_add {fx: {plugin_id}}` | first load needs approval (by design) |

## Voice (SPEC 21.5, later milestone)

| User action | MCP tool | Gap |
|---|---|---|
| Record, hum to notes, transcribe, takes, lyrics, audio import | `record_prepare`, `hum_prepare`, `transcribe`, `takes`, `lyrics_*`, `audio_import` (planned) | not built: Voice milestone |
