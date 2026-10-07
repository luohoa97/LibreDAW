<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
# Assets

Every non-code asset (icons, images, fonts, samples) is listed here with its
license and source. Binary files and JSON files cannot carry an SPDX comment, so each one
must appear below as `` `path` `` in the table. `tools/check-spdx.fish` fails
if a tracked binary file is missing from this list.

Fonts and icons come from the system theme and are not bundled (SPEC 11).

| Path | License | Source |
|---|---|---|
| `packaging/agents/claude-code-plugin/.claude-plugin/plugin.json` | GPL-3.0-or-later | LibreDAW (Claude Code plugin manifest) |
| `packaging/agents/claude-code-plugin/.mcp.json` | GPL-3.0-or-later | LibreDAW (MCP server registration) |
| `packaging/agents/claude-desktop/manifest.json` | GPL-3.0-or-later | LibreDAW (MCP bundle manifest) |
| `crates/audiofile/tests/fixtures/sine440.flac` | CC0-1.0 | LibreDAW (0.2 s 440 Hz sine generated with ffmpeg for decoder tests) |
| `crates/audiofile/tests/fixtures/sine440.mp3` | CC0-1.0 | LibreDAW (0.2 s 440 Hz sine generated with ffmpeg for decoder tests) |
| `crates/audiofile/tests/fixtures/sine440.ogg` | CC0-1.0 | LibreDAW (0.2 s 440 Hz sine generated with ffmpeg for decoder tests) |
| `crates/audiofile/tests/fixtures/sine440.wv` | CC0-1.0 | LibreDAW (0.2 s 440 Hz sine generated with ffmpeg for decoder tests) |
| `crates/transcribe/model/nmp.onnx` | Apache-2.0 | Spotify Basic Pitch, `saved_models/icassp_2022/nmp.onnx` from github.com/spotify/basic-pitch, SHA-256 2c3c1d144bfa61ad236e92e169c13535c880469a12a047d4e73451f2c059a0ec; NOTICE in `crates/transcribe/model/NOTICE.md` |
