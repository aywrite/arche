#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2022-2026 Andrew Wright

"""Measure one engine's bench against another's and print the Speed trailer.

A single pair of runs says nothing: this box swings ten percent between runs.
So the two binaries take turns, which side goes first alternating each round,
and each round's pair gives one ratio. The change is the Hodges-Lehmann
estimate over those ratios, with its 95% interval from the signed rank test,
and a verdict reads the interval against a threshold set above what moving
the code about can do to the rate on its own. The node counts and the time
to depth are printed too: a search change moves the counts and a speed
change must not, and nps normalises for the size of the tree, so a search
that visits fewer nodes at the same cost each finishes sooner while the
rate says nothing happened.

    speed.py <base binary> <candidate binary> [--rounds N] [--depth D]
             [--base-ref SHA] [--threshold PCT]

scripts/speed.sh builds the base commit and calls this.
"""

import argparse
import math
import statistics
import subprocess
import sys
import textwrap
from dataclasses import dataclass, field

# The confidence of the interval. Two sided, so each tail gets half.
CONFIDENCE = 0.95

# How far the rate moves between two builds that differ only in where the
# code lands, as a percentage. Read off the Bench workflow's speed job over
# the pull requests up to #321. Of the 53 that changed no build input, so
# that both sides were one binary, 4 had an interval that excluded zero.
# Release version bumps and comment sweeps, which change nothing but layout,
# posted offsets near 1.5% with intervals that excluded zero. More rounds do
# not average that away, since it belongs to the binary and not to the run.
THRESHOLD = 2.0


@dataclass
class Measured:
    base_nps: list[int] = field(default_factory=list)
    candidate_nps: list[int] = field(default_factory=list)
    base_nodes: int = 0
    candidate_nodes: int = 0


@dataclass
class Estimate:
    """A change as a percentage, and the interval around it."""

    change: float
    low: float
    high: float


def last_line(text: str) -> tuple[int, int] | None:
    """The nodes and the rate from the bench's last line, `N nodes M nps`,
    or nothing when the output does not end that way."""
    lines = text.strip().splitlines()
    words = lines[-1].split() if lines else []
    if len(words) != 4 or words[1] != "nodes" or words[3] != "nps":
        return None
    return int(words[0]), int(words[2])


def bench(binary: str, depth: int | None) -> tuple[int, int]:
    command = [binary, "bench"] + ([str(depth)] if depth is not None else [])
    # stdin closed, so a binary from before the bench existed does not sit in
    # its uci loop waiting
    output = subprocess.run(
        command,
        check=True,
        capture_output=True,
        text=True,
        stdin=subprocess.DEVNULL,
    )
    if (read := last_line(output.stdout)) is None:
        raise SystemExit(
            f"{binary}: no bench in its output, which ended: {output.stdout[-200:]!r}"
        )
    return read


def measure(base: str, candidate: str, rounds: int, depth: int | None) -> Measured:
    measured = Measured()
    for round_ in range(rounds):
        # alternating, so a machine warming up or cooling down leans on
        # neither side. The side is carried rather than read off the path,
        # which both share when an engine is measured against itself
        order = [(True, base), (False, candidate)]
        if round_ % 2:
            order.reverse()
        for is_base, binary in order:
            nodes, nps = bench(binary, depth)
            if is_base:
                measured.base_nps.append(nps)
                measured.base_nodes = nodes
            else:
                measured.candidate_nps.append(nps)
                measured.candidate_nodes = nodes
    return measured


def change(base: float, candidate: float) -> float:
    """The candidate against the base, as a percentage. Below zero is less of
    whatever was counted: faster for a time, slower for a rate."""
    return 100.0 * (candidate - base) / base


