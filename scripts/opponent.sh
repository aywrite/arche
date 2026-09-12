#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2022-2026 Andrew Wright

# Build an engine the calibration gauntlet plays against:
#
#     opponent.sh list                          every engine named below
#     opponent.sh repository <engine>           where it is cloned from
#     opponent.sh build <engine> <pin> <binary> clone it, build it, put it there
#
# A pin is a tag or a whole commit sha, and nothing else: a name that is not
# forty hex digits is fetched as refs/tags/<pin>, so a branch cannot be pinned
# by name. The pin matters as much as the engine does, because a rating on the
# ccrl list belongs to the exact version it names and a pin at any other
# revision is a different engine as far as that number is concerned. A branch
# would move under the rating; a tag can still be moved by whoever owns the
# repository, which is a smaller risk and one nothing here can see. Everything
# is fetched at a depth of one, so only that revision arrives and a repository
# with a long history costs no more than a short one.
#
# One engine is one block below: where it comes from, how it is built from the
# root of that clone, and where the build leaves its binary. Adding an opponent
# is adding a block. The names the blocks declare are what the workflow checks
# a ladder against, so an engine nobody has written a block for is refused
# before any match is played rather than at the build.
#
# The blocks are as close to the project's own instructions as a build on a
# runner can be. Where they are not, the comment says why: an old source that a
# current compiler will not take is given a flag rather than an edit, so that
# what is played is the release at its pin and not a patched version of it.
set -euo pipefail

# The table. A block is a repository here and a build below it, and a name is
# in the list a ladder is checked against only when it has both.
declare -A REPOSITORY

# Stash, the ladder these matches began with. A plain makefile that moved into
# src/ after v12, which is why the directory is looked for rather than named.
REPOSITORY[stash]=https://github.com/mhouppin/stash-bot.git
stash_build() {
    local directory=.
    if [ -f src/Makefile ]; then
        directory=src
    fi
    make -C "$directory" -j"$(nproc)" > /dev/null
    echo "${directory}/stash-bot"
}

# BBC, Code Monkey King's bitboard engine. One source file per release, kept
# beside its predecessors, so the pin names the file as well as the revision.
# The first release is the exception and calls it bbc.c, which is why the name
# is keyed to the pin rather than looked for: a pin whose file is missing is a
# pin whose version this would otherwise guess at. It declares a maximum hash
# of 128MB, under the 256MB the match asks for, so it plays on its own default.
REPOSITORY[bbc]=https://github.com/maksimKorzh/bbc.git
bbc_build() {
    local source="src/bbc_${1}.c"
    if [ "$1" = 1.0 ]; then
        source=src/bbc.c
    fi
    gcc -O3 -o bbc "$source" -lm
    echo bbc
}

