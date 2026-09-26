#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2022-2026 Andrew Wright

# Build the engine once and link it again into many code layouts:
#
#     layouts.sh <ref | --tree> <dir> <count> <shuffle | pad>
#
# <dir>/default is the ordinary build. <dir>/1 to <dir>/<count> are the same
# compiled code linked again, so they run the same instructions and differ
# only in where the code lands. Under pad, layout i moves the start of .text
# by a multiple of sixteen bytes chosen from i. Under shuffle it does that and
# also has lld order the input sections by seed i, so every function and
# every table moves. Layout i takes the same offset and seed for any commit,
# and speed.py runs it on both sides in round i. Shuffling needs lld, which
# rustc links x86_64 linux with by default; pad works under GNU ld too.
#
# The build keeps the object the link starts from (-C save-temps) and prints
# the link command (--print link-args), and a layout is that command run
# again with another output and the flags above, about thirty milliseconds
# each. Both flags go through cargo rustc to the final crate only, so the
# commit's own .cargo/config.toml still applies, and the default binary is
# byte for byte what cargo build makes. A commit is built from an export by
# build_at.sh, and --tree builds the working tree as it stands.
#
# The object is overwritten by the next build, so the layouts are linked here
# and not later. What save-temps keeps, about thirty megabytes of the
# engine's crate, stays in the target directory, and under --tree the next
# cargo build links the engine again, since the flags differ.
#
# A layout is rustc's own printing of the link command, run again by bash.
# That holds for the paths a checkout and a target directory have, and a
# directory whose name carries a quote, a dollar sign, a backslash, a
# backtick, a hash or an ampersand is refused rather than passed through.
set -euo pipefail

usage="usage: layouts.sh <ref | --tree> <dir> <count> <shuffle | pad>"
ref=${1:?$usage}
dir=${2:?$usage}
count=${3:?$usage}
mode=${4:?$usage}
case "$mode" in
    shuffle | pad) ;;
    *) echo "layouts.sh: the mode is shuffle or pad, not ${mode}" >&2; exit 2 ;;
esac

target=$(realpath -m "${CARGO_TARGET_DIR:-target}")
dir=$(realpath -m "$dir")
case "$dir" in
    *[\"\$\\\`\#\&]*)
        echo "layouts.sh: ${dir} has a character the link command cannot carry" >&2
        exit 2
        ;;
esac
# only a directory this script made is emptied, so a slip of the argument
# cannot take a checkout or a target directory with it
if [ -e "$dir" ] && [ -n "$(ls -A "$dir")" ] && [ ! -f "${dir}/mode" ]; then
    echo "layouts.sh: ${dir} is not empty and was not made by layouts.sh" >&2
    exit 2
fi
rm -rf "$dir"
mkdir -p "$dir"

build=(cargo rustc --release --quiet --bin arche -- -C save-temps --print link-args)
# cargo prints the link command only when it links, and a crate it holds
# fresh is not linked again, so the engine's own crate is cleaned first. The
# dependencies are kept
if [ "$ref" = --tree ]; then
    cargo clean --release --quiet --package arche
    "${build[@]}" > "${target}/link-args"
    cp "${target}/release/arche" "${dir}/default"
    link=$(cat "${target}/link-args")
else
    # build_at.sh runs the command inside the export with CARGO_TARGET_DIR
    # set to its own target directory, which is where the line is written.
    # The quotes are single so that the inner shell expands them there
    # shellcheck disable=SC2016
    "$(dirname "$0")/build_at.sh" "$ref" "${dir}/default" ../release/arche \
        bash -c 'cargo clean --release --quiet --package arche &&
                 "$@" > "${CARGO_TARGET_DIR}/link-args"' build "${build[@]}"
    link=$(cat "${target}/at/link-args")
fi
case "$link" in
    *'"-o" "'*) ;;
    *) echo "layouts.sh: the build printed no link command" >&2; exit 1 ;;
esac
if [ "$mode" = shuffle ]; then
    case "$link" in
        *-fuse-ld=lld* | *gcc-ld*) ;;
        *)
            echo "layouts.sh: the link is not lld's, which shuffle needs; pad does not" >&2
            exit 1
            ;;
    esac
fi

for i in $(seq 1 "$count"); do
    out="${dir}/${i}"
    # a multiple of sixteen, the alignment of .text, spread over a page. The
    # script writes one byte and skips the rest, since lld drops an empty
    # output section rather than placing it
    pad=$(( (i * 97) % 256 * 16 + 16 ))
    printf 'SECTIONS { .textpad : { BYTE(0); . += %d; } } INSERT BEFORE .text;\n' \
        "$((pad - 1))" > "${out}.ld"
    extra="\"-Wl,-T,${out}.ld\""
    if [ "$mode" = shuffle ]; then
        extra="${extra} \"-Wl,--shuffle-sections=*=${i}\""
    fi
    bash -c "$(printf '%s' "$link" | sed -E "s#\"-o\" \"[^\"]*\"#\"-o\" \"${out}\" ${extra}#")"
    rm "${out}.ld"
done
# for speed.py, which names the kind in the trailer and refuses to pair a
# shuffled side with a padded one
echo "$mode" > "${dir}/mode"
