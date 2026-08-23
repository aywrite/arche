#!/usr/bin/env bash
# Count the bench of every commit between two that states one, and compare.
#
#     verify_bench.sh <base> <head>
#
# Each commit in base..head with a Bench trailer is checked out and built,
# its bench run, and the count compared with the one stated. The trailer is
# read the way git reads it, so a line the changelog would not take is not
# one here either. Every commit is reported, a build or a bench that fails
# counts as a mismatch and the walk goes on, and the checkout is put back
# where it started. The count is exact and the same on any machine, which is
# what lets this gate where a timing could not.
set -euo pipefail

base=${1:?usage: verify_bench.sh <base> <head>}
head=${2:?usage: verify_bench.sh <base> <head>}
bin="${CARGO_TARGET_DIR:-target}/release/arche"
# a branch to come back to, or the commit when there is none
start=$(git symbolic-ref -q --short HEAD || git rev-parse HEAD)

failed=0
for sha in $(git rev-list --reverse "${base}..${head}"); do
    subject=$(git log -1 --format=%s "$sha")
    # git prints the values and a blank line after them, so the last value
    # is the last line with anything on it
    stated=$(git log -1 --format='%(trailers:key=Bench,valueonly)' "$sha" | awk 'NF { value = $0 } END { print value }')
    if [ -z "$stated" ]; then
        echo "${sha:0:7} ${subject}: no bench stated"
        continue
    fi
    git checkout -q --detach "$sha"
    if ! cargo build --release --quiet --locked; then
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
git checkout -q "$start"
exit "$failed"
