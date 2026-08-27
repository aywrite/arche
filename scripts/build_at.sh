#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2022-2026 Andrew Wright

# Build the engine as it was at a commit, and put the binary where asked:
#
#     build_at.sh <ref> <binary>
#
# The commit is exported with git archive into <target>/at/src, where
# <target> is CARGO_TARGET_DIR or target/, and built with <target>/at as its
# target directory. The working tree is never checked out: nothing here
# moves the branch, the index or a file someone is half way through, and a
# script building one commit to measure against another has nothing to put
# back afterwards.
#
# The export has a target directory of its own, and its files are stamped
# with the time they were extracted rather than the commit's. Both are what
# keep cargo honest. Cargo tells a crate fresh by its sources being older
# than its last build, and names a workspace crate's build by the crate and
# not by where it was built from: so an export stamped with the commit's
# time looks older than whatever was built last and is handed that binary
# back, and an export sharing the tree's target directory is taken for the
# tree. The engine is built afresh for every commit; what the target
# directory keeps across calls is the dependencies.
#
# No --locked: a baseline old enough that its lock file predates a registry
# change would refuse to build, and the pull request's own tree is held to
# its lock file by the Rust workflow.
set -euo pipefail

ref=${1:?usage: build_at.sh <ref> <binary>}
binary=${2:?usage: build_at.sh <ref> <binary>}

sha=$(git rev-parse --verify "${ref}^{commit}") \
    || { echo "build_at.sh: ${ref} is not a commit" >&2; exit 1; }
# absolute, because the build runs from inside it
target=$(realpath -m "${CARGO_TARGET_DIR:-target}")/at
src="${target}/src"

rm -rf "$src"
mkdir -p "$src"
git archive "$sha" | tar -xm -C "$src"
(cd "$src" && cargo build --release --quiet --target-dir "$target")
mkdir -p "$(dirname "$binary")"
cp "${target}/release/arche" "$binary"
