#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2022-2026 Andrew Wright

# The ccrl lists the calibration gauntlet can place the engine on:
#
#     ladders.sh list              every list named below
#     ladders.sh preset <list>     what a gauntlet against it plays, as
#                                  name=value lines for GITHUB_OUTPUT
#
# One list is one block below: the ladder of rungs read off it, the control
# the gauntlet plays at, the games against each rung, the wall clock cap on a
# rung's play, the control the list itself rates at and the prefix of the
# run's artifacts. The workflow asks for a list and takes everything else from
# its block unless a box is filled in, so a ladder cannot be paired with the
# ratings of another list by leaving a default in place.
#
# A rung is engine:pin:rating. The engines are the ones scripts/opponent.sh
# can build, and the rating is the one the list gives the version the pin
# points at.
set -euo pipefail

# A name is in the list the workflow checks against only when it has all six
# fields.
declare -A LADDER TIME_CONTROL GAMES MINUTES RATED_AT ARTIFACT

# The ccrl blitz list of 5 September 2026. Eight rungs of six lineages
# bracketing 2700, at 20+0.2, which is roughly a sixth of 2+1. Fifty games
# against a rung take about half an hour.
LADDER[blitz]=stash:v20.0.1:2511,tantabus:v2.0.0:2555,weiss:v0.9:2650,blunder:v8.5.5:2663,stash:v21.0:2713,inanis:v1.1.0:2763,zahak:6.2:2825,weiss:v0.10:2846
TIME_CONTROL[blitz]=20+0.2
GAMES[blitz]=50
MINUTES[blitz]=90
RATED_AT[blitz]=2m+1s
ARTIFACT[blitz]=calibrate

# The ccrl 40/15 list of 18 September 2026. Six rungs from 2558 to 2845,
# four of them the blitz panel's own, at 40/150, one sixth of 40/15. The
# clock allows a game about twelve minutes, so sixteen games against a rung
# take at most about an hour and a half.
LADDER[40/15]=tantabus:v2.0.0:2558,weiss:v0.9:2651,blunder:v8.5.5:2692,inanis:v1.1.0:2746,stash:v21.2:2785,weiss:v1.0:2845
TIME_CONTROL[40/15]=40/150
GAMES[40/15]=16
MINUTES[40/15]=240
RATED_AT[40/15]="40 moves in 15 minutes"
ARTIFACT[40/15]=calibrate-ccrl-40-15

# A list missing one of its six fields is not listed, so the workflow refuses
# it before a match rather than playing part of a preset.
list() {
    local name
    for name in "${!LADDER[@]}"; do
        if [ -n "${TIME_CONTROL[$name]+named}" ] && [ -n "${GAMES[$name]+named}" ] \
            && [ -n "${MINUTES[$name]+named}" ] && [ -n "${RATED_AT[$name]+named}" ] \
            && [ -n "${ARTIFACT[$name]+named}" ]; then
            printf '%s\n' "$name"
        fi
    done | sort
}

# Asked of the list rather than of the table, as in book.sh: the name arrives
# from a text box.
known() {
    local name
    for name in $(list); do
        if [ "$name" = "$1" ]; then
            return
        fi
    done
    echo "ladders.sh: no list named ${1}," \
        "a gauntlet can play $(list | tr '\n' ' ')" >&2
    exit 1
}

preset() {
    known "$1"
    echo "ladder=${LADDER[$1]}"
    echo "time_control=${TIME_CONTROL[$1]}"
    echo "games=${GAMES[$1]}"
    echo "max_match_minutes=${MINUTES[$1]}"
    echo "rated_at=${RATED_AT[$1]}"
    echo "artifact=${ARTIFACT[$1]}"
}

command=${1:-}
case "$command" in
    list) list ;;
    preset) preset "${2:?usage: ladders.sh preset <list>}" ;;
    *) echo "usage: ladders.sh list|preset" >&2; exit 1 ;;
esac
