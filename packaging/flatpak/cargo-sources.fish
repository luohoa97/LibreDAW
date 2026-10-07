#!/usr/bin/env fish
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Generates the flatpak-builder sources file for offline Cargo builds from
# Cargo.lock (SPEC.md section 19.3). Replaces flatpak-cargo-generator (Python).
#
# Usage: packaging/flatpak/cargo-sources.fish [Cargo.lock] [output.json]
#   defaults: <repo>/Cargo.lock and packaging/flatpak/cargo-sources.json
#
# For every crates.io package in the lockfile it emits:
#   - an archive source (static.crates.io .crate, sha256 from Cargo.lock)
#     extracted to cargo/vendor/<name>-<version>
#   - an inline .cargo-checksum.json beside it ({"package": sha256, "files": {}})
# plus cargo/config.toml, which points crates-io at the vendor directory.
# Git dependencies are not supported (the project has none) and make the
# script fail loudly. Build with CARGO_HOME=/run/build/<module>/cargo.

set -l root (path resolve (status dirname)/../..)
set -l lock $root/Cargo.lock
set -l out $root/packaging/flatpak/cargo-sources.json
test (count $argv) -ge 1; and set lock $argv[1]
test (count $argv) -ge 2; and set out $argv[2]
test -f $lock; or begin
    echo "cargo-sources: no such file: $lock" >&2
    exit 2
end

set -g __entries
set -g __count 0
set -g __name
set -g __version
set -g __source
set -g __checksum

function __emit_package
    test -n "$__name"; or return
    test -n "$__source"; or return # workspace member
    if not string match -q 'registry+https://github.com/rust-lang/crates.io-index*' -- $__source
        echo "cargo-sources: unsupported source for $__name $__version: $__source" >&2
        exit 1
    end
    if test -z "$__checksum"
        echo "cargo-sources: no checksum for $__name $__version" >&2
        exit 1
    end
    set -l dest cargo/vendor/$__name-$__version
    set -l url https://static.crates.io/crates/$__name/$__name-$__version.crate
    set -ga __entries '  {"type": "archive", "archive-type": "tar-gzip", "url": "'$url'", "sha256": "'$__checksum'", "dest": "'$dest'"}'
    set -ga __entries '  {"type": "inline", "contents": "{\"package\": \"'$__checksum'\", \"files\": {}}", "dest": "'$dest'", "dest-filename": ".cargo-checksum.json"}'
    set -g __count (math $__count + 1)
end

function __val
    string match -r '"([^"]*)"' -- $argv[1] | tail -n 1
end

for line in (cat $lock)
    switch $line
        case '[[package]]'
            __emit_package
            set -g __name
            set -g __version
            set -g __source
            set -g __checksum
        case 'name = *'
            set -g __name (__val $line)
        case 'version = *'
            set -g __version (__val $line)
        case 'source = *'
            set -g __source (__val $line)
        case 'checksum = *'
            set -g __checksum (__val $line)
    end
end
__emit_package

set -l config '[source.crates-io]\\nreplace-with = \\"vendored-sources\\"\\n\\n[source.vendored-sources]\\ndirectory = \\"cargo/vendor\\"\\n'
set -ga __entries '  {"type": "inline", "contents": "'$config'", "dest": "cargo", "dest-filename": "config.toml"}'

begin
    echo '['
    set -l n (count $__entries)
    for i in (seq $n)
        if test $i -lt $n
            echo "$__entries[$i],"
        else
            echo $__entries[$i]
        end
    end
    echo ']'
end >$out
echo "cargo-sources: $__count crates -> $out" >&2
