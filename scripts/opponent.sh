#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2022-2026 Andrew Wright

# Build an engine the calibration gauntlet plays against:
#
#     opponent.sh list                          every engine named below
#     opponent.sh repository <engine>           where it is cloned from
#     opponent.sh build <engine> <pin> <binary> clone it, build it, put it there
#
# A pin is a tag or a whole commit sha: a name that is not forty hex digits is
# fetched as refs/tags/<pin>, so a branch cannot be pinned by name. A rating
# on the ccrl list belongs to the exact version it names, and a branch would
# move under it. A tag can still be moved by whoever owns the repository,
# which nothing here can see. Everything is fetched at a depth of one.
#
# One engine is one block below: where it comes from, how it is built from the
# root of that clone, and where the build leaves its binary. The names the
# blocks declare are what the workflow checks a ladder against.
#
# The blocks follow each project's own instructions where a runner can. Where
# they cannot, the comment says why: an old source a current compiler will not
# take is given a flag rather than an edit, so what plays is the release at
# its pin.
set -euo pipefail

# A name is in the list a ladder is checked against only when it has both a
# repository here and a build function below.
declare -A REPOSITORY

# Stash. Its makefile moved into src/ after v12, so the directory is looked
# for rather than named.
REPOSITORY[stash]=https://github.com/mhouppin/stash-bot.git
stash_build() {
    local directory=.
    if [ -f src/Makefile ]; then
        directory=src
    fi
    make -C "$directory" -j"$(nproc)" > /dev/null
    echo "${directory}/stash-bot"
}

# BBC. One source file per release, kept beside its predecessors, so the pin
# names the file as well as the revision; the first release calls it bbc.c.
# It declares a maximum hash of 128MB, under the 256MB the match asks for, so
# it plays on its own default.
REPOSITORY[bbc]=https://github.com/maksimKorzh/bbc.git
bbc_build() {
    local source="src/bbc_${1}.c"
    if [ "$1" = 1.0 ]; then
        source=src/bbc.c
    fi
    gcc -O3 -o bbc "$source" -lm
    echo bbc
}

# Bad Chess Engine. Its globals are declared in a header every unit includes,
# which linked before gcc took -fno-common as its default, so it is given the
# old default rather than an edit. Its cmake file compiles the same files.
REPOSITORY[badchessengine]=https://github.com/Notoh/badchessengine.git
badchessengine_build() {
    gcc -std=c99 -O3 -fcommon -o badchessengine ./*.c -lm
    echo badchessengine
}

# Zagreus, built by its own cmake file. Its release build asks for a plain
# x86-64 rather than the machine it is on.
REPOSITORY[zagreus]=https://github.com/Dannyj1/Zagreus.git
zagreus_build() {
    cmake -S . -B build -DCMAKE_BUILD_TYPE=Release > /dev/null
    cmake --build build -j"$(nproc)" > /dev/null
    echo build/Zagreus
}

# Goldfish, a rust workspace. Its makefile runs this build and copies the
# binary, which the caller does here.
REPOSITORY[goldfish]=https://github.com/bsamseth/Goldfish.git
goldfish_build() {
    cargo build --release --quiet --bin goldfish
    echo target/release/goldfish
}

# SoFCheck, built by its own cmake file. Its generators want python3, and its
# dependencies are vendored in third-party/, so a shallow clone is enough.
REPOSITORY[sofcheck]=https://github.com/alex65536/sofcheck.git
sofcheck_build() {
    cmake -S . -B build -DCMAKE_BUILD_TYPE=Release > /dev/null
    cmake --build build -j"$(nproc)" --target sofcheck > /dev/null
    echo build/sofcheck
}

# Tantabus, a rust workspace whose uci crate is the binary. Its makefile asks
# for target-cpu=native, dropped here so the build does not depend on which
# runner picked it up.
REPOSITORY[tantabus]=https://github.com/analog-hors/tantabus.git
tantabus_build() {
    cargo build --release --quiet -p tantabus-uci
    echo target/release/tantabus-uci
}

# Weiss. Fathom is vendored, so a shallow clone is enough. The default
# target's flags name the machine they are built on, dropped here as in the
# tantabus block. Popcount is kept because the rated version is a popcount
# build; pext is not, since the runner may lack bmi2. This target leaves the
# binary beside the sources rather than in ../bin.
REPOSITORY[weiss]=https://github.com/TerjeKir/weiss.git
weiss_build() {
    make -C src bench-basic CFLAGS="-std=gnu11 -O3 -flto -msse3 -mpopcnt" > /dev/null
    echo src/weiss
}

# Cinnamon. Its cmake file builds a debug target against clang and gtest, so
# the release build is its makefile, whose generic target is the plain x86-64.
REPOSITORY[cinnamon]=https://github.com/gekomad/Cinnamon.git
cinnamon_build() {
    make -C src cinnamon64-generic -j"$(nproc)" > /dev/null
    echo src/cinnamon
}

# FoxSEE, a single rust crate with no dependencies.
REPOSITORY[foxsee]=https://github.com/redsalmon91/FoxSEE.git
foxsee_build() {
    cargo build --release --quiet
    echo target/release/foxsee
}

# Blunder, a Go module whose main package is the blunder/ directory. Built
# with -o because the package's name is that directory, which go will not
# overwrite with the binary.
REPOSITORY[blunder]=https://github.com/deanmchris/blunder.git
blunder_build() {
    mkdir -p bin
    go build -o bin/blunder ./blunder
    echo bin/blunder
}

# Zahak, a Go module whose main package is the zahak/ directory. Its version
# string is set at link time, and is given the pin so the banner names it;
# left unset the engine calls itself dev. Its tags carry no v, so a pin is
# 6.2 rather than v6.2.
REPOSITORY[zahak]=https://github.com/amanjpro/zahak.git
zahak_build() {
    mkdir -p bin
    go build -ldflags "-X 'main.version=${1}'" -o bin/zahak ./zahak
    echo bin/zahak
}

# Inanis, a single rust crate. The binary needs no file beside it.
REPOSITORY[inanis]=https://github.com/Tearth/Inanis.git
inanis_build() {
    cargo build --release --quiet
    echo target/release/inanis
}

# An engine with a repository and no build, or the reverse, is not listed, so
# the workflow refuses it where the ladder is read rather than after the clone.
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
    # a trap rather than a last line, since under set -e a failed fetch or
    # build has no other way out. WORK is not local because the trap runs
    # after this function has returned
    WORK=$(mktemp -d)
    trap 'rm -rf "$WORK"' EXIT
    # anything but a whole sha is asked for as a tag: a bare name would fetch
    # a branch of that name just as happily, and a branch moves under the
    # rating. An abbreviated sha is refused by the protocol
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
