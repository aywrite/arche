#!/usr/bin/env bash
# Print the Bench trailer for the tree as it stands, for an engine commit:
#
#     git commit --trailer "$(scripts/bench_trailer.sh)"
#
# Builds the release binary first, unless ARCHE names one already built, from
# the tree as it stands rather than as it is staged. The number is the
# bench's last line, which is the count the hook checks and the one openbench
# reads.
set -euo pipefail

if [ -z "${ARCHE:-}" ]; then
    cargo build --release --quiet
    # where cargo put it, which CARGO_TARGET_DIR moves
    ARCHE="${CARGO_TARGET_DIR:-target}/release/arche"
fi

nodes=$("$ARCHE" bench | tail -n 1 | cut -d' ' -f1)
if ! [ "$nodes" -gt 0 ] 2>/dev/null; then
    echo "bench_trailer.sh: no node count in the output of $ARCHE bench" >&2
    exit 1
fi
echo "Bench: ${nodes}"
