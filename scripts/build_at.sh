#!/usr/bin/env bash
# Build the engine as it was at a commit, and put the binary where asked:
#
#     build_at.sh <ref> <binary>
#
# The commit is exported with git archive into <target>/at/src, where
# <target> is CARGO_TARGET_DIR or target/, and built there with <target> as
# its target directory. The working tree is never checked out: nothing here
# moves the branch, the index or a file someone is half way through, and a
# script building one commit to measure against another has nothing to put
# back afterwards. The source path is the same for every commit and the
# target directory is shared, so cargo keeps what it built last time, the
# dependencies always and the engine when the commit left it alone, the way
# building in a checkout would.
set -euo pipefail

ref=${1:?usage: build_at.sh <ref> <binary>}
binary=${2:?usage: build_at.sh <ref> <binary>}

sha=$(git rev-parse --verify --quiet "${ref}^{commit}") \
    || { echo "build_at.sh: ${ref} is not a commit" >&2; exit 1; }
# absolute, because the build runs from inside it
target=$(realpath -m "${CARGO_TARGET_DIR:-target}")
src="${target}/at/src"

rm -rf "$src"
mkdir -p "$src"
git archive "$sha" | tar -x -C "$src"
(cd "$src" && cargo build --release --quiet --locked --target-dir "$target")
mkdir -p "$(dirname "$binary")"
cp "${target}/release/arche" "$binary"
