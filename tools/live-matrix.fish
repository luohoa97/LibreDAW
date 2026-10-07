#!/usr/bin/env fish
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Phase 1 live measurements, sequential, release build, gain 0, 48 kHz.
#
# Usage: tools/live-matrix.fish OUTDIR [SECONDS] [HOST...]
#   SECONDS  per run, default 120
#   HOST     any of pipewire, alsa, jack; default: pipewire
#
# One log per run in OUTDIR; each RESULT line is also echoed to stdout, so
# `tools/live-matrix.fish DIR > DIR/matrix.out` can be followed with tail -f.
#
# pipewire and jack: the graph quantum decides the callback size and cpal
# cannot change it for JACK, so for each run the script sets PipeWire's
# clock.force-quantum to the buffer size and resets it to 0 afterwards, also
# on exit or interrupt. That is a temporary change to the running session,
# not a file. jack runs go through `pw-jack` (PipeWire's libjack).
# alsa goes through pipewire-alsa and needs no forced quantum.

cd (path resolve (status dirname)/..); or exit 2
test (count $argv) -ge 1; or begin
    echo "usage: tools/live-matrix.fish OUTDIR [SECONDS] [HOST...]"
    exit 2
end
set -l out $argv[1]
set -l seconds 120
test (count $argv) -ge 2; and set seconds $argv[2]
set -l hosts pipewire
test (count $argv) -ge 3; and set hosts $argv[3..-1]
mkdir -p $out

function reset_quantum --on-event fish_exit
    pw-metadata -n settings 0 clock.force-quantum 0 >/dev/null 2>&1
end

# cpal's pipewire feature builds pipewire-sys with bindgen, which needs libclang
if not set -q LIBCLANG_PATH; and command -sq llvm-config
    set -gx LIBCLANG_PATH (llvm-config --libdir)
end
cargo build --release --locked -p libredaw-engine; or exit 1
set -l bin target/release/metronome

for host in $hosts
    for buf in 64 128 256 512
        set -l rc 0
        set -l log $out/$host-$buf.log
        echo (date +%T) "start $host $buf ($seconds s)"
        if test $host = alsa
            $bin --host alsa --buffer $buf --seconds $seconds --rate 48000 --bpm 120 --gain 0 >$log 2>&1
            set rc $status
        else
            pw-metadata -n settings 0 clock.force-quantum $buf >/dev/null
            sleep 2
            if test $host = jack
                pw-jack $bin --host jack --buffer $buf --seconds $seconds --rate 48000 --bpm 120 --gain 0 >$log 2>&1
            else
                $bin --host $host --buffer $buf --seconds $seconds --rate 48000 --bpm 120 --gain 0 >$log 2>&1
            end
            set rc $status
            pw-metadata -n settings 0 clock.force-quantum 0 >/dev/null
        end
        echo (date +%T) "done $host $buf: exit $rc"
        grep -h '^RESULT' $log
        sleep 3
    end
end
echo (date +%T) "matrix finished"