def signed_rank_depth(rounds: int) -> int:
    """How far each bound of the interval steps in from its end of the sorted
    Walsh averages: the most k with P(W < k) no more than half of what the
    confidence leaves, W being the signed rank statistic of that many rounds
    when nothing changed. Counted exactly rather than from the normal
    approximation, which is poor at the rounds a laptop can spare. Zero when
    no interval reaches the confidence, which at 95% is below six rounds."""
    # ways[w] is how many of the 2^n sign patterns have a rank sum of w
    ways = [1]
    for rank in range(1, rounds + 1):
        grown = ways + [0] * rank
        for w, count in enumerate(ways):
            grown[w + rank] += count
        ways = grown
    tail = (1 - CONFIDENCE) / 2 * 2**rounds
    below = 0
    for k, count in enumerate(ways):
        if below + count > tail:
            return k
        below += count
    return len(ways)


def paired(base: list[int], candidate: list[int]) -> Estimate:
    """The Hodges-Lehmann estimate of the change, and its interval.

    A round's two runs sit next to each other, so their ratio cancels
    whatever the machine was doing then; a runner that drifts through a job
    moves the medians apart and leaves the ratios alone. The estimate is the
    median of the averages of every pair of ratios, which one loaded round
    moves little. The interval is the one the signed rank test gives, which
    assumes only that the ratios are spread evenly about the true change. A
    loaded round does move the interval: its averages with every other ratio
    sit together at one end, and at nine rounds one such round is enough to
    carry the bound out to it. That errs towards no claim, and more rounds
    take more loaded ones to do it. Worked in logs, so that half as fast and
    twice as fast are the same distance from no change.
    """
    ratios = [math.log(c / b) for b, c in zip(base, candidate)]
    walsh = sorted(
        (ratios[i] + ratios[j]) / 2
        for i in range(len(ratios))
        for j in range(i, len(ratios))
    )
    k = signed_rank_depth(len(ratios))
    if k == 0:
        raise ValueError(f"{len(ratios)} rounds have no interval at {CONFIDENCE:.0%}")
    return Estimate(
        100.0 * math.expm1(statistics.median(walsh)),
        100.0 * math.expm1(walsh[k - 1]),
        100.0 * math.expm1(walsh[-k]),
    )


def seconds(nodes: int, rates: list[int]) -> list[float]:
    """How long each round took: the count divided by the rate, which is exact
    since the count is the same every round. A round at a time rather than
    from the median rate, because a median over an even number of rounds is
    the mean of the middle two and does not survive being divided into.
    """
    return [nodes / rate for rate in rates]


def time_to_depth(measured: Measured) -> tuple[float, float]:
    """The median seconds each side spent reaching the bench's depth. Not the
    better number but the other one: a change can shrink the tree and make
    every node dearer at once, and only the two together say what happened.
    """
    base = statistics.median(seconds(measured.base_nodes, measured.base_nps))
    candidate = statistics.median(
        seconds(measured.candidate_nodes, measured.candidate_nps)
    )
    return base, candidate


# Said when the counts differ, because the trailer on its own would then read
# as a claim it is not.
COUNTS_DIFFER = """the node counts differ, so this is a search change as well as a speed
one. nps only says what a node costs. Time to depth is what the change is
worth at this depth, and whether the new tree is the right one is for
games to say."""


def verdict(estimate: Estimate, threshold: float) -> str:
    """What the interval says against the threshold. A change is claimed only
    when the whole interval is past the threshold, and ruled out only when
    the whole interval is inside it. Anything else wants more rounds."""
    if estimate.low > threshold:
        return f"faster: the whole interval is above +{threshold:.1f}%"
    if estimate.high < -threshold:
        return f"slower: the whole interval is below -{threshold:.1f}%"
    if -threshold <= estimate.low and estimate.high <= threshold:
        return f"no change beyond ±{threshold:.1f}%: the whole interval is inside it"
    return (
        f"not resolved: the interval reaches past ±{threshold:.1f}% without "
        "clearing it, so more rounds are needed to say either way"
    )


