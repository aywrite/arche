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

A binary's rate also depends on where its code landed, which any edit
redraws: comment sweeps and version bumps have moved it by 1.5%. Given two
directories that scripts/layouts.sh made instead of two binaries, round n
runs both sides on layout n, so the interval carries that draw instead of one
build's worth of it sitting inside the change. The default link, the one a
release ships, is then measured after as a diagnostic.

    speed.py <base> <candidate> [--rounds N] [--depth D] [--base-ref SHA]
             [--threshold PCT] [--loaded PCT] [--cpu LIST]

scripts/speed.sh builds the base commit and calls this.
"""

import argparse
import math
import os
import statistics
import subprocess
import sys
import textwrap
from dataclasses import dataclass, field

# two sided, so each tail gets half
CONFIDENCE = 0.95

# A percentage, set above how far the rate moves between two builds that
# differ only in where the code lands. Over the speed job's comments on the
# pull requests up to #321, 4 of the 53 that changed no build input had an
# interval that excluded zero, and release bumps and comment sweeps posted
# offsets near 1.5%. More rounds do not average that away, since it belongs
# to the binary and not to the run.
THRESHOLD = 2.0

# How far below the median pair a round's pair can run before the round is
# run again, as a fraction. Simulated with a tenth of the runs slowed by 3%
# to 15%, it took the interval at nine rounds from 7.0% wide to 4.1% and at
# twenty five from 2.0% to 1.4%, and the intervals went on holding the true
# change 94% to 97% of the time. It is for a machine whose load comes in
# bursts: on the Bench workflow's runners it marked 41 of 1,791 rounds up to
# #321 and changed little, and that job turns it off.
LOADED = 0.03

# The threshold when every round runs on a layout of its own. The layout's
# share of the rate is then inside the interval instead of beside it, so this
# is not a floor under a bias but the smallest change worth calling one, and
# it has to be one an interval can clear. A version bump, which moves the code
# and nothing else, measured over forty shuffled layouts on four runners gave
# intervals from ±0.2% to ±1.1%, and three of the four were inside ±1%. Same
# seeds on nearly the same code are nearly the same layouts, so a larger
# change pairs less closely and its interval is wider.
LAYOUT_THRESHOLD = 1.0

# One binary, or the layouts of one build in the order the rounds use them.
Side = str | list[str]


def binary_for(side: Side, number: int) -> str:
    """The binary round `number` runs for a side, counting from one."""
    return side if isinstance(side, str) else side[number - 1]


@dataclass
class Layouts:
    """What scripts/layouts.sh left in a directory."""

    default: str
    mode: str
    numbered: list[str]


def layouts_in(directory: str, needed: int) -> Layouts:
    """The layouts in a directory scripts/layouts.sh made, the first
    `needed` of them, or a usage error naming what is missing."""
    try:
        with open(os.path.join(directory, "mode")) as file:
            mode = file.read().strip()
    except OSError:
        raise SystemExit(f"{directory}: no mode file, so not made by layouts.sh")
    numbered = [os.path.join(directory, str(i)) for i in range(1, needed + 1)]
    missing = [path for path in numbered if not os.path.isfile(path)]
    if missing:
        raise SystemExit(
            f"{directory}: {needed} layouts needed and {missing[0]} is not there"
        )
    return Layouts(os.path.join(directory, "default"), mode, numbered)


@dataclass
class Measured:
    base_nps: list[int] = field(default_factory=list)
    candidate_nps: list[int] = field(default_factory=list)
    base_nodes: int = 0
    candidate_nodes: int = 0
    # the number each kept round was run as, counting from one
    rounds: list[int] = field(default_factory=list)
    # the rounds run again, as their number and the two rates they measured
    replaced: list[tuple[int, int, int]] = field(default_factory=list)


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


def loaded(measured: Measured, cut: float) -> list[int]:
    """The kept rounds whose pair ran more than `cut` below the median pair,
    as indices, the furthest below first.

    A pair is read by the geometric mean of its two rates, which says how
    fast the machine was that round and, when the two sides are as noisy as
    each other, nothing about their ratio. Reading each run against its own
    side instead trims the low tail of whichever side is noisier, which is
    the ratio's tail and biases the change."""
    pairs = [
        math.sqrt(b * c) for b, c in zip(measured.base_nps, measured.candidate_nps)
    ]
    median = statistics.median(pairs)
    shortfalls = [(1 - pair / median, i) for i, pair in enumerate(pairs)]
    return [i for shortfall, i in sorted(shortfalls, reverse=True) if shortfall > cut]


def budget(rounds: int, cut: float) -> int:
    """How many rounds may be run again: a fifth, or none with no cut."""
    return rounds // 5 if cut > 0 else 0


