#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2022-2026 Andrew Wright

"""Read how fast each side searched in a match's games, and print it for the
Strength summary.

The bench measures speed on a fixed suite at a fixed depth with a small
table, and the games run under other conditions: a larger table, two games
to a runner, deeper and warmer searches from book openings, so a speed
change on the bench need not reach the games, and this reads the games
themselves.

fastchess notes in each move's comment the thinking time and the nodes and
nodes a second of the engine's last info line. The nodes and the time do not
cover the same span: a search stopped partway through an iteration prints no
last line, so the nodes after it are in the time and not in the count, and
nodes over time read low by a share that moves with how the search spends
its iterations. So a move's rate is read over the span its last line covers,
its nodes over their nodes a second, and a side's rate over a game is its
nodes over the sum of those spans. Each game gives one ratio, the candidate's
rate against the baseline's, and since both sides played that game under the
same load the ratio cancels it, the way a bench round does. The change is the
Hodges-Lehmann estimate over the games' ratios with its 95% interval, as
speed.py reads rounds.

    ingame_speed.py <games directory> --candidate NAME [--candidate SHA]
                    --baseline NAME [--baseline SHA]

The directory is searched for games.pgn files, as the Strength workflow's
shard artifacts leave them.
"""

import argparse
import pathlib
import re
import sys
from dataclasses import dataclass

import speed

# a searched move's comment: score/depth, then the time, then n= and nps=
SEARCHED = re.compile(r"^[^/\s]*/\d+ ([\d.]+)s,.*?\bn=(\d+)\b.*?\bnps=(\d+)")
COMMENT = re.compile(r"\{([^}]*)\}")
TAG = re.compile(r'^\[(\w+) "([^"]*)"\]', re.MULTILINE)

# a side that thought for less than this over a whole game says too little
MINIMUM_SECONDS = 1.0


@dataclass
class Game:
    candidate_nps: float
    baseline_nps: float


@dataclass
class Side:
    """What one side reported over a game."""

    nodes: int = 0
    # the span the reported nodes were counted over, from their rate
    counted: float = 0.0
    # the thinking time, which the span sits inside
    thought: float = 0.0


def sides(movetext: str, black_first: bool) -> tuple[list[Side], int]:
    """Each side's report over a game, white first, and how many comments
    the game had. A comment is written for every ply, a book move's
    included, so the comments alternate with the side to move."""
    both = [Side(), Side()]
    comments = COMMENT.findall(movetext)
    for ply, comment in enumerate(comments):
        found = SEARCHED.match(comment.strip())
        if not found:
            continue
        side = both[(ply + black_first) % 2]
        nodes, rate = int(found.group(2)), int(found.group(3))
        side.thought += float(found.group(1))
        if rate > 0:
            side.nodes += nodes
            side.counted += nodes / rate
    return both, len(comments)


HEX = re.compile(r"[0-9a-f]{7,40}")


def same_engine(name: str, known: list[str]) -> bool:
    """Whether a PGN's name for an engine is one of the names it is known by.
    A run names an engine by the ref it was given, which is a branch, a tag
    or a sha, and a sha may be cut short on either side."""
    for other in known:
        if name == other:
            return True
        if (
            HEX.fullmatch(name)
            and HEX.fullmatch(other)
            and (name.startswith(other) or other.startswith(name))
        ):
            return True
    return False


def games_in(
    text: str, candidate: list[str], baseline: list[str]
) -> tuple[list[Game], int]:
    """The games between the two sides, and how many were passed over: for
    too little thinking time on one side, or for a comment count that does
    not match the plies, where the sides could not be told apart."""
    games = []
    skipped = 0
    for block in re.split(r"\n(?=\[Event )", text):
        tags = dict(TAG.findall(block))
        white, black = tags.get("White", ""), tags.get("Black", "")
        if same_engine(white, candidate) and same_engine(black, baseline):
            candidate_side = 0
        elif same_engine(black, candidate) and same_engine(white, baseline):
            candidate_side = 1
        else:
            continue
        black_first = " b " in tags.get("FEN", "")
        movetext = block.split("\n\n", 1)[1] if "\n\n" in block else ""
        both, comments = sides(movetext, black_first)
        plies = tags.get("PlyCount")
        if plies is not None and int(plies) != comments:
            skipped += 1
            continue
        if min(side.thought for side in both) < MINIMUM_SECONDS or not all(
            side.counted for side in both
        ):
            skipped += 1
            continue
        rates = [side.nodes / side.counted for side in both]
        games.append(Game(rates[candidate_side], rates[1 - candidate_side]))
    return games, skipped


def report(games: list[Game], skipped: int) -> list[str]:
    """The summary's section, in markdown."""
    lines = ["### Speed in the games", ""]
    if speed.signed_rank_depth(len(games)) == 0:
        lines.append(
            f"{len(games)} games with thinking time on both sides, too few "
            "for an interval."
        )
        return lines
    estimate = speed.paired(
        [game.baseline_nps for game in games],
        [game.candidate_nps for game in games],
    )
    lines.append(
        f"Nodes a second, candidate against baseline: {estimate.change:+.1f}% "
        f"(95% interval {speed.interval(estimate)}) over {len(games)} games."
    )
    if skipped:
        lines.append(
            f"{skipped} games are left out, where a side thought for under "
            f"{MINIMUM_SECONDS:g} s or the comments did not match the plies."
        )
    lines += [
        "",
        (
            "Each game gives one ratio, the candidate's nodes a second against "
            "the baseline's, read from the move comments over the span each "
            "move's last info line covers; both sides played that game under "
            "the same load. When the search changed, the sides count different "
            "nodes, and this says what a reported node cost rather than whether "
            "the change is faster. This batch's games only, and the bench's "
            "speed, read on a fixed suite with a small table, can differ from it."
        ),
    ]
    return lines


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("games", type=pathlib.Path)
    # each side by any name a run may give it: its ref, and its sha
    parser.add_argument("--candidate", action="append", required=True)
    parser.add_argument("--baseline", action="append", required=True)
    args = parser.parse_args(argv)

    if set(args.candidate) & set(args.baseline):
        parser.error("the two sides share a name, so the games cannot tell them apart")
    games: list[Game] = []
    skipped = 0
    for pgn in sorted(args.games.rglob("games.pgn")):
        found, passed = games_in(
            pgn.read_text(encoding="utf-8", errors="replace"),
            args.candidate,
            args.baseline,
        )
        games += found
        skipped += passed
    for line in report(games, skipped):
        print(line)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
