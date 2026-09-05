#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2022-2026 Andrew Wright

"""Count how the games in a fastchess pgn ended, and who each ending fell on.

A match that lost games to a crash or to the clock still reports an elo, and
that number is worth less than it looks. The games stay in the result: dropping
them would be choosing what to count after seeing it. The counts sit beside the
estimate instead, so a surprising run can be read for how much of it was play.

fastchess writes the kind of ending in the Termination tag and the side it fell
on in the reason, which goes at the end of the comment on the last move. Both
are needed. Termination alone does not say which engine disconnected, and
`abandoned` covers a crash and a stall alike.

The block goes to stdout, a `key: value` per line, for the run summary and the
manifest. Anything worth an alert goes to stderr, so the workflow can raise it
from there rather than parsing it back out of the block.
"""

import argparse
import re
import sys
from collections import Counter
from pathlib import Path

import rating_estimate

# The game split and the tag parse are the ones the rating estimate already
# reads a fastchess pgn with, which take a game at a time so that one truncated
# by an interrupted match cannot borrow the next one's tags.
RECORD = rating_estimate.RECORD
TAG = rating_estimate.TAG

COMMENT = re.compile(r"\{([^{}]*)\}")

# The wordings fastchess ends the last comment with when an ending fell on one
# side. The colour is what it names, and the White and Black tags place it. An
# adjudication names the winner rather than a sufferer, so it is not here.
BLAMED = re.compile(
    r"\b(White|Black)"
    r"(?: loses on time|'s connection stalls| disconnects| makes an illegal move)"
)

# Termination as fastchess spells it. abandoned is not in the list because it
# covers two endings that are worth telling apart, and the reason tells them
# apart; see kind() below.
ORDER = (
    "normal",
    "adjudication",
    "time forfeit",
    "disconnect",
    "stall",
    "illegal move",
    "unterminated",
)

# The endings that say something went wrong with a player rather than with the
# position. An adjudication is the match settling a game that was already
# decided, and an unterminated game is one the match was stopped in the middle
# of, which the workflow's own clock cap already reports.
FAULTS = ("time forfeit", "disconnect", "stall", "illegal move")


def kind(termination: str, reason: str) -> str:
    """What to file a game under. Every Termination fastchess writes is used as
    it stands, so a word added to a later release is counted rather than
    dropped, except that abandoned is split into the crash and the stall it
    stands for."""
    if termination == "abandoned":
        return "stall" if "connection stalls" in reason else "disconnect"
    return termination or "unrecorded"


def count(text: str) -> tuple[Counter, dict[str, Counter]]:
    """The games by how they ended, and within each, by the engine it fell on.

    A record with no players is not a game, so it is not counted. One with no
    Termination is counted as unrecorded, which is what a pgn written with
    `min=true` would be all of."""
    totals: Counter = Counter()
    blamed: dict[str, Counter] = {}
    for record in RECORD.split(text)[1:]:
        tags = dict(TAG.findall(record))
        white, black = tags.get("White"), tags.get("Black")
        if not (white and black):
            continue
        comments = COMMENT.findall(record)
        reason = comments[-1] if comments else ""
        name = kind(tags.get("Termination", ""), reason)
        totals[name] += 1
        if (colour := BLAMED.search(reason)) is not None:
            engine = white if colour.group(1) == "White" else black
            blamed.setdefault(name, Counter())[engine] += 1
    return totals, blamed


def suffered(blamed: dict[str, Counter], name: str) -> str:
    """Who an ending fell on, as the engines and their counts, or nothing when
    the pgn did not say."""
    if not (who := blamed.get(name)):
        return ""
    return ", ".join(f"{engine} {n}" for engine, n in sorted(who.items()))


def block(totals: Counter, blamed: dict[str, Counter]) -> str:
    """The counts as the summary and the manifest carry them. Every ending is
    printed, zero included: no engine having crashed is worth recording too."""
    lines = [f"games: {sum(totals.values())}"]
    for name in [*ORDER, *sorted(set(totals) - set(ORDER))]:
        who = suffered(blamed, name)
        lines.append(f"{name}: {totals[name]}{f' ({who})' if who else ''}")
    return "\n".join(lines)


def remark(totals: Counter, blamed: dict[str, Counter]) -> str:
    """One line about the games that ended in a fault, or nothing when none
    did."""
    faulted = sum(totals[name] for name in FAULTS)
    if not faulted:
        return ""
    parts = []
    for name in FAULTS:
        if not totals[name]:
            continue
        who = suffered(blamed, name)
        parts.append(f"{totals[name]} by {name}{f' ({who})' if who else ''}")
    games = sum(totals.values())
    return (
        f"{faulted} of {games} games ended by a fault and not by play:"
        f" {', '.join(parts)}"
    )


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("pgn", type=Path, help="the games the match played")
    args = parser.parse_args()

    totals, blamed = count(args.pgn.read_text())
    print(block(totals, blamed))
    if line := remark(totals, blamed):
        print(line, file=sys.stderr)


if __name__ == "__main__":
    main()
