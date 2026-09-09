#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2022-2026 Andrew Wright

# Build the engine as it was at a commit, and put the binary where asked:
#
#     build_at.sh <ref> <binary> [built [build command...]]
#
# The commit is exported with git archive into <target>/at/src, where
# <target> is CARGO_TARGET_DIR or target/, and built with <target>/at as its
# target directory. The working tree is never checked out: nothing here
# moves the branch, the index or a file someone is half way through, and a
# script building one commit to measure against another has nothing to put
# back afterwards.
#
# How the export is built, and where that build leaves its binary, are the
# last two arguments. Both default to what this repository builds with, so a
# caller naming neither gets what this script has always done. They are
# arguments rather than a table keyed by engine, because the command belongs
# to whoever is calling rather than to a list this script would have to be
# told about, and the command is the rest of the line rather than one string
# so that nothing here has to split or eval it.
#
# The build is given CARGO_TARGET_DIR, so a cargo command lands in the
# export's target directory whether or not it says --target-dir. The default
# command says it as well, which is one directory named twice and not two. A
# build that is not cargo ignores the variable and has to leave its binary
# where <built> says.
#
# <built> is read from the root of the export, which is where the build runs.
# A build writing into the target directory names it from there rather than
# by an absolute path nobody outside this script can compose: that directory
# is the export's parent, which is what the default ../release/arche says.
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
# No --locked in the default command: a baseline old enough that its lock
# file predates a registry change would refuse to build, and the pull
# request's own tree is held to its lock file by the Rust workflow. A caller
# naming a command of its own answers that for itself.
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

# The build command's default is taken here rather than beside its argument,
# because the target directory it names is only known once CARGO_TARGET_DIR
# has been read. An argument left empty is an argument nobody gave.
if [ "${#build[@]}" -eq 0 ]; then
    build=(cargo build --release --quiet --target-dir "$target")
fi

rm -rf "$src"
mkdir -p "$src"
git archive "$sha" | tar -xm -C "$src"
(cd "$src" && CARGO_TARGET_DIR="$target" "${build[@]}")
# A build that put its binary somewhere else says so here rather than as
# whatever cp makes of a path that is not there.
[ -f "${src}/${built}" ] \
    || { echo "build_at.sh: the build left no ${built}" >&2; exit 1; }
mkdir -p "$(dirname "$binary")"
cp "${src}/${built}" "$binary"
