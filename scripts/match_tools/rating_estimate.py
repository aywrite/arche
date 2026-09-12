# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2022-2026 Andrew Wright

"""Estimate a rating on the ccrl scale from a gauntlet against rated engines.

Every opponent is held at its published ccrl figure, so the only free parameter
is our own rating, and the maximum likelihood fit of the logistic model is
simply the rating at which the expected score equals the score actually made.

The margin printed with it is a 95% interval on how much a score of this size
wobbles, and it describes the games and nothing else. It is the interval the
pooled match estimate prints under the same symbol, read off the same constant,
because one ± that means two things is worse than no ± at all. Every figure this
tool printed before 2026-09-12 was one standard error, so it was about half as
wide. Whether one rating can describe the
results at all is a separate question, asked separately: when the opponents
disagree with each other by more than chance allows, that margin is an
understatement rather than an estimate, and a note saying so goes to stderr.

The table goes to stdout. `--line` prints the estimate alone and `--json`
prints the fit as data, in a shape that is provisional while its format is 0.
"""

import argparse
import json
import math
import re
import sys
from pathlib import Path

from . import JSON_FORMAT, tool

LN10_OVER_400 = math.log(10) / 400
# The 95% interval, in standard errors. Both tools print ±, so both read this:
# the pooled match estimate imports it from here rather than keeping a second
# copy that could drift to a different scale under the same symbol.
CONFIDENCE = 1.96
# A pairing that ends 25-0 puts no upper bound on the winner, so both the
# implied rating and the search for the fitted one stop this far out rather than
# running off to wherever the bracket happens to end.
MAX_IMPLIED = 1200.0

# Games are read one at a time rather than by scanning the whole file for tags,
# so that a game truncated by an interrupted match cannot pair its opponent with
# the next game's result. Both are also what match_terminations.py and
# match_estimate.py read a pgn with, so how a fastchess game is picked apart is
# written down once; the fit here has no use for Termination or for Round.
RECORD = re.compile(r"^\[Event ", re.MULTILINE)
TAG = re.compile(r'^\[(White|Black|Result|Termination|Round) "([^"]*)"\]', re.MULTILINE)


def expected(rating: float, opponent: float) -> float:
    return 1.0 / (1.0 + 10 ** ((opponent - rating) / 400))


def implied(opponent: float, score: float, games: int) -> float:
    """The rating a single pairing on its own points at."""
    fraction = score / games
    if fraction <= 0.0:
        return opponent - MAX_IMPLIED
    if fraction >= 1.0:
        return opponent + MAX_IMPLIED
    return opponent - 400 * math.log10(1 / fraction - 1)


def chi_square_95(dof: int) -> float:
    """The point a chi square of this many degrees of freedom passes one time in
    twenty by chance. Wilson and Hilferty's cube root approximation, which is
    within a couple of percent from one degree of freedom upwards and saves
    carrying a table of critical values around."""
    return dof * (1 - 2 / (9 * dof) + 1.645 * math.sqrt(2 / (9 * dof))) ** 3


class Estimate:
    def __init__(self, rating: float, margin: float, games: int, bounded: str = ""):
        self.rating = rating
        self.margin = margin
        self.games = games
        # set when every game went one way, so the games bound the rating from
        # one side only and a figure with a margin either side would be a lie
        self.bounded = bounded

    def __str__(self) -> str:
        if self.bounded:
            return f"{self.bounded} on the ccrl blitz scale ({self.games} games)"
        # the scale is printed with the figure, so a line pasted into a
        # release note carries what its ± means wherever it ends up
        return (
            f"{self.rating:.0f} ±{self.margin:.0f} (95%)"
            f" on the ccrl blitz scale ({self.games} games)"
        )


