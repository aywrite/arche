#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2022-2026 Andrew Wright

"""Recompute what a trace recorded, from what it recorded as the inputs.

A second implementation, written here from the rules rather than from the
engine's tables, is what finds a record missing an input: the engine's own
function would read the missing input off its board and agree with itself.

- Every magic probe in `sliders` is recomputed by walking the four rays from
  its square through its relevant occupancy, and the relevant occupancy is
  checked to lie inside the square's blocker mask.
- Every position in `nodes` is checked to be a board: the piece boards
  disjoint, their union the two colours' union, one king a side, and the side
  to move, castling and en passant fields in range.
- `attacks` is checked for consistency only (a result inside the occupancy
  passed). Its calls come from inside make_move, where the board is the
  child's and the stream does not record it.

    replay.py <trace directory>

exits 0 when every record agrees and prints the first few that do not.
"""

import sys

import numpy as np

from read import manifest, stream

STRAIGHT = [(1, 0), (-1, 0), (0, 1), (0, -1)]
DIAGONAL = [(1, 1), (1, -1), (-1, 1), (-1, -1)]


def rays(square, occupied, directions):
    """The squares a slider on `square` reaches, each ray including the
    blocker it stops on."""
    file, rank = square % 8, square // 8
    reached = 0
    for df, dr in directions:
        f, r = file + df, rank + dr
        while 0 <= f < 8 and 0 <= r < 8:
            bit = 1 << (r * 8 + f)
            reached |= bit
            if occupied & bit:
                break
            f, r = f + df, r + dr
    return reached


def blocker_mask(square, directions):
    """The squares whose occupancy a probe from `square` reads: each ray
    without its last square, which is reached whether or not it is empty."""
    file, rank = square % 8, square // 8
    mask = 0
    for df, dr in directions:
        f, r = file + df, rank + dr
        while 0 <= f + df < 8 and 0 <= r + dr < 8:
            mask |= 1 << (r * 8 + f)
            f, r = f + df, r + dr
    return mask


def check_sliders(directory, report):
    records = stream(directory, "sliders")
    masks = {
        (straight, square): blocker_mask(square, STRAIGHT if straight else DIAGONAL)
        for straight in (0, 1)
        for square in range(64)
    }
    # a probe is determined by its key, so each distinct key is walked once
    keys, first, inverse = np.unique(
        records[["straight", "square", "relevant"]],
        return_index=True,
        return_inverse=True,
    )
    expected = np.empty(len(keys), dtype=np.uint64)
    bad = 0
    for k, key in enumerate(keys):
        straight, square, relevant = int(key[0]), int(key[1]), int(key[2])
        expected[k] = rays(square, relevant, STRAIGHT if straight else DIAGONAL)
        if relevant & ~masks[(straight, square)]:
            bad += 1
            report(f"sliders record {first[k]}: occupancy outside the blocker mask")
    wrong = np.flatnonzero(records["result"] != expected[inverse.ravel()])
    bad += len(np.unique(inverse.ravel()[wrong]))
    for at in wrong[:10]:
        report(
            f"sliders record {at}: square {int(records['square'][at])}, "
            f"got {int(records['result'][at]):#x}, rays give {int(expected[inverse.ravel()[at]]):#x}"
        )
    return len(records), len(keys), bad


def check_nodes(directory, report):
    records = stream(directory, "nodes")
    bad = 0
    for i, n in enumerate(records):
        pieces = [int(p) for p in n["pieces"]]
        union = 0
        clash = False
        for p in pieces:
            clash |= bool(union & p)
            union |= p
        white, black = int(n["white"]), int(n["black"])
        kings = pieces[5]
        wrong = (
            clash
            or white & black
            or union != white | black
            or bin(kings & white).count("1") != 1
            or bin(kings & black).count("1") != 1
            or n["side"] > 1
            or n["castle"] > 15
            or n["en_passant"] > 64
            or n["kind"] > 2
        )
        if wrong:
            bad += 1
            report(f"nodes record {i}: node {int(n['node'])} is not a board")
    return len(records), bad


def check_attacks(directory, report):
    records = stream(directory, "attacks")
    bad = int(np.count_nonzero(records["result"] & ~records["occupied"]))
    if bad:
        report(f"attacks: {bad} results outside the occupancy passed")
    return len(records), bad


def main(argv):
    if len(argv) != 2:
        print("usage: replay.py <trace directory>", file=sys.stderr)
        return 2
    directory = argv[1]
    shown = []

    def report(line):
        if len(shown) < 10:
            shown.append(line)

    m = manifest(directory)
    print(f"{m['engine']}, {m['settings']}")
    probes, distinct, bad_sliders = check_sliders(directory, report)
    print(
        f"sliders  {probes:>12} records, {distinct} distinct keys, {bad_sliders} keys disagree"
    )
    nodes, bad_nodes = check_nodes(directory, report)
    print(f"nodes    {nodes:>12} records, {bad_nodes} not boards")
    attacks, bad_attacks = check_attacks(directory, report)
    print(f"attacks  {attacks:>12} records, {bad_attacks} inconsistent")
    for line in shown:
        print(line)
    return 1 if bad_sliders or bad_nodes or bad_attacks else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
