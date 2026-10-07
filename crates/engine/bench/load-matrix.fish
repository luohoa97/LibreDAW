#!/usr/bin/env fish
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Milestone B load matrix: the heavy load project through Engine::start on
# PipeWire at buffers 64, 128, 256 and 512, sequential, release build,
# 48 kHz, then an offline render of the same project.
#
# Usage: crates/engine/bench/load-matrix.fish OUTDIR [SECONDS]
#   SECONDS  per live run, default 60
#
# One log per run in OUTDIR; each RESULT line is also echoed to stdout. For
# each live run the script sets PipeWire's clock.force-quantum to the buffer
# size and resets it to 0 afterwards, also on exit or interrupt. That is a
# temporary change to the running session, not a file.

cd (path resolve (status dirname)/../../..); or exit 2
test (count $argv) -ge 1; or begin
    echo "usage: crates/engine/bench/load-matrix.fish OUTDIR [SECONDS]"
    exit 2
end
set -l out $argv[1]
set -l seconds 60
test (count $argv) -ge 2; and set seconds $argv[2]
mkdir -p $out

function reset_quantum --on-event fish_exit
    pw-metadata -n settings 0 clock.force-quantum 0 >/dev/null 2>&1
end
function on_signal --on-signal INT --on-signal TERM --on-signal HUP
    reset_quantum
    exit 130
end

# cpal's pipewire feature builds pipewire-sys with bindgen, which needs libclang
if not set -q LIBCLANG_PATH; and command -sq llvm-config
    set -gx LIBCLANG_PATH (llvm-config --libdir)
end
cargo build --release --locked -p libredaw-engine --bin loadbench; or exit 1
set -l bin target/release/loadbench

for buf in 64 128 256 512
    set -l log $out/pipewire-$buf.log
    echo (date +%T) "start pipewire $buf ($seconds s)"
    pw-metadata -n settings 0 clock.force-quantum $buf >/dev/null
    sleep 2
    $bin --host pipewire --buffer $buf --seconds $seconds --rate 48000 >$log 2>&1
    set -l rc $status
    pw-metadata -n settings 0 clock.force-quantum 0 >/dev/null
    echo (date +%T) "done pipewire $buf: exit $rc"
    grep -h '^RESULT' $log
    sleep 3
end

set -l log $out/offline.log
echo (date +%T) "start offline"
$bin --offline --rate 48000 >$log 2>&1
echo (date +%T) "done offline: exit $status"
grep -h '^RESULT' $log
echo (date +%T) "matrix finished"
