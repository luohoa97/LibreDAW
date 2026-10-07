<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
# Connecting AI agents to LibreDAW

`libredaw-mcp` is a small program that an AI client starts. LibreDAW's
control socket (`$XDG_RUNTIME_DIR/libredaw/control.sock`) speaks MCP itself,
so `libredaw-mcp` only relays bytes between the client's stdin/stdout and
that socket (and starts LibreDAW if it is not running, waiting up to 30 s).
Any MCP client that can launch a stdio server works unchanged. There is no
network listener and no HTTP transport. Agent control is off until you
switch it on in LibreDAW for the session; risky actions (loading a plugin
for the first time, opening a project over unsaved work, and so on) wait for
your click in the LibreDAW window. If you connect before enabling it, the
client's connection waits about 25 s for you to click the banner and then
fails with a message saying what to enable.

## What an agent gets

- Tools, built for few round trips: `project_summary` (start here),
  `beat_grid_set` / `beat_grid_get` (drum rows as text like `x...|x...|x.2.|x.68`),
  `notes_write` (`C2:0:1/4 E2:1/4:1/8:90`, times are fractions of a bar),
  `mix_set` (batched), `activity_set` (what the agent is doing, shown as
  an orange glow and a pill in the window), `sound_search`, `kit_add`,
  `suggestion_submit`, plus the structured tools (`edit`, `channel_add`,
  `pattern_new`, `steps_set`, `notes_add`, `track_set`, `analyze`,
  `export_wav`, transport, history, settings, plugins). Every editing tool
  returns the new revision and a compact diff; each call is one undo group.
  The grammar of both text forms is in the tool descriptions.
- Resources: `libredaw://project`, `libredaw://pattern/<id>`,
  `libredaw://mixer`, `libredaw://song`, `libredaw://suggestions_pending`.
  Subscribe and you are told when the user changes the project.
- Prompts: `make_beat` (genre, tempo), `add_hihat_roll`, `fix_my_mix`.
- Progress notifications while an export or analysis runs; logging.
- Suggestions: the Suggest button in the Pattern and Song views asks the
  connected agent. If the client supports MCP sampling the DAW asks its
  model directly; otherwise the request appears in
  `libredaw://suggestions_pending` and the agent answers with
  `suggestion_submit`. Either way the user sees a preview and accepts or
  rejects it; nothing changes before that. LibreDAW has no model, no API
  key, and no network client of its own.
- Names and tags from the project are untrusted: tools return them cleaned,
  capped, and only in quoted or structured fields.

## Flatpak

Inside the LibreDAW Flatpak the program is started as

```sh
flatpak run --command=libredaw-mcp io.github.luohoa97.LibreDAW
```

and `libredaw-mcp setup` run inside the sandbox cannot see or run the AI
clients on your host. It prints the exact commands to run on the host
instead and changes nothing, for example:

```sh
claude mcp add --scope user libredaw -- flatpak run --command=libredaw-mcp io.github.luohoa97.LibreDAW
codex mcp add libredaw -- flatpak run --command=libredaw-mcp io.github.luohoa97.LibreDAW
```

(`flatpak run --command=libredaw-mcp io.github.luohoa97.LibreDAW setup`
prints them for every client.) The control socket lives in
`$XDG_RUNTIME_DIR/app/io.github.luohoa97.LibreDAW/libredaw/`, the one
runtime directory the Flatpak shares between separate `flatpak run`
invocations, so the relay finds the running DAW.

## Easiest: one command for every client found

```sh
libredaw-mcp setup            # shows each change, asks before applying
libredaw-mcp setup --yes      # apply without asking
libredaw-mcp setup --remove   # undo
libredaw-mcp setup --only cursor
```

It uses the absolute path of the `libredaw-mcp` you ran, prefers a client's
own CLI where there is one, and otherwise edits the config file, keeping
everything else and leaving a `*.libredaw-backup` copy. A config file that
contains comments (Zed's `settings.json` often does) is not touched; the
command prints what to add by hand.

## By hand, one line each

Put the real path in place of `/path/to/libredaw-mcp` (`command -v libredaw-mcp`).

For the Flatpak, replace `/path/to/libredaw-mcp` by
`flatpak run --command=libredaw-mcp io.github.luohoa97.LibreDAW` (as
`command` plus `args` in JSON files).

| Client | One-line setup |
| --- | --- |
| Claude Code | `claude mcp add --scope user libredaw -- /path/to/libredaw-mcp` |
| Claude Code (plugin) | `claude --plugin-dir packaging/agents/claude-code-plugin` (needs `libredaw-mcp` on `PATH`) |
| Codex | `codex mcp add libredaw -- /path/to/libredaw-mcp` |
| Cursor | add `{"mcpServers":{"libredaw":{"command":"/path/to/libredaw-mcp","args":[]}}}` to `~/.cursor/mcp.json` |
| VS Code | `code --add-mcp '{"name":"libredaw","command":"/path/to/libredaw-mcp","args":[]}'` |
| Gemini CLI | `gemini mcp add -s user libredaw /path/to/libredaw-mcp` |
| Zed | add `"context_servers": {"libredaw": {"command": "/path/to/libredaw-mcp", "args": [], "env": {}}}` to `~/.config/zed/settings.json` |
| Claude Desktop | build the bundle below and open the `.mcpb` file |

## Files here

- `claude-code-plugin/`: a Claude Code plugin (`.claude-plugin/plugin.json`
  and `.mcp.json`). The server entry runs `libredaw-mcp` from `PATH`.
- `claude-desktop/manifest.json`: MCP bundle (MCPB) manifest for Claude
  Desktop. A bundle is a zip with this `manifest.json` at the top and the
  binary at `server/libredaw-mcp`:

  ```sh
  mkdir -p build/bundle/server
  cp packaging/agents/claude-desktop/manifest.json build/bundle/
  cp target/release/libredaw-mcp build/bundle/server/
  (cd build/bundle && zip -r ../libredaw.mcpb .)
  ```

## Formats checked (2026-10-07)

Against the official docs: Claude Code (`claude mcp add/remove`, scopes,
`.mcp.json`, plugin manifest fields and layout, `${CLAUDE_PLUGIN_ROOT}`),
MCPB `MANIFEST.md` (manifest_version 0.3, binary server, `${__dirname}`),
Codex MCP page (`codex mcp add`, `[mcp_servers.<name>]`), Cursor MCP page
(`~/.cursor/mcp.json`, `mcpServers`), VS Code MCP page (`mcp.json` with
`servers`, `code --add-mcp`), Gemini CLI MCP page (`~/.gemini/settings.json`,
`gemini mcp add`), and Zed's MCP page (`context_servers`).

Not confirmed from docs: `codex mcp remove` (used by `--remove`), whether
Zed's current docs still use the flat `command`/`args`/`env` form on every
version, and that Zed's `settings.json` accepts comments (assumed, which is
why `setup` refuses to rewrite files it cannot parse as plain JSON). The
Claude Code plugin and the MCPB manifest have not been loaded in the real
clients.
