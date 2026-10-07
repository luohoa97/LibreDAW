#!/usr/bin/env fish
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Phase 1 live measurements: ALSA and JACK at 64/128/256/512 frames,
# sequentially, release build, gain 0. One log file per run.
#
# Usage: tools/live-matrix.fish OUTDIR [SECONDS]      (default 600 s per run)
#
# JACK runs go through `pw-jack` (PipeWire's libjack) because cpal cannot
# change the JACK buffer size and rejects a mismatch. So for each JACK run
# the script sets PipeWire's clock.force-quantum to the buffer size and
# resets it to 0 afterwards, also on exit or interrupt. That is a temporary
# change to the running session, not a file.

cd (path resolve (status dirname)/..); or exit 2
test (count $argv) -ge 1; or begin
    echo "usage: tools/live-matrix.fish OUTDIR [SECONDS]"
    exit 2
end
set -l out $argv[1]
set -l seconds 600
test (count $argv) -ge 2; and set seconds $argv[2]
mkdir -p $out

function reset_quantum --on-event fish_exit
    pw-metadata -n settings 0 clock.force-quantum 0 >/dev/null 2>&1
end

cargo build --release --locked -p libredaw-engine; or exit 1
set -l bin target/release/metronome

for host in alsa jack
    for buf in 64 128 256 512
        set -l rc 0
        set -l log $out/$host-$buf.log
        echo (date +%T) "start $host $buf"
        if test $host = jack
            pw-metadata -n settings 0 clock.force-quantum $buf >/dev/null
            sleep 2
            pw-jack $bin --host jack --buffer $buf --seconds $seconds --rate 48000 --bpm 120 --gain 0 >$log 2>&1
            set rc $status
            pw-metadata -n settings 0 clock.force-quantum 0 >/dev/null
        else
            $bin --host alsa --buffer $buf --seconds $seconds --rate 48000 --bpm 120 --gain 0 >$log 2>&1
            set rc $status
        end
        echo (date +%T) "done $host $buf: exit $rc"
        sleep 3
    end
end
echo (date +%T) "matrix finished"
