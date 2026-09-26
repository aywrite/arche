#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2022-2026 Andrew Wright

"""Count the instructions one engine's bench executes against another's.

The count is taken under cachegrind with its cache simulation off, so it is
the number of instructions the bench ran and nothing else. It is the same on
every run to within a few hundred instructions (the clock reads differ), so
one run a side is a measurement, and a change far below what speed.py can
see is still a change here. What it cannot see is everything the hardware
adds to an instruction: cache misses, branch mispredictions and where the
code lands. Read it beside the speed, not in place of it.

    instructions.py <base binary> <candidate binary> [--depth D]
                    [--base-ref SHA] [--valgrind PATH]
"""

import argparse
import re
import subprocess
import sys
from dataclasses import dataclass

# cachegrind's summary line, which it writes to stderr with the pid in front
REFS = re.compile(r"I\s+refs:\s+([\d,]+)")


@dataclass
class Counted:
    instructions: int
    nodes: int


def instructions_in(stderr: str) -> int | None:
    """The instruction count from cachegrind's summary, or nothing when it
    printed none."""
    found = REFS.findall(stderr)
    return int(found[-1].replace(",", "")) if found else None


def nodes_in(stdout: str) -> int | None:
    """The node count from the bench's last line, `N nodes M nps`."""
    lines = stdout.strip().splitlines()
    words = lines[-1].split() if lines else []
    if len(words) != 4 or words[1] != "nodes" or words[3] != "nps":
        return None
    return int(words[0])


def count(valgrind: str, binary: str, depth: int | None) -> Counted:
    command = [
        valgrind,
        "--tool=cachegrind",
        "--cache-sim=no",
        # the per line counts are for annotating by hand, not for this
        "--cachegrind-out-file=/dev/null",
        binary,
        "bench",
    ] + ([str(depth)] if depth is not None else [])
    # stdin closed, so a binary from before the bench existed does not sit in
    # its uci loop waiting
    output = subprocess.run(
        command,
        check=True,
        capture_output=True,
        text=True,
        stdin=subprocess.DEVNULL,
    )
    instructions = instructions_in(output.stderr)
    nodes = nodes_in(output.stdout)
    if instructions is None or nodes is None:
        raise SystemExit(
            f"{binary}: no instruction count or no bench under {valgrind}, "
            f"whose output ended: {output.stderr[-300:]!r}"
        )
    return Counted(instructions, nodes)


def change(base: float, candidate: float) -> str:
    return f"{100.0 * (candidate - base) / base:+.2f}%"


def report(base: Counted, candidate: Counted) -> list[str]:
    """One row a side and the change under it. The per node column is there
    for when the counts differ: the total then moves with the size of the
    tree, and the cost of a node is what the rest of the change is."""
    rows = [
        ("", "instructions", "nodes", "per node"),
        (
            "base",
            f"{base.instructions:,}",
            str(base.nodes),
            f"{base.instructions / base.nodes:.1f}",
        ),
        (
            "candidate",
            f"{candidate.instructions:,}",
            str(candidate.nodes),
            f"{candidate.instructions / candidate.nodes:.1f}",
        ),
        (
            "change",
            change(base.instructions, candidate.instructions),
            change(base.nodes, candidate.nodes)
            if base.nodes != candidate.nodes
            else "",
            change(
                base.instructions / base.nodes,
                candidate.instructions / candidate.nodes,
            ),
        ),
    ]
    widths = [max(len(row[i]) for row in rows) for i in range(4)]
    return [
        (
            f"{row[0]:<{widths[0]}}  "
            + "  ".join(f"{cell:>{width}}" for cell, width in zip(row[1:], widths[1:]))
        ).rstrip()
        for row in rows
    ]


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("base")
    parser.add_argument("candidate")
    parser.add_argument("--depth", type=int, default=None)
    parser.add_argument("--base-ref", default="base")
    parser.add_argument("--valgrind", default="valgrind")
    args = parser.parse_args(argv)

    base = count(args.valgrind, args.base, args.depth)
    candidate = count(args.valgrind, args.candidate, args.depth)
    print(f"instructions against {args.base_ref}, one cachegrind run a side")
    print()
    for line in report(base, candidate):
        print(line)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
