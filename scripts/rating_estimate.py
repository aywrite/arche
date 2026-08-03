#!/usr/bin/env python3
"""Estimate a rating on the ccrl scale from a gauntlet against rated engines.

Every opponent is held at its published ccrl figure, so the only free parameter
is our own rating, and the maximum likelihood fit of the logistic model is
simply the rating at which the expected score equals the score actually made.

Two error bars come out of this and the larger is the one reported. The first is
the usual one from how much a score of this size wobbles. The second is how far
the opponents disagree with each other: a single number cannot describe an
engine that does better against one opponent than another predicts, and when
that happens the first error bar is an understatement rather than an estimate.
"""

import math
import re
import sys
from pathlib import Path

LN10_OVER_400 = math.log(10) / 400
# a pairing that ends 25-0 puts no bound on how much stronger the winner is, so
# the implied rating is capped rather than allowed to run off to infinity
MAX_IMPLIED = 1200.0

GAME = re.compile(
    r'\[White "([^"]+)"\].*?\[Black "([^"]+)"\].*?\[Result "([^"]+)"\]',
    re.DOTALL,
)


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


def fit(pairings: list[tuple[str, float, int, int, int]]) -> tuple[float, float, str]:
    scored = sum(w + d / 2 for _, _, w, d, _ in pairings)

    # the expected score only rises with the rating, so bisection cannot miss
    low, high = 0.0, 4000.0
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
    # count as the low variance results they are.
    variance = 0.0
    slope = 0.0
    for _, opponent, w, d, loss in pairings:
        games = w + d + loss
        mean = (w + d / 2) / games
        second_moment = (w + d / 4) / games
        variance += games * max(second_moment - mean * mean, 1e-9)
        chance = expected(rating, opponent)
        slope += games * LN10_OVER_400 * chance * (1 - chance)
    statistical = math.sqrt(variance) / slope

    # How far the opponents disagree. With one opponent there is nothing to
    # disagree with, so there is no second opinion to report.
    points = [
        implied(opponent, w + d / 2, w + d + loss)
        for _, opponent, w, d, loss in pairings
    ]
    if len(points) > 1:
        mean = sum(points) / len(points)
        spread = math.sqrt(sum((p - mean) ** 2 for p in points) / (len(points) - 1))
        between = spread / math.sqrt(len(points))
    else:
        between = 0.0

    if between > statistical:
        return (
            rating,
            between,
            "opponents disagree by more than the games alone explain",
        )
    return rating, statistical, ""


def read_pairings(
    pgn: Path, engine: str, ladder: dict[str, float]
) -> list[tuple[str, float, int, int, int]]:
    tally: dict[str, list[int]] = {name: [0, 0, 0] for name in ladder}
    for white, black, result in GAME.findall(pgn.read_text()):
        if engine not in (white, black):
            continue
        opponent = black if white == engine else white
        if opponent not in tally:
            continue
        if result == "1/2-1/2":
            tally[opponent][1] += 1
        elif (result == "1-0") == (white == engine):
            tally[opponent][0] += 1
        else:
            tally[opponent][2] += 1
    return [
        (name, ladder[name], *tally[name]) for name in ladder if sum(tally[name]) > 0
    ]


def main() -> None:
    pgn, engine, spec = Path(sys.argv[1]), sys.argv[2], sys.argv[3]
    ladder = {}
    for entry in spec.split(","):
        name, rating = entry.rsplit(":", 1)
        ladder[name.strip()] = float(rating)

    pairings = read_pairings(pgn, engine, ladder)
    if not pairings:
        sys.exit(f"no games for {engine} against any of {', '.join(ladder)}")

    rating, margin, caveat = fit(pairings)
    for name, opponent, w, d, loss in pairings:
        games = w + d + loss
        percent = 100 * (w + d / 2) / games
        print(
            f"| {name} | {opponent:.0f} | {w}-{d}-{loss} | {percent:.1f}% |"
            f" {implied(opponent, w + d / 2, games):.0f} |"
        )
    total = sum(w + d + loss for _, _, w, d, loss in pairings)
    print()
    print(f"{rating:.0f} ±{margin:.0f} on the ccrl blitz scale ({total} games)")
    if caveat:
        print(f"note: {caveat}")


if __name__ == "__main__":
    main()
