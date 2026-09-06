#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2022-2026 Andrew Wright

"""Pool the shards of a match into one estimate.

A match of any size is more games than one job can play inside its timeout, so
the games are played by several jobs at once, each with a slice of the opening
book of its own, and this reads them back as one match. Every shard is a pgn,
and no two shards share an opening, so the games pool as if one match had
played them.

fastchess prints an estimate of its own, but only over the games the one
process played. It cannot see the other shards, so the pooled figure is worked
out here from the games themselves.

The error bar is measured over pairs and not over games. With `-repeat` the two
games of a round are the same opening with the colours reversed, so they are
one draw and not two, and treating them as two would understate the spread. The
pair is also what a shard that ran out of clock can leave half of, so the games
it left over are counted and said, and kept out of the pair statistics.

The report goes to stdout for the run summary. `--line` prints the one line the
release notes carry and `--trailer` the trailer a commit does. Anything worth an
alert goes to stderr, so the workflow can raise it from there rather than
parsing it back out of the report.
"""

import argparse
import math
import re
import sys
from collections import Counter
from pathlib import Path

import match_terminations
import rating_estimate

# The game split and the tag parse the rest of this tooling reads a fastchess
# pgn with. Round is wanted here and by nothing else: fastchess writes it on
# every game, and with -repeat the two games of a round share an opening.
RECORD = rating_estimate.RECORD
TAG = rating_estimate.TAG

LN10 = math.log(10)
# the 95% interval, in standard errors
CONFIDENCE = 1.96
# A match that went one way throughout bounds the difference from one side
# only. This is as far out as it is worth reading, and is where the rating
# estimate stops its own extrapolation.
MAX_ELO = rating_estimate.MAX_IMPLIED

# what a result tag is worth to the player of the white pieces
RESULTS = {"1-0": 1.0, "1/2-1/2": 0.5, "0-1": 0.0}

# the shard index, as the artifact names carry it
SHARD = re.compile(r"shard-(\d+)")

# the five scores a pair can end on, from the candidate's point of view
PENTANOMIAL = (0.0, 0.5, 1.0, 1.5, 2.0)


def read_games(text: str, candidate: str) -> tuple[dict[str, list[float]], int]:
    """The candidate's score in each finished game of one shard, by round.

    The colours are read from the tags rather than assumed, since `-repeat`
    plays the second game of every round the other way round. A game with no
    result is one the match was stopped in the middle of; it is counted and
    left out."""
    rounds: dict[str, list[float]] = {}
    unfinished = 0
    for record in RECORD.split(text)[1:]:
        tags = dict(TAG.findall(record))
        white, black, result = tags.get("White"), tags.get("Black"), tags.get("Result")
        if not (white and black) or candidate not in (white, black):
            continue
        if result not in RESULTS:
            unfinished += 1
            continue
        score = RESULTS[result] if white == candidate else 1 - RESULTS[result]
        rounds.setdefault(tags.get("Round", ""), []).append(score)
    return rounds, unfinished


def pair_up(rounds: dict[str, list[float]]) -> tuple[list[float], int]:
    """The pair scores of one shard, out of two, and the games left over.

    A round holds the two games of one opening, so a round with both of them
    is a pair. A shard stopped by the clock in the middle of a round leaves
    one game, which counts in the score and in nothing the pair is the unit
    of."""
    scores, unpaired = [], 0
    for _, games in sorted(rounds.items()):
        for index in range(0, len(games) - 1, 2):
            scores.append(games[index] + games[index + 1])
        if len(games) % 2:
            unpaired += 1
    return scores, unpaired


