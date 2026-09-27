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


SEE_VALUES = [100, 300, 300, 500, 900, 10_000]
KNIGHT = [(1, 2), (2, 1), (2, -1), (1, -2), (-1, -2), (-2, -1), (-2, 1), (-1, 2)]
KING = [(1, 0), (1, 1), (0, 1), (-1, 1), (-1, 0), (-1, -1), (0, -1), (1, -1)]


def steps(square, offsets):
    file, rank = square % 8, square // 8
    out = 0
    for df, dr in offsets:
        f, r = file + df, rank + dr
        if 0 <= f < 8 and 0 <= r < 8:
            out |= 1 << (r * 8 + f)
    return out


def attackers(square, occupied, pieces, white, black):
    """Every piece of either colour bearing on `square` through `occupied`."""
    pawns, knights, bishops, rooks, queens, kings = pieces
    # a white pawn attacks upwards, so the white pawns that bear on a square
    # stand one rank below it, and the black ones one rank above
    white_pawns = steps(square, [(-1, -1), (1, -1)]) & white & pawns
    black_pawns = steps(square, [(-1, 1), (1, 1)]) & black & pawns
    found = white_pawns | black_pawns
    found |= steps(square, KNIGHT) & knights
    found |= steps(square, KING) & kings
    found |= rays(square, occupied, DIAGONAL) & (bishops | queens)
    found |= rays(square, occupied, STRAIGHT) & (rooks | queens)
    return found & occupied


def see(node, swap):
    """The swap on the recorded position, from the rules: the least valuable
    attacker recaptures in turn, sliders behind joining as the line opens,
    a promotion counted as the pawn, en passant lifting the pawn it takes,
    and the fold letting either side stop."""
    pieces = [int(p) for p in node["pieces"]]
    white, black = int(node["white"]), int(node["black"])
    side_white = int(node["side"]) == 1
    src, dst = int(swap["from"]), int(swap["to"])
    gain = [SEE_VALUES[int(swap["victim"])]]
    occupied = (white | black) & ~(1 << src)
    if swap["en_passant"]:
        taken = dst - 8 if side_white else dst + 8
        occupied &= ~(1 << taken)
    on_square = int(swap["attacker"])
    mover_white = not side_white
    while True:
        side_mask = white if mover_white else black
        found = attackers(dst, occupied, pieces, white, black) & side_mask
        if not found:
            break
        for kind in range(6):
            subset = found & pieces[kind]
            if subset:
                bit = subset & -subset
                break
        gain.append(SEE_VALUES[on_square] - gain[-1])
        if on_square == 5:
            break
        occupied &= ~bit
        on_square = kind
        mover_white = not mover_white
    while len(gain) > 1:
        go_on = gain.pop()
        gain[-1] = -max(-gain[-1], go_on)
    return gain[0]


def check_swaps(directory, report):
    """Every swap recomputed on its node's recorded position. The ordering
    runs before the node makes a move, so the position is the node's."""
    swaps = stream(directory, "swaps")
    nodes = stream(directory, "nodes")
    by_node = {int(n["node"]): n for n in nodes}
    bad = 0
    missing = 0
    for i, s in enumerate(swaps):
        node = by_node.get(int(s["node"]))
        if node is None:
            missing += 1
            continue
        expected = see(node, s)
        if expected != int(s["see"]):
            bad += 1
            report(
                f"swaps record {i}: node {int(s['node'])} {int(s['from'])}->{int(s['to'])}, "
                f"recorded {int(s['see'])}, the rules give {expected}"
            )
    return len(swaps), missing, bad


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
    swaps, missing, bad_swaps = check_swaps(directory, report)
    print(
        f"swaps    {swaps:>12} records, {bad_swaps} disagree, {missing} with no node record"
    )
    for line in shown:
        print(line)
    return 1 if bad_sliders or bad_nodes or bad_attacks or bad_swaps or missing else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
