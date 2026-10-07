<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
# Release checklist (SPEC 19.4)

Release 0.1.0 ships only when every gate below is green. Roles: **owner**
(the project owner), **main** (the orchestrator), **packaging**, **validator**,
**ux-qa** (the independent UX tester). Run from the repository root on the
release commit, with a clean tree.

## 1. Gate

| # | Gate | Who | Command or evidence |
|---|---|---|---|
| 1 | CI green | main | `fish tools/ci.fish` (fmt, clippy -D warnings, tests, cargo deny, SPDX, THIRD_PARTY) |
| 2 | Live benchmark, zero xruns | validator | `fish tools/live-matrix.fish OUTDIR 120 pipewire alsa jack`; every RESULT line shows `xruns=0` |
| 3 | Load benchmark, zero xruns | validator | `fish crates/engine/bench/load-matrix.fish OUTDIR 120`; zero xruns in every row |
| 4 | UX replay | ux-qa | Replay the owner's flows and the "Make Your First Beat" tutorial on the Flatpak build; report has no open blocker or major issue |
| 5 | THIRD_PARTY.md and ASSETS.md complete | main | `fish tools/third-party.fish /tmp/tp.md; diff THIRD_PARTY.md /tmp/tp.md`; `fish tools/check-spdx.fish` |
| 6 | Flatpak builds offline | packaging | step 2 below, with the sources file generated from the release `Cargo.lock` |
| 7 | Runs on a clean GNOME install | packaging | step 3 below (VM or container with only GNOME 50 runtime installed) |
| 8 | Metadata valid | packaging | `desktop-file-validate data/*.desktop`; `appstreamcli validate --pedantic data/*.metainfo.xml` (with network: remove `--no-net`, screenshots must resolve) |

## 2. Build the bundle (packaging)

1. Set the version: `[workspace.package] version = "0.1.0"` in `Cargo.toml`, the
   `<release>` entry in `data/io.github.luohoa97.Oto.metainfo.xml`
   (version and date), `cargo update --workspace` offline check. Commit.
2. Replace the local `libredaw-sounds` dir source in
   `packaging/flatpak/io.github.luohoa97.Oto.yml` with a public git URL, a
   tag and the commit hash (`type: git`). Replace the `libredaw` (Oto) dir source with
   `type: git` at the release tag too, so the bundle is reproducible.
3. Put the screenshots in `docs/screenshots/{home,pattern,mixer}.png`
   (main, after the UI gate), and check the URLs in the metainfo.
4. Build:

       fish packaging/flatpak/cargo-sources.fish
       flatpak-builder --user --install-deps-from=flathub --force-clean \
           --repo=packaging/flatpak/repo packaging/flatpak/build-dir \
           packaging/flatpak/io.github.luohoa97.Oto.yml
       flatpak build-bundle packaging/flatpak/repo Oto-0.1.0.flatpak \
           io.github.luohoa97.Oto
       sha256sum Oto-0.1.0.flatpak > Oto-0.1.0.flatpak.sha256

   For the offline check, run the second command a second time with the network
   disabled (`unshare -rn` or a firewalled VM) after sources were downloaded
   once; the build must not touch the network.

## 3. Clean-install test (packaging)

In a VM or container with only the GNOME 50 runtime (no other Oto
dependencies):

    flatpak install --user ./Oto-0.1.0.flatpak
    OTO_DEBUG=1 flatpak run io.github.luohoa97.Oto
    flatpak run --command=oto-mcp io.github.luohoa97.Oto --version
    flatpak uninstall --user io.github.luohoa97.Oto

Check: the Home page opens, a template plays sound through PipeWire, the debug
output reports real-time priority (or the documented fallback), scripting shows
its "not available in the Flatpak" message.

## 4. Tag and publish (main, with the owner)

1. Merge all branches to `main`; confirm gate rows 1 to 8 are recorded in the
   release issue.
2. Signed tag (owner's key): `git tag -s v0.1.0 -m "Oto 0.1.0"`;
   `git tag -v v0.1.0`.
3. `git push origin main v0.1.0` (owner approves the push).
4. GitHub release: `gh release create v0.1.0 Oto-0.1.0.flatpak
   Oto-0.1.0.flatpak.sha256 --title "Oto 0.1.0" --notes-file
   packaging/RELEASE-NOTES-0.1.0.md`.
5. Flathub submission is a separate, later owner decision.

# Known limits (stated in the notes)

Host CLAP plugins in `/usr/lib/clap` are not visible from the Flatpak; scripting
is disabled in the Flatpak; `.oto` bundles show as folders in GNOME Files.
