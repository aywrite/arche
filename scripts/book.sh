#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2022-2026 Andrew Wright

# The opening books a match can be played on:
#
#     book.sh list                  every book named below
#     book.sh file <book>           the file it plays as
#     book.sh format <book>         what fastchess reads that file as
#     book.sh count <book> <path>   how many openings that file holds
#     book.sh fetch <book> <dir>    download it at the pin, unzip it, check it
#     book.sh verify <book> <dir>   check one already there against the table
#
# One book is one block below: the file it arrives as, the format fastchess
# reads it in, how many openings it holds and the sha256 of the unzipped file.
# Adding a book is adding a block. The names the blocks declare are what the
# workflows check their book input against, so a typo is refused where the
# input is read rather than after a runner has fetched something.
#
# Counting openings is not one question. A pgn holds a game per opening and an
# epd holds a position a line, so the count belongs beside the format rather
# than in the workflow, which is what lets one workflow play either.
set -euo pipefail

# Every book is fetched at this commit. The action fetched from master, which
# is a branch, and a branch moves under the run that names it: the manifest's
# book_sha256 would record the change with nothing failing. scripts/opponent.sh
# refuses a branch for an engine for the same reason. The pin is in the
# match-tools cache key as well, so changing it here cannot leave a cache
# handing back the old file under the new name.
PIN=65815ccdbc7727cd4f6aee252ba8f67fb740e92f

# The table. A block is these four fields, and a name is in the list the
# workflows check against only when it has all of them.
declare -A FILE FORMAT OPENINGS SHA256

# The book every Strength and Calibrate figure so far was played on, and the
# default of both workflows. Its openings are eight moves of a real game
# apiece, balanced by construction, so a side does not start an opening ahead.
FILE[8moves_v3]=8moves_v3.pgn
FORMAT[8moves_v3]=pgn
OPENINGS[8moves_v3]=34700
SHA256[8moves_v3]=5835239f88cc2c7511b177c32392a69f3ede21819cf0616f80a7f907cd21d17e

# Unbalanced on purpose, where the book above is balanced. Under -repeat both
# engines play both colours of an opening, so an unbalanced book does not bias
# who wins; it raises how often a game is decided. That is what makes a figure
# on it a contrast with a figure on the other rather than a second helping of
# the same thing.
FILE[UHO_4060_v2]=UHO_4060_v2.epd
FORMAT[UHO_4060_v2]=epd
OPENINGS[UHO_4060_v2]=242201
SHA256[UHO_4060_v2]=36f2ec751ab78def6be1307430cbe2cd2ba65ade8d2aaae8f10e3df7d0ea83e1

# A book missing one of its four fields is not listed. The workflows read this
# list to decide whether their book input can be played at all, so a half added
# book is refused before a match rather than at the count.
list() {
    local book
    for book in "${!FILE[@]}"; do
        if [ -n "${FORMAT[$book]+named}" ] && [ -n "${OPENINGS[$book]+named}" ] \
            && [ -n "${SHA256[$book]+named}" ]; then
            printf '%s\n' "$book"
        fi
    done | sort
}

# Asked of the list rather than of the table, because an associative array
# reads @ and * as every key rather than as a name nobody has written a block
# for, and the book arrives here from a text box.
known() {
    local book
    for book in $(list); do
        if [ "$book" = "$1" ]; then
            return
        fi
    done
    echo "book.sh: no book named ${1}," \
        "a match can play $(list | tr '\n' ' ')" >&2
    exit 1
}

file() {
    known "$1"
    echo "${FILE[$1]}"
}

format() {
    known "$1"
    echo "${FORMAT[$1]}"
}

# What a book counts is what its format counts: a pgn opening is a game and
# every game carries an Event tag, an epd opening is a line. The file is
# counted rather than the block's figure printed, because a cache hit hands a
# run a file the fetch never checked.
count() {
    local book=$1 path=$2 held
    known "$book"
    if [ ! -r "$path" ]; then
        echo "book.sh: ${path} is not there to count" >&2
        exit 1
    fi
    case "${FORMAT[$book]}" in
        pgn) held=$(grep -c '^\[Event' "$path") || held=0 ;;
        epd) held=$(grep -c . "$path") || held=0 ;;
        *)
            echo "book.sh: nothing here counts a ${FORMAT[$book]}" >&2
            exit 1 ;;
    esac
    if [ "$held" -lt 1 ]; then
        echo "book.sh: ${path} holds no ${FORMAT[$book]} openings" >&2
        exit 1
    fi
    echo "$held"
}

# Of the unzipped file, which is what is played and what a manifest records.
# The pin says which file a run plays and this is what holds it to that, so it
# is a step of its own as well as part of a fetch: a cache hit skips the fetch,
# and a restored file is what most runs actually play.
checked() {
    local book=$1 path=$2 arrived
    arrived=$(sha256sum "$path" | cut -d' ' -f1)
    if [ "$arrived" != "${SHA256[$book]}" ]; then
        echo "book.sh: ${FILE[$book]} is ${arrived}," \
            "not the ${SHA256[$book]} the table names" >&2
        exit 1
    fi
}

verify() {
    local book=$1 directory=$2
    known "$book"
    local file=${FILE[$book]}
    if [ ! -r "${directory}/${file}" ]; then
        echo "book.sh: ${directory}/${file} is not there to check" >&2
        exit 1
    fi
    checked "$book" "${directory}/${file}"
}

fetch() {
    local book=$1 directory=$2
    known "$book"
    local file=${FILE[$book]}
    # the download is thrown away however this ends, which set -e makes a trap
    # rather than a last line: a fetch that failed and a file that arrived as
    # something else have no other way out. WORK is not local because the trap
    # runs after this function has returned
    WORK=$(mktemp -d)
    trap 'rm -rf "$WORK"' EXIT
    curl -sSLf -o "${WORK}/book.zip" \
        "https://raw.githubusercontent.com/official-stockfish/books/${PIN}/${file}.zip" \
        || { echo "book.sh: cannot fetch ${file}.zip at ${PIN}" >&2; exit 1; }
    unzip -qo "${WORK}/book.zip" "$file" -d "$WORK" \
        || { echo "book.sh: ${file}.zip does not hold ${file}" >&2; exit 1; }
    # checked before the file is moved, so a run that fetched something else
    # has nothing to play rather than something to play
    checked "$book" "${WORK}/${file}"
    mkdir -p "$directory"
    mv "${WORK}/${file}" "${directory}/${file}"
}

command=${1:-}
case "$command" in
    list) list ;;
    file) file "${2:?usage: book.sh file <book>}" ;;
    format) format "${2:?usage: book.sh format <book>}" ;;
    count)
        count "${2:?usage: book.sh count <book> <path>}" \
            "${3:?usage: book.sh count <book> <path>}"
        ;;
    fetch)
        fetch "${2:?usage: book.sh fetch <book> <dir>}" \
            "${3:?usage: book.sh fetch <book> <dir>}"
        ;;
    verify)
        verify "${2:?usage: book.sh verify <book> <dir>}" \
            "${3:?usage: book.sh verify <book> <dir>}"
        ;;
    *) echo "usage: book.sh list|file|format|count|fetch|verify" >&2; exit 1 ;;
esac
