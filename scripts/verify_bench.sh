#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2022-2026 Andrew Wright

# Count the bench of every commit between two that states one, and compare.
#
#     verify_bench.sh <base> <head>
#
# Each commit in base..head with a Bench trailer is built by build_at.sh from
# an export, its bench run and the count compared with the one stated. The
# trailer is read the way git reads it. Every commit is reported; a build or
# a bench that fails counts as a mismatch and the walk goes on. The count is
# exact on any machine, which is what lets this gate where a timing could not.
set -euo pipefail

base=${1:?usage: verify_bench.sh <base> <head>}
head=${2:?usage: verify_bench.sh <base> <head>}
bin="${CARGO_TARGET_DIR:-target}/at/arche"

failed=0
for sha in $(git rev-list --reverse "${base}..${head}"); do
    subject=$(git log -1 --format=%s "$sha")
    # the last non-blank value: git prints a blank line after the values
    stated=$(git log -1 --format='%(trailers:key=Bench,valueonly)' "$sha" | awk 'NF { value = $0 } END { print value }')
    if [ -z "$stated" ]; then
        echo "${sha:0:7} ${subject}: no bench stated"
        continue
    fi
    if ! "$(dirname "$0")/build_at.sh" "$sha" "$bin"; then
        echo "${sha:0:7} ${subject}: bench stated ${stated}, build failed"
        failed=1
        continue
    fi
    if ! counted=$("$bin" bench | tail -n 1 | cut -d' ' -f1) || [ -z "$counted" ]; then
        counted=nothing
    fi
    if [ "$counted" = "$stated" ]; then
        echo "${sha:0:7} ${subject}: bench ${stated} ok"
    else
        echo "${sha:0:7} ${subject}: bench stated ${stated}, counted ${counted}"
        failed=1
    fi
done
exit "$failed"
