#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2022-2026 Andrew Wright

# Measure the tree as it stands against the commit it will be made on top
# of, head by default, and print the Speed trailer for a perf commit:
#
#     git commit --trailer "$(scripts/bench_trailer.sh)" \
#                --trailer "$(scripts/speed.sh | tail -n 1)"
#
# Run before the commit exists, which is why the base is head and not its
# parent. The tree is built as it stands, not as it is staged. ROUNDS sets
# the rounds, fifteen by default.
#
# Each round runs both sides on a layout of its own, which layouts.sh links
# from one build a side. LAYOUTS picks the kind: shuffle by default, pad for
# a change whose point is where its functions sit (it keeps their order and
# moves only where the code starts) or for a link that is not lld's, or off
# for one binary a side. The base's
# layouts are kept under target/speed/<sha>-<kind>/, or its binary under
# target/speed/<sha>/ when off, so measuring again against the same commit
# costs only the rounds.
set -euo pipefail

base=$(git rev-parse --verify "${1:-HEAD}^{commit}")
short=$(git rev-parse --short "$base")
target="${CARGO_TARGET_DIR:-target}"
rounds=${ROUNDS:-15}
layouts=${LAYOUTS:-shuffle}
here=$(dirname "$0")

if [ "$layouts" = off ]; then
    kept="${target}/speed/${short}/arche"
    if [ ! -x "$kept" ]; then
        "${here}/build_at.sh" "$base" "$kept"
    fi
    cargo build --release --quiet
    candidate="${target}/release/arche"
else
    # one a round: speed.py runs no round again over layouts
    count=$rounds
    kept="${target}/speed/${short}-${layouts}"
    if [ ! -x "${kept}/${count}" ] || [ "$(cat "${kept}/mode" 2>/dev/null)" != "$layouts" ]; then
        "${here}/layouts.sh" "$base" "$kept" "$count" "$layouts"
    fi
    candidate="${target}/speed/tree-${layouts}"
    "${here}/layouts.sh" --tree "$candidate" "$count" "$layouts"
fi

python3 "${here}/speed.py" "$kept" "$candidate" --base-ref "$short" --rounds "$rounds"