def measure(
    base: Side,
    candidate: Side,
    rounds: int,
    depth: int | None,
    cut: float = LOADED,
) -> Measured:
    """Run each side once to warm up, run the rounds, then run again any the
    machine was loaded for.

    A side is one binary, or a list of layouts of one build, and then round
    n runs layout n on both sides. Every run of a side has to count the same
    nodes, since a layout moves the code and never the search.

    The warmup runs are thrown away: over 187 of the speed job's runs the
    first run of a job was 1.13% below its side's median (standard error
    0.21) and the second 0.10%, which leaned the first round towards the
    side that went second.

    A loaded round is replaced by a new one at the end, on a layout of its
    own, rather than dropped, so the count stays what was asked for. At most
    a fifth of the rounds are replaced, so load that keeps coming back ends
    in a wide interval rather than a loop. A `cut` of zero replaces nothing."""
    measured = Measured()
    for side in (base, candidate):
        bench(binary_for(side, 1), depth)
    base_first: list[bool] = []
    counted: dict[bool, int] = {}

    def one_round(first: bool) -> None:
        number = len(measured.rounds) + len(measured.replaced) + 1
        # the side is carried rather than read off the path, which both
        # share when an engine is measured against itself
        order = [(True, base), (False, candidate)]
        if not first:
            order.reverse()
        for is_base, side in order:
            binary = binary_for(side, number)
            nodes, nps = bench(binary, depth)
            if counted.setdefault(is_base, nodes) != nodes:
                raise SystemExit(
                    f"{binary} counted {nodes} nodes where the rest of its "
                    f"side counted {counted[is_base]}, so it is not the same "
                    "search"
                )
            if is_base:
                measured.base_nps.append(nps)
                measured.base_nodes = nodes
            else:
                measured.candidate_nps.append(nps)
                measured.candidate_nodes = nodes
        measured.rounds.append(number)
        base_first.append(first)

    # alternating, so a machine warming up or cooling down leans on neither
    # side
    for round_ in range(rounds):
        one_round(round_ % 2 == 0)
    left = budget(rounds, cut)
    while left and (worst := loaded(measured, cut)[:left]):
        firsts = []
        for i in sorted(worst, reverse=True):
            measured.replaced.append(
                (
                    measured.rounds.pop(i),
                    measured.base_nps.pop(i),
                    measured.candidate_nps.pop(i),
                )
            )
            firsts.append(base_first.pop(i))
        # a replacement goes first on the side its round did, so the kept
        # rounds stay as balanced as the alternation made them
        for first in firsts:
            one_round(first)
        left -= len(worst)
    measured.replaced.sort()
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


