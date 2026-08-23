#!/usr/bin/env bash
# Measure the tree as it stands against the commit it will be made on top
# of, head by default, and print the Speed trailer for a perf commit:
#
#     git commit --trailer "$(scripts/bench_trailer.sh)" \
#                --trailer "$(scripts/speed.sh | tail -n 1)"
#
# Run before the commit exists, which is why the base is head and not its
# parent. The base commit is built once into target/speed/<sha>/ and kept,
# so measuring again against the same commit costs only the rounds. The
# tree is built as it stands, not as it is staged. ROUNDS sets how many
# rounds there are, five by default.
set -euo pipefail

base=$(git rev-parse --verify "${1:-HEAD}^{commit}")
short=$(git rev-parse --short "$base")
dir="target/speed/${short}"

if [ ! -x "${dir}/release/arche" ]; then
    mkdir -p "${dir}/src"
    git archive "$base" | tar -x -C "${dir}/src"
    # the target dir is the commit's own, a level up from its source
    (cd "${dir}/src" && cargo build --release --quiet --target-dir ..)
fi
cargo build --release --quiet

python3 scripts/speed.py "${dir}/release/arche" \
    "${CARGO_TARGET_DIR:-target}/release/arche" \
    --base-ref "$short" --rounds "${ROUNDS:-5}"