class Shard:
    """One shard's games, and how they ended."""

    def __init__(self, name: str, text: str, candidate: str):
        self.name = name
        found = SHARD.search(name)
        self.index = int(found.group(1)) if found else None
        rounds, self.unfinished = read_games(text, candidate)
        self.games = [score for scores in rounds.values() for score in scores]
        self.pairs, self.unpaired = pair_up(rounds)
        totals, _ = match_terminations.count(text)
        self.faults = sum(totals[ending] for ending in match_terminations.FAULTS)

    @property
    def points(self) -> float:
        return sum(self.games)

    @property
    def percent(self) -> float:
        return 100 * self.points / len(self.games) if self.games else 0.0


class Estimate:
    """What the games say the difference is, and how far out that could be.

    The score is turned into elo with the logistic model the rest of this
    tooling uses, `elo = -400 log10(1/p - 1)`. The interval comes from the
    pairs: the variance of the pair scores as a fraction of the two points a
    pair is worth, divided by the number of pairs, is the variance of the
    score, and its square root is the standard error of the score. The
    derivative of the model at the score, `400 / (ln 10 p (1 - p))`, carries
    that into elo, and 1.96 of them is the 95% interval.

    A score of nought or of one has no elo: the model runs off to infinity
    there, so the estimate is bounded on one side and says so instead. A match
    with no complete pair has no interval either, which is the same answer as
    not having measured."""

    def __init__(self, points: float, games: int, pair_scores: list[float]):
        self.games = games
        self.pairs = len(pair_scores)
        self.score = points / games if games else 0.0
        self.elo = 0.0
        self.margin: float | None = None
        self.low, self.high = -math.inf, math.inf
        self.los = 0.5
        self.bounded = ""

        if not games or not pair_scores:
            self.bounded = "not measured"
            return

        mean = sum(pair_scores) / (2 * self.pairs)
        variance = sum((score / 2 - mean) ** 2 for score in pair_scores) / self.pairs
        error = math.sqrt(variance / self.pairs)

        if self.score <= 0.0 or self.score >= 1.0:
            above = self.score >= 1.0
            self.elo = MAX_ELO if above else -MAX_ELO
            self.bounded = f"above +{MAX_ELO:.0f}" if above else f"below -{MAX_ELO:.0f}"
            self.low = self.elo if above else -math.inf
            self.high = math.inf if above else self.elo
            self.los = 1.0 if above else 0.0
            return

        self.elo = -400 * math.log10(1 / self.score - 1)
        slope = 400 / (LN10 * self.score * (1 - self.score))
        self.margin = CONFIDENCE * error * slope
        self.low, self.high = self.elo - self.margin, self.elo + self.margin
        if error == 0.0:
            self.los = 1.0 if self.score > 0.5 else 0.0 if self.score < 0.5 else 0.5
        else:
            self.los = 0.5 * (1 + math.erf((self.score - 0.5) / (error * math.sqrt(2))))

    def __str__(self) -> str:
        if self.bounded == "not measured":
            return f"{100 * self.score:.1f}% score ({self.games} games)"
        if self.margin is None:
            return f"{self.bounded} Elo ({self.games} games)"
        return f"{round(self.elo):+d} ±{round(self.margin)} Elo ({self.games} games)"


def trailer(estimate: Estimate, tc: str, baseline: str) -> str:
    """The result as the Elo trailer a commit carries, in the shape the
    commit-msg hook accepts. A match with no estimate to state says so rather
    than quoting a number it does not have."""
    if estimate.margin is None:
        return "Elo: not measured"
    return (
        f"Elo: {round(estimate.elo):+d} ±{round(estimate.margin)}"
        f" ({estimate.games} games, {tc}, vs {baseline})"
    )


def interval(estimate: Estimate) -> str:
    if estimate.bounded == "not measured":
        return "There were no complete pairs, so there is no interval to report."
    if estimate.bounded:
        return (
            "Every pair went the same way, so the games bound the difference"
            f" from one side only, at {estimate.bounded} elo."
        )
    return (
        f"The 95% interval is {round(estimate.low):+d} to"
        f" {round(estimate.high):+d} elo, and the likelihood of superiority is"
        f" {100 * estimate.los:.1f}%."
    )


