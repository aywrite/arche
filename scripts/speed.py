#!/usr/bin/env python3
"""Measure one engine's bench against another's and print the Speed trailer.

A rate on its own says nothing across machines, and a single pair of runs
says nothing on one: this box swings ten percent between runs. So the two
binaries take turns, which side goes first alternating each round, the
medians are compared, and the spread of the rounds is printed beside the
change so a reader can tell a claim from noise. The node counts are printed
too, since a search change moves them and a speed change must not, and when
they do differ the report adds the time to depth, which is what nps cannot
see: nps normalises for the size of the tree by construction, so a search
that visits fewer nodes at the same cost each finishes sooner while the rate
says nothing happened.

    speed.py <base binary> <candidate binary> [--rounds N] [--depth D]
             [--base-ref SHA]

scripts/speed.sh builds the base commit and calls this.
"""

import argparse
import statistics
import subprocess
import sys
from dataclasses import dataclass, field


@dataclass
class Measured:
    base_nps: list[int] = field(default_factory=list)
    candidate_nps: list[int] = field(default_factory=list)
    base_nodes: int = 0
    candidate_nodes: int = 0


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
    # stdin closed, so a binary from before the bench existed, which would
    # start its uci loop and wait, ends instead
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
        # which side runs first alternates, so a machine warming up or
        # cooling down through the rounds leans on neither. The side is
        # carried along rather than read back from the path, which the two
        # sides may share when an engine is measured against itself
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


def spread(rates: list[int]) -> float:
    """How far the rounds ranged, as a share of the median."""
    return 100.0 * (max(rates) - min(rates)) / statistics.median(rates)


def change(base: float, candidate: float) -> float:
    """The candidate against the base, as a percentage. Below zero is less of
    whatever was counted, which for a time is faster and for a rate is slower,
    so what it reads as is left to the caller to say."""
    return 100.0 * (candidate - base) / base


def seconds(nodes: int, rates: list[int]) -> list[float]:
    """How long each round took. The bench prints a count and a rate rather
    than a time, and the count is the same every round, so one divided by the
    other is the time exactly rather than an estimate of it.

    Taken a round at a time rather than from the median rate, because a median
    over an even number of rounds is the mean of the middle two and does not
    survive being divided into.
    """
    return [nodes / rate for rate in rates]


def time_to_depth(measured: Measured) -> tuple[float, float]:
    """The median seconds each side spent reaching the bench's depth.

    This is what a change to the tree is worth and what the rate cannot see.
    It is not the better number, it is the other one: a change can shrink the
    tree and make every node dearer at the same time, and only the two
    together say what happened.
    """
    base = statistics.median(seconds(measured.base_nodes, measured.base_nps))
    candidate = statistics.median(
        seconds(measured.candidate_nodes, measured.candidate_nps)
    )
    return base, candidate


# Said when the counts differ, because neither number is a claim at that point
# and the trailer on its own would read like one.
COUNTS_DIFFER = """the node counts differ, so this is a search change as well as a speed one:
nps compares what a node costs, and the two sides are averaging over
different nodes. Time to depth is what the change is worth at this depth,
and only games say whether the tree it now searches is the right one."""


def trailer(base: list[int], candidate: list[int], base_ref: str) -> str:
    """The Speed trailer: the change between medians, and the wider of the
    two sides' spreads, which is what the change has to be read against."""
    change = 100.0 * (statistics.median(candidate) - statistics.median(base))
    change /= statistics.median(base)
    widest = max(spread(base), spread(candidate))
    return (
        f"Speed: {change:+.1f}% (bench nps, {len(base)} interleaved rounds "
        f"vs {base_ref}, spread {widest:.1f}%)"
    )


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("base")
    parser.add_argument("candidate")
    parser.add_argument("--rounds", type=int, default=5)
    parser.add_argument("--depth", type=int, default=None)
    parser.add_argument("--base-ref", default="base")
    args = parser.parse_args(argv)
    if args.rounds < 2:
        parser.error(
            "at least two rounds: one shows no spread, so it is no measurement"
        )

    measured = measure(args.base, args.candidate, args.rounds, args.depth)
    print(f"{'round':>5} {'base nps':>12} {'candidate nps':>14}")
    for i, (b, c) in enumerate(zip(measured.base_nps, measured.candidate_nps), 1):
        print(f"{i:>5} {b:>12} {c:>14}")
    base_seconds, candidate_seconds = time_to_depth(measured)
    print(
        f"base: {measured.base_nodes} nodes in {base_seconds:.2f} s, "
        f"candidate: {measured.candidate_nodes} nodes in {candidate_seconds:.2f} s"
    )
    if measured.base_nodes != measured.candidate_nodes:
        base_rate = statistics.median(measured.base_nps)
        candidate_rate = statistics.median(measured.candidate_nps)
        print(
            f"nodes {change(measured.base_nodes, measured.candidate_nodes):+.1f}%, "
            f"nps {change(base_rate, candidate_rate):+.1f}%, "
            f"time to depth {change(base_seconds, candidate_seconds):+.1f}%"
        )
        # set off on its own, since it is the part a reader skims past and
        # the one that says the trailer below is not the whole story. The
        # trailer stays the last line, which is what speed.sh is piped into
        print()
        print(COUNTS_DIFFER)
        print()
    print(trailer(measured.base_nps, measured.candidate_nps, args.base_ref))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