def faster_half(rates: list[int]) -> float:
    """The mean of a side's faster half of its rounds, the middle one
    included when there is an odd number.

    Trimming the slow runs suits noise that leans slow, which it does on the
    runners. Trim nothing and the loaded runs drag the mean; keep only the
    fastest run and what is left is the fast side's own noise. Over eighty
    rounds of one binary against itself on four runners, scored as speed
    jobs of 9, 15 and 25 rounds whose true change was zero, the faster half
    had a root mean square error of 0.63%, 0.56% and 0.49% against 0.83%,
    0.75% and 0.68% for the fastest run. Anything from a third to two thirds
    did about as well; a half is the middle of that.
    """
    kept = sorted(rates, reverse=True)[: (len(rates) + 1) // 2]
    return statistics.mean(kept)


def summary(measured: Measured) -> list[str]:
    """One row per side and the change under each column.

    The faster half column is a diagnostic, not a second estimate: it has no
    interval, and the verdict does not read it. When the counts match, the
    change row leaves nodes and time empty: the time is then the rate upside
    down and would say nothing the nps cell does not.
    """
    base_seconds, candidate_seconds = time_to_depth(measured)
    base_rate = statistics.median(measured.base_nps)
    candidate_rate = statistics.median(measured.candidate_nps)
    base_faster = faster_half(measured.base_nps)
    candidate_faster = faster_half(measured.candidate_nps)
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
            "faster half",
            f"{base_faster:.0f}",
            f"{candidate_faster:.0f}",
            f"{change(base_faster, candidate_faster):+.1f}%",
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


def cpus(text: str) -> set[int]:
    """The cpus `--cpu` names, as `4`, `4,5` or `4-7`, the way taskset reads
    them.

    Pinning keeps a run on the cpus given rather than wherever the scheduler
    puts it. Measured under WSL on a desktop that mixes fast and slow cores,
    with other work running, it halved how far the paired change strayed at
    fifteen rounds, from 3.2% to 1.6%. Under WSL a cpu is a virtual one that
    the host can still move, so that is a measurement and not a promise."""
    try:
        found: set[int] = set()
        for part in text.split(","):
            low, dash, high = part.partition("-")
            found.update(range(int(low), int(high if dash else low) + 1))
    except ValueError:
        raise argparse.ArgumentTypeError(f"not a cpu list: {text}") from None
    if not found:
        raise argparse.ArgumentTypeError(f"not a cpu list: {text}")
    return found


# How a trailer names the layouts its rounds ran on, by layouts.sh's mode
OVER = {"shuffle": "shuffled", "pad": "padded"}


def trailer(
    base: list[int], candidate: list[int], base_ref: str, mode: str | None = None
) -> str:
    """The Speed trailer: the paired change and its interval, and the kind
    of layout the rounds ran on when they ran on layouts."""
    estimate = paired(base, candidate)
    over = f" over {OVER[mode]} layouts" if mode else ""
    return (
        f"Speed: {estimate.change:+.1f}% (bench nps, "
        f"{CONFIDENCE:.0%} interval {interval(estimate)}, "
        f"{len(base)} interleaved rounds{over} vs {base_ref})"
    )


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("base", help="a binary, or a directory of layouts")
    parser.add_argument("candidate", help="a binary, or a directory of layouts")
    parser.add_argument("--rounds", type=int, default=15)
    parser.add_argument("--depth", type=int, default=None)
    parser.add_argument("--base-ref", default="base")
    parser.add_argument(
        "--threshold",
        type=float,
        default=None,
        help=f"{THRESHOLD:g} by default, {LAYOUT_THRESHOLD:g} over layouts",
    )
    parser.add_argument(
        "--loaded",
        type=float,
        default=None,
        help="percent below the median pair that has a round run again, or 0 "
        f"to run none again; {100 * LOADED:g} by default, 0 over layouts",
    )
    parser.add_argument(
        "--cpu",
        type=cpus,
        default=None,
        help="run every bench on these cpus, as 4, 4,5 or 4-7",
    )
    args = parser.parse_args(argv)
    if signed_rank_depth(args.rounds) == 0:
        parser.error(
            f"at least six rounds: fewer have no {CONFIDENCE:.0%} interval, "
            "so they are no measurement"
        )
    if args.cpu is not None:
        if not hasattr(os, "sched_setaffinity"):
            parser.error("--cpu needs a system that can pin a process, like linux")
        # set on this process, so every bench it starts inherits it
        try:
            os.sched_setaffinity(0, args.cpu)
        except (OSError, ValueError) as refused:
            parser.error(f"--cpu {sorted(args.cpu)}: {refused}")

    over_layouts = os.path.isdir(args.base)
    if over_layouts != os.path.isdir(args.candidate):
        parser.error("both sides are binaries or both are directories of layouts")
    # a pair's rates carry both sides' layout as well as the machine's load,
    # and alike when the two builds barely differ, so over layouts the rule
    # would run a round again for the layout it drew. Off unless asked for
    loaded = args.loaded
    if loaded is None:
        loaded = 0.0 if over_layouts else 100 * LOADED
    cut = loaded / 100
    mode = None
    base: Side = args.base
    candidate: Side = args.candidate
    if over_layouts:
        # a replaced round runs on a layout of its own
        needed = args.rounds + budget(args.rounds, cut)
        base_layouts = layouts_in(args.base, needed)
        candidate_layouts = layouts_in(args.candidate, needed)
        if base_layouts.mode != candidate_layouts.mode:
            parser.error(
                f"the base's layouts are {base_layouts.mode} and the "
                f"candidate's {candidate_layouts.mode}"
            )
        mode = base_layouts.mode
        base, candidate = base_layouts.numbered, candidate_layouts.numbered
    threshold = args.threshold
    if threshold is None:
        threshold = LAYOUT_THRESHOLD if mode else THRESHOLD

    measured = measure(base, candidate, args.rounds, args.depth, cut)
    print(f"{'round':>5} {'base nps':>12} {'candidate nps':>14} {'change':>7}")
    for n, b, c in zip(measured.rounds, measured.base_nps, measured.candidate_nps):
        print(f"{n:>5} {b:>12} {c:>14} {change(b, c):>+6.1f}%")
    if measured.replaced:
        print()
        print(f"run again, each pair more than {loaded:g}% below the median pair:")
        for n, b, c in measured.replaced:
            print(f"{n:>5} {b:>12} {c:>14} {change(b, c):>+6.1f}%")
    print()
    for line in summary(measured):
        print(line)
    estimate = paired(measured.base_nps, measured.candidate_nps)
    print()
    print(
        f"paired change {estimate.change:+.1f}%, "
        f"{CONFIDENCE:.0%} interval {interval(estimate)}"
    )
    if mode:
        # the layout each side ships with, for seeing how far this build's
        # own draw sits from the rest. A diagnostic: the verdict and the
        # trailer read the layouts
        rounds = max(6, args.rounds // 3)
        default = measure(
            base_layouts.default, candidate_layouts.default, rounds, args.depth, cut
        )
        on_default = paired(default.base_nps, default.candidate_nps)
        print(
            f"diagnostic, on the default layout alone {on_default.change:+.1f}%, "
            f"{CONFIDENCE:.0%} interval {interval(on_default)}, {rounds} rounds"
        )
    print()
    if measured.base_nodes != measured.candidate_nodes:
        # the rates are then over different trees, and a verdict would read
        # them as if they were not
        print(COUNTS_DIFFER)
    else:
        print(textwrap.fill(verdict(estimate, threshold), width=72))
    # the trailer stays the last line, which is what speed.sh reads
    print()
    print(trailer(measured.base_nps, measured.candidate_nps, args.base_ref, mode))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
