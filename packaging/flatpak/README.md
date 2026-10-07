<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
# LibreDAW Flatpak

App id: `io.github.luohoa97.LibreDAW` (owner decision, 2026-10-07).

## Runtime choice

GNOME runtime 50 (libadwaita 1.9, newer than the required 1.5), built on
Freedesktop 25.08. Therefore the SDK extensions (`rust-stable`, `llvm20`) and
the `org.freedesktop.LinuxAudio.Plugins` extension point use branch `25.08`.
`llvm20` provides libclang for `bindgen` (`LIBCLANG_PATH=/usr/lib/sdk/llvm20/lib`).

## Build

    fish packaging/flatpak/cargo-sources.fish        # Cargo.lock -> cargo-sources.json
    flatpak-builder --user --install-deps-from=flathub --force-clean \
        --repo=packaging/flatpak/repo packaging/flatpak/build-dir \
        packaging/flatpak/io.github.luohoa97.LibreDAW.yml
    flatpak build-bundle packaging/flatpak/repo LibreDAW-0.1.0.flatpak io.github.luohoa97.LibreDAW

`cargo-sources.fish` replaces the Python `flatpak-cargo-generator`: it reads
`Cargo.lock` and writes archive sources (crates.io URL + SHA-256 from the
lockfile) under `cargo/vendor/`, with `.cargo-checksum.json` files and a
`cargo/config.toml` that vendors crates-io. The build runs `cargo --offline`.

## Notes

- The sound packs module uses a local directory
  (`/var/home/neilluo/Projects/libredaw-sounds`). A release needs a public
  git URL and tag for `libredaw-sounds`.
- Plugins: CLAP plugins come from Flatpak plugin extensions
  (`org.freedesktop.LinuxAudio.Plugins.*`, under `/app/extensions/Plugins`) and
  from `~/.clap` (read-only). Host plugins in `/usr/lib/clap` are not visible
  inside the sandbox.
- Scripting is disabled in the Flatpak (no `deno` in the sandbox).
- Findings from the first test build (Flatpak 1.18, GNOME 50):
  - Flatpak 1.18 rejects `--socket=pipewire`; the manifest uses
    `--filesystem=xdg-run/pipewire-0` for now.
  - `APP_ID` in `crates/ui/src/run.rs` must be `io.github.luohoa97.LibreDAW`
    (the app cannot own another D-Bus name in the sandbox).
  - Real-time priority is not granted inside the sandbox: audio runs through
    PipeWire but `pw_out` stays SCHED_OTHER (the host build gets SCHED_RR).
    RealtimeKit calls from a sandbox carry sandbox pid/tid; the realtime
    portal (`org.freedesktop.portal.Realtime.MakeThreadRealtimeWithPID`)
    translates them. The engine needs a portal path when `/.flatpak-info` exists.
  - `flatpak-builder` needs the system `appstreamcli` (with compose) first on
    `PATH`; the Homebrew build lacks `appstreamcli-compose`.
- Agents: `flatpak run --command=libredaw-mcp io.github.luohoa97.LibreDAW`.
