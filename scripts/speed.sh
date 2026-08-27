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
# parent. The base commit is built once, by build_at.sh, and its binary kept
# under target/speed/<sha>/, so measuring again against the same commit costs
# only the rounds. The tree is built as it stands, not as it is staged.
# ROUNDS sets how many rounds there are, five by default.
set -euo pipefail

base=$(git rev-parse --verify "${1:-HEAD}^{commit}")
short=$(git rev-parse --short "$base")
target="${CARGO_TARGET_DIR:-target}"
kept="${target}/speed/${short}/arche"

if [ ! -x "$kept" ]; then
    "$(dirname "$0")/build_at.sh" "$base" "$kept"
fi
cargo build --release --quiet

python3 scripts/speed.py "$kept" "${target}/release/arche" \
    --base-ref "$short" --rounds "${ROUNDS:-5}"
