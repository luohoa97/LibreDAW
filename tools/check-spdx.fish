#!/usr/bin/env fish
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Checks that every tracked file except LICENSE carries
# "SPDX-License-Identifier: GPL-3.0-or-later" in its first 10 lines.
# Files that cannot hold a comment (binary assets, JSON) must be listed in ASSETS.md.
# (SPEC.md section 2. The REUSE tool is not used because it is Python.)

cd (path resolve (status dirname)/..); or exit 2

set -l id 'SPDX-License-Identifier: GPL-3.0-or-later'
set -l bad 0
set -l checked 0

for f in (git ls-files)
    test $f = LICENSE; and continue
    test -f $f; or continue # deleted in the working tree but not yet staged
    set checked (math $checked + 1)
    if test -s $f; and not grep -Iq . -- $f
        # binary file: must be listed in ASSETS.md
        if not grep -qF -- "`$f`" ASSETS.md
            echo "check-spdx: binary file not listed in ASSETS.md: $f"
            set bad (math $bad + 1)
        end
        continue
    end
    # comment-less text formats (JSON) may also be listed in ASSETS.md instead
    if string match -q -- "*.json" $f; and grep -qF -- "`$f`" ASSETS.md
        continue
    end
    if not head -n 10 -- $f | grep -qF -- $id
        echo "check-spdx: missing SPDX identifier: $f"
        set bad (math $bad + 1)
    end
end

if test $bad -gt 0
    echo "check-spdx: $bad file(s) failed ($checked checked)"
    exit 1
end
echo "check-spdx: ok ($checked files)"