def fit(pairings: list[tuple[str, float, int, int, int]]) -> tuple[Estimate, str]:
    scored = sum(w + d / 2 for _, _, w, d, _ in pairings)
    played = sum(w + d + loss for _, _, w, d, loss in pairings)
    opponents = [opponent for _, opponent, _, _, _ in pairings]

    if scored == 0:
        return Estimate(0, 0, played, f"below {min(opponents):.0f}"), ""
    if scored == played:
        return Estimate(0, 0, played, f"above {max(opponents):.0f}"), ""

    # The expected score only rises with the rating, so bisection cannot miss.
    # The bracket stops where a single pairing would, because past that point
    # the games say nothing and the answer would be the bracket, not the fit.
    low, high = min(opponents) - MAX_IMPLIED, max(opponents) + MAX_IMPLIED
    for _ in range(200):
        mid = (low + high) / 2
        total = sum(
            (w + d + loss) * expected(mid, opponent)
            for _, opponent, w, d, loss in pairings
        )
        if total < scored:
            low = mid
        else:
            high = mid
    rating = (low + high) / 2

    # How much a result of this size wobbles. The variance is the one the games
    # actually showed rather than one a win/loss model assumes, so that draws
    # count as the low variance results they are, and it is pooled across the
    # pairings because how often a game is drawn is a property of the engines
    # rather than of the opponent, and a pairing that was swept shows none.
    spread = 0.0
    modelled = 0.0
    slope = 0.0
    for _, opponent, w, d, loss in pairings:
        games = w + d + loss
        mean = (w + d / 2) / games
        spread += games * max((w + d / 4) / games - mean * mean, 0.0)
        chance = expected(rating, opponent)
        modelled += games * chance * (1 - chance)
        slope += games * LN10_OVER_400 * chance * (1 - chance)
    # Every game going the same way inside every pairing leaves no wobble to
    # measure, which is not the same as there being none, so fall back to the
    # wobble the fitted rating would predict rather than report no doubt at all.
    if spread == 0.0:
        spread = modelled
    per_game = spread / played
    margin = CONFIDENCE * math.sqrt(spread) / slope

    # Whether one rating describes all of the pairings, which is a different
    # question from how precisely it is pinned down. Comparing each pairing's
    # score with the one the fit expects of it answers it, and a fit that has
    # already been bent to match the total costs a degree of freedom.
    dof = len(pairings) - 1
    note = ""
    if dof > 0:
        chi = sum(
            ((w + d / 2) - (w + d + loss) * expected(rating, opponent)) ** 2
            / ((w + d + loss) * per_game)
            for _, opponent, w, d, loss in pairings
        )
        if chi > chi_square_95(dof):
            note = (
                "the opponents disagree with each other by more than chance"
                " allows, so one rating does not describe these results and the"
                " margin above understates how uncertain the figure is"
            )
    return Estimate(rating, margin, played, ""), note


def read_pairings(
    pgn: Path,
    engine: str,
    ladder: dict[str, float],
    remarks: list[str] | None = None,
) -> list[tuple[str, float, int, int, int]]:
    """The pairings the gauntlet played, and what is worth saying about the
    games it did not fit. What is said goes to stderr as it is found, and into
    `remarks` as well when a caller wants to print it somewhere else too."""
    tally: dict[str, list[int]] = {name: [0, 0, 0] for name in ladder}
    unfinished = 0
    text = pgn.read_text()

    def say(line: str) -> None:
        print(line, file=sys.stderr)
        if remarks is not None:
            remarks.append(line)

    for record in RECORD.split(text)[1:]:
        tags = dict(TAG.findall(record))
        white, black, result = tags.get("White"), tags.get("Black"), tags.get("Result")
        # a game still in progress when the match was interrupted has no result
        # to count, and no business borrowing the next one's
        if not (white and black) or engine not in (white, black):
            continue
        opponent = black if white == engine else white
        if opponent not in tally:
            continue
        if result == "1/2-1/2":
            tally[opponent][1] += 1
        elif result == "1-0":
            tally[opponent][0 if white == engine else 2] += 1
        elif result == "0-1":
            tally[opponent][2 if white == engine else 0] += 1
        else:
            unfinished += 1
    # A handful of these is a match that was interrupted. A lot of them is the
    # estimate being drawn from a fraction of the games that were paid for, so
    # say how many rather than quietly fitting whatever finished.
    if unfinished:
        plural = "" if unfinished == 1 else "s"
        say(f"{unfinished} game{plural} with no result, left out of the fit")
    for name in ladder:
        if sum(tally[name]) == 0:
            say(f"{name} played no games and is not in the fit")
    return [
        (name, ladder[name], *tally[name]) for name in ladder if sum(tally[name]) > 0
    ]


