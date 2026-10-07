#!/usr/bin/env fish
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Local and CI gate. Stops at the first failing step.

cd (path resolve (status dirname)/..); or exit 2

# cpal's pipewire feature builds pipewire-sys with bindgen, which needs libclang
if not set -q LIBCLANG_PATH; and command -sq llvm-config
    set -gx LIBCLANG_PATH (llvm-config --libdir)
end

function step
    echo "==> $argv"
    $argv
    or begin
        echo "ci: FAILED: $argv"
        exit 1
    end
end

function third_party_current
    set -l tmp (mktemp)
    tools/third-party.fish $tmp; or begin
        rm -f $tmp
        return 1
    end
    diff -u THIRD_PARTY.md $tmp
    set -l rc $status
    rm -f $tmp
    test $rc -eq 0; or echo "ci: THIRD_PARTY.md is out of date; run tools/third-party.fish"
    return $rc
end

step cargo fmt --check
step cargo clippy --workspace --all-targets --locked -- -D warnings
step cargo test --workspace --locked
step cargo deny check
step tools/check-spdx.fish
step third_party_current
echo "ci: all steps passed"