def summary(measured: Measured) -> list[str]:
    """One row per side and the change under each column.

    The fastest column is there because nothing sharing the machine ever
    makes a run faster, so each side's best round is its least interfered
    one. It is a second reading and has no interval. When the counts match,
    the change row leaves nodes and time empty: the time is then the rate
    upside down and would say nothing the nps cell does not.
    """
    base_seconds, candidate_seconds = time_to_depth(measured)
    base_rate = statistics.median(measured.base_nps)
    candidate_rate = statistics.median(measured.candidate_nps)
    base_fastest = max(measured.base_nps)
    candidate_fastest = max(measured.candidate_nps)
    differ = measured.base_nodes != measured.candidate_nodes
    columns = [
        (
            "nodes",
            str(measured.base_nodes),
            str(measured.candidate_nodes),
            f"{change(measured.base_nodes, measured.candidate_nodes):+.1f}%"
            if differ
            else "",
        ),
        (
            "time",
            f"{base_seconds:.2f} s",
            f"{candidate_seconds:.2f} s",
            f"{change(base_seconds, candidate_seconds):+.1f}%" if differ else "",
        ),
        (
            "median nps",
            f"{base_rate:.0f}",
            f"{candidate_rate:.0f}",
            f"{change(base_rate, candidate_rate):+.1f}%",
        ),
        (
            "fastest nps",
            str(base_fastest),
            str(candidate_fastest),
            f"{change(base_fastest, candidate_fastest):+.1f}%",
        ),
    ]
    labels = ["", "base", "candidate", "change"]
    label_width = max(len(label) for label in labels)
    widths = [max(len(cell) for cell in column) for column in columns]
    lines = []
    for row, label in enumerate(labels):
        cells = (f"{column[row]:>{width}}" for column, width in zip(columns, widths))
        lines.append((f"{label:<{label_width}}  " + "  ".join(cells)).rstrip())
    return lines


def interval(estimate: Estimate) -> str:
    return f"{estimate.low:+.1f}% to {estimate.high:+.1f}%"


def trailer(base: list[int], candidate: list[int], base_ref: str) -> str:
    """The Speed trailer: the paired change and its interval."""
    estimate = paired(base, candidate)
    return (
        f"Speed: {estimate.change:+.1f}% (bench nps, "
        f"{CONFIDENCE:.0%} interval {interval(estimate)}, "
        f"{len(base)} interleaved rounds vs {base_ref})"
    )


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("base")
    parser.add_argument("candidate")
    parser.add_argument("--rounds", type=int, default=15)
    parser.add_argument("--depth", type=int, default=None)
    parser.add_argument("--base-ref", default="base")
    parser.add_argument("--threshold", type=float, default=THRESHOLD)
    args = parser.parse_args(argv)
    if signed_rank_depth(args.rounds) == 0:
        parser.error(
            f"at least six rounds: fewer have no {CONFIDENCE:.0%} interval, "
            "so they are no measurement"
        )

    measured = measure(args.base, args.candidate, args.rounds, args.depth)
    print(f"{'round':>5} {'base nps':>12} {'candidate nps':>14} {'change':>7}")
    for i, (b, c) in enumerate(zip(measured.base_nps, measured.candidate_nps), 1):
        print(f"{i:>5} {b:>12} {c:>14} {change(b, c):>+6.1f}%")
    print()
    for line in summary(measured):
        print(line)
    estimate = paired(measured.base_nps, measured.candidate_nps)
    print()
    print(
        f"paired change {estimate.change:+.1f}%, "
        f"{CONFIDENCE:.0%} interval {interval(estimate)}"
    )
    print()
    if measured.base_nodes != measured.candidate_nodes:
        # the rates are then over different trees, and a verdict would read
        # them as if they were not
        print(COUNTS_DIFFER)
    else:
        print(textwrap.fill(verdict(estimate, args.threshold), width=72))
    # the trailer stays the last line, which is what speed.sh reads
    print()
    print(trailer(measured.base_nps, measured.candidate_nps, args.base_ref))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
