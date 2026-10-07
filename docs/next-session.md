<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
# Open work after the first timeline build (2026-10-07)

The first build is merged: timeline model (SPEC 20), plain language (20.6),
Oto rename in app, doc and packaging, FL Studio library and decoders,
Surge XT, Odin 2 and Dexed discovery, and MCP tools on v3. Open items, by
owner area:

## UI
- Home page and onboarding (SPEC 19). ProjectClose replies "not available".
- Agent presence: window and object glow, pill, Escape stop, activity
  groups with Undo (18, 18.1, 18.6).
- Versions panel and the history/branch bridge (crates/control/BRIDGE.md;
  the reference bridge is crates/control/tests/support/mod.rs).
- Role-based instrument picker from crates/plugin-host/presets/instruments.toml.
  The native Synth and 808 are still offered (Amendment 23).
- FL Studio section and import in the sound browser (crates/library API:
  scan_with(.., false) first, then scan() in the background).
- Mixer sends and effect parameter panels.
- Piano view: allow drawing past the content length.
- Live audio-device change in Preferences.

## MCP (crates/control, crates/mcp)
- project_new name, project_close as a global action (18.6).
- Oto rename: oto-mcp binary, server name, socket dir, oto:// URIs,
  setup migration of old libredaw entries (temp-HOME tests).
- Plain-language words in tool descriptions (20.6).
- Proposed protocol additions: next_id in the Project reply, Audition,
  Seek, SoundAdd.

## Protocol and doc
- NewInstrument::Clap { preset: Option<String> } for picker presets.
- Starter project 808 row: move to a sampler or Surge XT preset.

## Done since (see docs/HANDOVER.md)
- Format v4: audio clips, patterns lane, shapes, bypass; Home; agent
  presence; FL library; Surge sounds; hum to notes; effects; analysis.

## Later milestones
- Voice: recording, hum to notes, lyrics (SPEC 21).
- Provenance export (SPEC 22).
- Memory gate tools/mem.fish and the budgets (SPEC 23).
- Flatpak release 0.1.0 (SPEC 19.3, packaging/RELEASE.md), including a
  test of the realtime portal inside the sandbox.
