#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2022-2026 Andrew Wright

# Build the engine as it was at a commit, and put the binary where asked:
#
#     build_at.sh <ref> <binary> [built [build command...]]
#
# The commit is exported with git archive into <target>/at/src, where
# <target> is CARGO_TARGET_DIR or target/, and built with <target>/at as its
# target directory. The working tree is never checked out, so nothing here
# moves the branch, the index or a half written file.
#
# The build command is the rest of the line rather than one string, so
# nothing here has to split or eval it. It is given CARGO_TARGET_DIR, so a
# cargo command lands in the export's target directory whether or not it
# says --target-dir; a build that is not cargo has to leave its binary where
# <built> says. <built> is read from the root of the export, which is where
# the build runs, so the default ../release/arche names the export's parent.
#
# The export has a target directory of its own, and its files are stamped
# with the time they were extracted rather than the commit's. Both keep cargo
# honest: cargo tells a crate fresh by its sources being older than its last
# build, and names a workspace crate's build by the crate, so an export
# stamped with the commit's time is handed the last build back, and an export
# sharing the tree's target directory is taken for the tree. What the target
# directory keeps across calls is the dependencies.
#
# Where the export lands decides which .cargo/config.toml it is built under,
# because cargo finds a config by walking up from where it builds. Inside the
# tree, which is where the default target directory puts it, the export is
# built the way the tree is; under a CARGO_TARGET_DIR outside the tree it is
# built the way its commit was. A comparison spanning a change to
# .cargo/config.toml reads about zero from inside the tree and reads the
# change from outside it.
#
# No --locked in the default command: a baseline old enough that its lock
# file predates a registry change would refuse to build, and the pull
# request's own tree is held to its lock file by the Rust workflow.
set -euo pipefail

ref=${1:?usage: build_at.sh <ref> <binary> [built [build command...]]}
binary=${2:?usage: build_at.sh <ref> <binary> [built [build command...]]}
built=${3:-../release/arche}
build=("${@:4}")

sha=$(git rev-parse --verify "${ref}^{commit}") \
    || { echo "build_at.sh: ${ref} is not a commit" >&2; exit 1; }
# absolute, because the build runs from inside it
target=$(realpath -m "${CARGO_TARGET_DIR:-target}")/at
src="${target}/src"

# defaulted here because it names the target directory. An empty argument is
# an argument nobody gave
if [ "${#build[@]}" -eq 0 ]; then
    build=(cargo build --release --quiet --target-dir "$target")
fi

rm -rf "$src"
mkdir -p "$src"
git archive "$sha" | tar -xm -C "$src"
(cd "$src" && CARGO_TARGET_DIR="$target" "${build[@]}")
[ -f "${src}/${built}" ] \
    || { echo "build_at.sh: the build left no ${built}" >&2; exit 1; }
mkdir -p "$(dirname "$binary")"
cp "${src}/${built}" "$binary"