def as_json(
    engine: str,
    ladder: dict[str, float],
    pairings: list[tuple[str, float, int, int, int]],
    estimate: Estimate,
    note: str,
    remarks: list[str],
) -> dict:
    """The fit as data, for a reader that is not a person.

    The figures are unrounded: rounding is what the table and the line do with
    them, and a consumer can round what it reads. A rating the games bound
    from one side only has no figure at all, so it is null and the bounded
    string is what says which side. What went to stderr is here as well, so
    that reading stdout alone loses nothing.

    The shape is provisional while the format is 0, which JSON_FORMAT
    explains."""
    measured = not estimate.bounded
    return {
        "format": JSON_FORMAT,
        "tool": tool("rating_estimate"),
        "engine": engine,
        "ladder": ladder,
        "pairings": [
            {
                "opponent": name,
                "ccrl": opponent,
                "wins": w,
                "draws": d,
                "losses": loss,
                "games": w + d + loss,
                "score": (w + d / 2) / (w + d + loss),
                "implied": implied(opponent, w + d / 2, w + d + loss),
            }
            for name, opponent, w, d, loss in pairings
        ],
        "rating": estimate.rating if measured else None,
        "margin": estimate.margin if measured else None,
        "games": estimate.games,
        "bounded": estimate.bounded,
        "note": note,
        "remarks": remarks,
        "line": str(estimate),
    }


def read_ladder(spec: str) -> dict[str, float]:
    ladder = {}
    for entry in spec.split(","):
        entry = entry.strip()
        if not entry:
            continue
        name, _, rating = entry.rpartition(":")
        try:
            ladder[name.strip()] = float(rating)
        except ValueError:
            sys.exit(f"ladder entry {entry!r} is not name:rating")
        if not name.strip():
            sys.exit(f"ladder entry {entry!r} is not name:rating")
    if not ladder:
        sys.exit("the ladder is empty")
    return ladder


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("pgn", type=Path, help="the games the gauntlet played")
    parser.add_argument("engine", help="the name the engine played under")
    parser.add_argument("ladder", help="opponents, as name:rating pairs")
    # each of these replaces the whole of stdout, so at most one of them
    printed = parser.add_mutually_exclusive_group()
    printed.add_argument(
        "--line",
        action="store_true",
        help="print only the estimate, for somewhere a table does not fit",
    )
    printed.add_argument(
        "--json",
        action="store_true",
        help="print the fit as json instead of the table. The shape is"
        " provisional while the format is 0",
    )
    args = parser.parse_args()

    ladder = read_ladder(args.ladder)
    remarks: list[str] = []
    pairings = read_pairings(args.pgn, args.engine, ladder, remarks)
    if not pairings:
        sys.exit(f"no games for {args.engine} against any of {', '.join(ladder)}")

    estimate, note = fit(pairings)
    if args.json:
        # allow_nan=False rather than the default, so a figure that is not a
        # number fails here rather than being written as one no parser has to
        # read back
        print(
            json.dumps(
                as_json(args.engine, ladder, pairings, estimate, note, remarks),
                indent=2,
                allow_nan=False,
            )
        )
    else:
        if not args.line:
            print("| opponent | ccrl | w-d-l | score | implies |")
            print("| --- | --- | --- | --- | --- |")
            for name, opponent, w, d, loss in pairings:
                games = w + d + loss
                percent = 100 * (w + d / 2) / games
                print(
                    f"| {name} | {opponent:.0f} | {w}-{d}-{loss} | {percent:.1f}% |"
                    f" {implied(opponent, w + d / 2, games):.0f} |"
                )
            print()
        print(estimate)
    if note:
        print(f"note: {note}", file=sys.stderr)


if __name__ == "__main__":
    main()