def pentanomial(pairs: list[float]) -> list[str]:
    """The pairs by what the candidate scored in them, which is what the
    interval is measured over."""
    counted = Counter(pairs)
    heads = " | ".join(f"{score:g}" for score in PENTANOMIAL)
    rule = " | ".join("---" for _ in PENTANOMIAL)
    counts = " | ".join(str(counted[score]) for score in PENTANOMIAL)
    return [f"| pair score | {heads} |", f"| --- | {rule} |", f"| pairs | {counts} |"]


def table(shards: list[Shard], estimate: Estimate) -> list[str]:
    """A row per shard, so a runner that played fewer games than the others, or
    lost some of them to a fault, shows rather than being averaged away."""
    rows = ["| shard | games | score | faults |", "| --- | --- | --- | --- |"]
    for shard in shards:
        rows.append(
            f"| {shard.name} | {len(shard.games)} |"
            f" {shard.percent:.1f}% | {shard.faults} |"
        )
    faults = sum(shard.faults for shard in shards)
    rows.append(
        f"| pooled | {estimate.games} | {100 * estimate.score:.1f}% | {faults} |"
    )
    return rows


def report(shards: list[Shard], estimate: Estimate, text: str) -> str:
    unpaired = sum(shard.unpaired for shard in shards)
    unfinished = sum(shard.unfinished for shard in shards)
    left_over = (
        f" {unpaired} of the games had no partner, so they are in the score and"
        " not in the interval."
        if unpaired
        else ""
    )
    stopped = (
        f" {unfinished} games had no result and are left out altogether."
        if unfinished
        else ""
    )
    lines = [
        str(estimate),
        "",
        (
            f"{estimate.pairs} pairs from {estimate.games} games.{left_over}"
            f"{stopped} {interval(estimate)}"
        ),
        "",
        *pentanomial([score for shard in shards for score in shard.pairs]),
        "",
        *table(shards, estimate),
        "",
        "How the games ended:",
        "",
        "```",
        match_terminations.block(*match_terminations.count(text)),
        "```",
    ]
    return "\n".join(lines)


def read_shards(paths: list[Path], candidate: str) -> tuple[list[Shard], str]:
    """One shard per pgn, named after the directory it arrived in, which is the
    artifact it was downloaded from. A single artifact is extracted without a
    directory of its own, so the name falls back to the file's."""
    shards, texts = [], []
    for path in paths:
        text = path.read_text()
        texts.append(text)
        shards.append(Shard(path.parent.name or path.name, text, candidate))
    shards.sort(key=lambda shard: (shard.index is None, shard.index or 0, shard.name))
    return shards, "\n".join(texts)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("pgn", type=Path, nargs="+", help="a shard's games, one file")
    parser.add_argument("--candidate", required=True, help="the name it played under")
    parser.add_argument("--baseline", required=True, help="what it played against")
    parser.add_argument("--tc", default="", help="the time control played")
    printed = parser.add_mutually_exclusive_group()
    printed.add_argument(
        "--line",
        action="store_true",
        help="print the one line for the release notes instead of the report",
    )
    printed.add_argument(
        "--trailer",
        action="store_true",
        help="print the Elo trailer for a commit instead of the report",
    )
    args = parser.parse_args()

    shards, text = read_shards(args.pgn, args.candidate)
    games = [score for shard in shards for score in shard.games]
    if not games:
        sys.exit(f"no games for {args.candidate} in {len(args.pgn)} shards")
    pairs = [score for shard in shards for score in shard.pairs]
    estimate = Estimate(sum(games), len(games), pairs)

    if args.trailer:
        print(trailer(estimate, args.tc, args.baseline))
    elif args.line:
        print(estimate)
    else:
        print(report(shards, estimate, text))
    if fault := match_terminations.remark(*match_terminations.count(text)):
        print(fault, file=sys.stderr)


if __name__ == "__main__":
    main()