# Bad Chess Engine. Its sources declare their globals in a header that every
# unit includes, which linked before gcc took -fno-common as its default, so
# it is given the old default rather than an edit. Its cmake file names the
# same set of files this line compiles.
REPOSITORY[badchessengine]=https://github.com/Notoh/badchessengine.git
badchessengine_build() {
    gcc -std=c99 -O3 -fcommon -o badchessengine ./*.c -lm
    echo badchessengine
}

# Zagreus, built by its own cmake file. Its release build asks for a plain
# x86-64 rather than the machine it is on, which is what a runner wants.
REPOSITORY[zagreus]=https://github.com/Dannyj1/Zagreus.git
zagreus_build() {
    cmake -S . -B build -DCMAKE_BUILD_TYPE=Release > /dev/null
    cmake --build build -j"$(nproc)" > /dev/null
    echo build/Zagreus
}

# Goldfish, a rust engine built by cargo from the workspace root. Its makefile
# runs the same build and then copies the binary, which the caller does here.
REPOSITORY[goldfish]=https://github.com/bsamseth/Goldfish.git
goldfish_build() {
    cargo build --release --quiet --bin goldfish
    echo target/release/goldfish
}

# SoFCheck, built by its own cmake file. Its generators are run during the
# build and want python3, which the runner has. jsoncpp and cxxopts are
# vendored in third-party/, so a shallow clone brings everything it needs.
REPOSITORY[sofcheck]=https://github.com/alex65536/sofcheck.git
sofcheck_build() {
    cmake -S . -B build -DCMAKE_BUILD_TYPE=Release > /dev/null
    cmake --build build -j"$(nproc)" --target sofcheck > /dev/null
    echo build/sofcheck
}

# Tantabus, a rust workspace whose uci crate is the binary. Its makefile asks
# for target-cpu=native, which is dropped here for the same reason the zagreus
# block takes a plain x86-64: what a runner builds should not depend on which
# machine picked it up.
REPOSITORY[tantabus]=https://github.com/analog-hors/tantabus.git
tantabus_build() {
    cargo build --release --quiet -p tantabus-uci
    echo target/release/tantabus-uci
}

# Cinnamon. The cmake file beside its sources builds a debug target against
# clang and gtest, so the release build is the project's own makefile, which
# names a target per instruction set. The generic one is the plain x86-64.
# Weiss, Terje Kirstihagen's engine in c. Fathom is vendored beside its own
# sources rather than fetched, so a shallow clone holds everything the makefile
# compiles. The default target is the one openbench builds, and its flags name
# the machine they are built on, which is dropped here for the same reason the
# zagreus and tantabus blocks drop it: what plays should be the release the
# rating belongs to and not a build tuned to whichever runner picked it up.
# Popcount is kept because the version the list rates is a popcount build, and
# pext is not, since it asks for bmi2 the runner may not have. This target
# leaves the binary beside the sources rather than in ../bin.
REPOSITORY[weiss]=https://github.com/TerjeKir/weiss.git
weiss_build() {
    make -C src bench-basic CFLAGS="-std=gnu11 -O3 -flto -msse3 -mpopcnt" > /dev/null
    echo src/weiss
}

REPOSITORY[cinnamon]=https://github.com/gekomad/Cinnamon.git
cinnamon_build() {
    make -C src cinnamon64-generic -j"$(nproc)" > /dev/null
    echo src/cinnamon
}

# FoxSEE, a single rust crate with no dependencies. It declares a maximum hash
# of 512MB, which covers the 256MB the match asks for.
REPOSITORY[foxsee]=https://github.com/redsalmon91/FoxSEE.git
foxsee_build() {
    cargo build --release --quiet
    echo target/release/foxsee
}

# A block is a repository and a build, and an engine with one and not the other
# is not listed. The workflow checks a ladder against this list, so a half
# added engine is refused where the ladder is read rather than after the clone.
list() {
    local engine
    for engine in "${!REPOSITORY[@]}"; do
        if declare -F "${engine}_build" > /dev/null; then
            printf '%s\n' "$engine"
        fi
    done | sort
}

known() {
    local engine=$1
    if [ -z "${REPOSITORY[$engine]+named}" ] \
        || ! declare -F "${engine}_build" > /dev/null; then
        echo "opponent.sh: no engine named ${engine}," \
            "the ladder can name $(list | tr '\n' ' ')" >&2
        exit 1
    fi
}

repository() {
    known "$1"
    echo "${REPOSITORY[$1]}"
}

build() {
    local engine=$1 pin=$2 binary=$3
    known "$engine"
    # the clone is thrown away however this ends, which set -e makes a trap
    # rather than a last line: a failed fetch or build has no other way out.
    # WORK is not local because the trap runs after this function has returned
    WORK=$(mktemp -d)
    trap 'rm -rf "$WORK"' EXIT
    # A whole sha is asked for as it is, and anything else as a tag. Asking for
    # a bare name would fetch a branch of that name just as happily, and a
    # branch moves while the rating on the rung playing it does not. An
    # abbreviated sha is refused by the protocol, which asks by name.
    local ref=refs/tags/$pin
    if [ "${#pin}" = 40 ] && [ -z "${pin//[0-9a-f]/}" ]; then
        ref=$pin
    fi
    (
        cd "$WORK"
        git init -q .
        git remote add origin "$(repository "$engine")"
        git fetch -q --depth 1 origin "$ref" \
            || { echo "opponent.sh: ${engine} has no ${ref}" >&2; exit 1; }
        git checkout -q FETCH_HEAD
        built=$("${engine}_build" "$pin")
        [ -f "$built" ] \
            || { echo "opponent.sh: the ${engine} build left no ${built}" >&2; exit 1; }
        mv "$built" built
    )
    mkdir -p "$(dirname "$binary")"
    cp "${WORK}/built" "$binary"
}

command=${1:-}
case "$command" in
    list) list ;;
    repository) repository "${2:?usage: opponent.sh repository <engine>}" ;;
    build)
        build "${2:?usage: opponent.sh build <engine> <pin> <binary>}" \
            "${3:?usage: opponent.sh build <engine> <pin> <binary>}" \
            "${4:?usage: opponent.sh build <engine> <pin> <binary>}"
        ;;
    *) echo "usage: opponent.sh list|repository|build" >&2; exit 1 ;;
esac
