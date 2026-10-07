<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
# Connecting AI agents to Oto

`oto-mcp` is a small program that an AI client starts. It talks MCP
over stdin/stdout and controls a running Oto through its control socket
(`$XDG_RUNTIME_DIR/oto/control.sock`). Agent control is off until you
switch it on in Oto for the session; risky actions (loading a plugin
for the first time, opening a project over unsaved work, and so on) wait for
your click in the Oto window.

## Easiest: one command for every client found

```sh
oto-mcp setup            # shows each change, asks before applying
oto-mcp setup --yes      # apply without asking
oto-mcp setup --remove   # undo
oto-mcp setup --only cursor
```

It uses the absolute path of the `oto-mcp` you ran, prefers a client's
own CLI where there is one, and otherwise edits the config file, keeping
everything else and leaving a `*.oto-backup` copy. A config file that
contains comments (Zed's `settings.json` often does) is not touched; the
command prints what to add by hand.

## By hand, one line each

Put the real path in place of `/path/to/oto-mcp` (`command -v oto-mcp`).

| Client | One-line setup |
| --- | --- |
| Claude Code | `claude mcp add --scope user oto -- /path/to/oto-mcp` |
| Claude Code (plugin) | `claude --plugin-dir packaging/agents/claude-code-plugin` (needs `oto-mcp` on `PATH`) |
| Codex | `codex mcp add oto -- /path/to/oto-mcp` |
| Cursor | add `{"mcpServers":{"oto":{"command":"/path/to/oto-mcp","args":[]}}}` to `~/.cursor/mcp.json` |
| VS Code | `code --add-mcp '{"name":"oto","command":"/path/to/oto-mcp","args":[]}'` |
| Gemini CLI | `gemini mcp add -s user oto /path/to/oto-mcp` |
| Zed | add `"context_servers": {"oto": {"command": "/path/to/oto-mcp", "args": [], "env": {}}}` to `~/.config/zed/settings.json` |
| Claude Desktop | build the bundle below and open the `.mcpb` file |

## Files here

- `claude-code-plugin/`: a Claude Code plugin (`.claude-plugin/plugin.json`
  and `.mcp.json`). The server entry runs `oto-mcp` from `PATH`.
- `claude-desktop/manifest.json`: MCP bundle (MCPB) manifest for Claude
  Desktop. A bundle is a zip with this `manifest.json` at the top and the
  binary at `server/oto-mcp`:

  ```sh
  mkdir -p build/bundle/server
  cp packaging/agents/claude-desktop/manifest.json build/bundle/
  cp target/release/oto-mcp build/bundle/server/
  (cd build/bundle && zip -r ../oto.mcpb .)
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
