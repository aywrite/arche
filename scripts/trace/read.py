#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2022-2026 Andrew Wright

"""Read what `arche trace` wrote: the manifest and each stream's records.

A stream is a 24 byte header (the tag ARCHETRC, then the version, the stream
number, the record width and a reserved word, each a little endian u32) and
fixed width little endian records. The column layouts below are the ones
arche-core/src/trace.rs writes, and a header naming another version or width
is refused rather than misread.

    read.py <trace directory>

prints each stream's record count, which is also how a directory is checked.
"""

import json
import sys
from pathlib import Path

import numpy as np

VERSION = 1

# the columns of each stream, in record order; "_" fields are padding
LAYOUTS = {
    "nodes": [
        ("node", "<u8"),
        ("parent", "<u8"),
        ("position", "<u2"),
        ("kind", "u1"),
        ("ply", "u1"),
        ("depth", "u1"),
        ("side", "u1"),
        ("castle", "u1"),
        ("en_passant", "u1"),
        ("pieces", "<u8", (6,)),
        ("white", "<u8"),
        ("black", "<u8"),
        ("key", "<u8"),
    ],
    "sliders": [
        ("node", "<u8"),
        ("site", "<u2"),
        ("straight", "u1"),
        ("square", "u1"),
        ("_", "<u4"),
        ("relevant", "<u8"),
        ("result", "<u8"),
    ],
    "swaps": [
        ("list", "<u8"),
        ("node", "<u8"),
        ("from", "u1"),
        ("to", "u1"),
        ("victim", "u1"),
        ("attacker", "u1"),
        ("promote", "u1"),
        ("en_passant", "u1"),
        ("_1", "<u2"),
        ("see", "<i4"),
        ("_2", "<u4"),
    ],
    "lists": [
        ("list", "<u8"),
        ("node", "<u8"),
        ("what", "u1"),
        ("kind", "u1"),
        ("index", "u1"),
        ("from", "u1"),
        ("to", "u1"),
        ("victim", "u1"),
        ("promote", "u1"),
        ("flags", "u1"),
        ("_", "<u8"),
    ],
    "evals": [
        ("node", "<u8"),
        ("eval", "<u8"),
        ("key", "<u8"),
        ("site", "<u2"),
        ("flags", "u1"),
        ("walked", "u1"),
        ("score", "<i2"),
        ("phase", "<u2"),
        ("psqt", "<i4"),
        ("material", "<i4"),
        ("machine", "<i4"),
        ("mobility", "<i4"),
        ("king_attack", "<i4"),
        ("shelter", "<i4"),
        ("pawn_structure", "<i4"),
        ("_", "<u4"),
        # white's mobility counts, black's, white's ring counts, black's,
        # knight, bishop, rook and queen in each
        ("counts", "<u2", (16,)),
    ],
    "walks": [
        ("eval", "<u8"),
        ("attacks", "<u8"),
        ("square", "u1"),
        ("kind", "u1"),
        ("color", "u1"),
        ("scope", "u1"),
        ("ring", "u1"),
        ("_1", "u1"),
        ("_2", "<u2"),
    ],
    "bounds": [
        ("node", "<u8"),
        ("eval", "<u8"),
        ("kind", "u1"),
        ("outcome", "u1"),
        ("_", "<u2"),
        ("threshold", "<i4"),
        ("value", "<i4"),
        ("aux", "<i4"),
    ],
    "attacks": [
        ("node", "<u8"),
        ("site", "<u2"),
        ("query", "u1"),
        ("square", "u1"),
        ("color", "u1"),
        ("_1", "u1"),
        ("_2", "<u2"),
        ("occupied", "<u8"),
        ("result", "<u8"),
    ],
}

KINDS = {0: "root", 1: "full", 2: "quiescence"}
# a `lists` record's `what`
ORDERED, REACHED, CUTOFF = 0, 1, 2
# a piece code, pawn to king, and 6 for none
PIECES = "pnbrqk-"
# an `evals` record's `flags`: the sum answered zero for material that
# cannot mate, it was handed the searcher's tables, and each table held the
# position
DRAWN, CACHED, SHELTER_HIT, PAWNS_HIT = 1, 2, 4, 8
# a `walks` record's `kind`, and the order an `evals` record's counts are in
WALKED = "nbrq"
# a `bounds` record's `kind`
BOUNDS = {
    1: "stand pat beta",
    2: "stand pat alpha",
    3: "delta filter",
    4: "delta loop",
    5: "returned",
    6: "reverse futility",
    7: "null move",
    8: "quiet futility",
    9: "late move gate",
}


def manifest(directory):
    return json.loads((Path(directory) / "manifest.json").read_text())


def stream(directory, name):
    """One stream's records as a numpy structured array."""
    dtype = np.dtype(LAYOUTS[name])
    raw = (Path(directory) / f"{name}.bin").read_bytes()
    tag, rest = raw[:8], raw[8:24]
    version, number, width, _ = np.frombuffer(rest, dtype="<u4")
    if tag != b"ARCHETRC":
        raise ValueError(f"{name}: not a trace stream")
    if version != VERSION:
        raise ValueError(f"{name}: version {version}, this reader knows {VERSION}")
    if width != dtype.itemsize:
        raise ValueError(
            f"{name}: records of {width} bytes, the layout says {dtype.itemsize}"
        )
    return np.frombuffer(raw[24:], dtype=dtype)


def sites(directory):
    """Site number to "file:line:column"."""
    return {
        s["id"]: f"{s['file']}:{s['line']}:{s['column']}"
        for s in manifest(directory)["sites"]
    }


def main(argv):
    if len(argv) != 2:
        print(__doc__.strip().splitlines()[-3].strip(), file=sys.stderr)
        return 2
    directory = argv[1]
    m = manifest(directory)
    print(
        f"{m['engine']}, {m['settings']}: {m['entered']} nodes entered, {m['sampled']} sampled"
    )
    for entry in m["streams"]:
        if entry["name"] not in LAYOUTS:
            print(f"{entry['name']:<8} {entry['records']:>12} records, no layout here to read them")
            continue
        records = stream(directory, entry["name"])
        if len(records) != entry["records"]:
            print(
                f"{entry['name']}: {len(records)} records, the manifest says {entry['records']}"
            )
            return 1
        print(f"{entry['name']:<8} {len(records):>12} records")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
