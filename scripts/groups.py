# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2022-2026 Andrew Wright

"""Which of the tuner's three groups a pair of games falls in.

Both halves of the tuner read this: `scripts/build_corpus.py` labels a
position from its own group's games alone, and `scripts/tune.py` puts each
row in its group. Two copies of the mapping would be a corpus built to one
split and fitted against another, with nothing printed about it. A file of
its own because the two scripts share no dependency otherwise.

The unit is the pair rather than the game. A strength run plays every opening
twice with the colours reversed, so the two games share their first moves and
lean on each other's result. `build_corpus.py` writes the pair's key on every
row as the `pair` operand; a game with no partner is a pair of one.
"""

from pathlib import Path

# The five slices of a key, and the group each one is.
SLICES = (
    "train",
    "train",
    "train",
    "selection",
    "calibration",
)

# The slices when the sealed group is named rather than drawn from the key:
# three quarters of what is not named trains and a quarter chooses the ridge.
OPEN_SLICES = (
    "train",
    "train",
    "train",
    "selection",
)

# The group that is not read until the weights are final.
CALIBRATION = "calibration"

# What a pair key is: the hex of a sha256.
KEY_LENGTH = 64
HEX = set("0123456789abcdef")


def sealed_pairs(path):
    """The pair keys a file names, one to a line. Blank lines and anything
    after a hash are skipped; every other line has to be a pair key, since a
    typo that named no pair would be a game quietly not sealed."""
    named = {}
    for number, line in enumerate(
        Path(path).read_text(encoding="utf-8").splitlines(), 1
    ):
        key = line.split("#", 1)[0].strip()
        if not key:
            continue
        if len(key) != KEY_LENGTH or set(key) - HEX:
            raise ValueError(f"{path} line {number} is no pair key: {key!r}")
        named.setdefault(key, number)
    if not named:
        raise ValueError(f"{path} names no pair")
    return set(named)


def group_of(key, sealed=None):
    """Which group a pair falls in: the first byte of its key modulo five, or
    the group `sealed` names it.

    The game is the unit because the label is: every position of a game
    carries its result and consecutive positions are a move apart, so a row
    held out while its neighbours are trained on is a row whose answer the fit
    has seen. Measured on the corpus this was written for (1,809 games), a
    split on positions left 1,805 games with rows on both sides and 53.1% of
    held-out rows with the position a ply away, same game and same label, in
    the training set.

    Three groups rather than two: the ridge is ranked on the selection group,
    so the loss there is the fit's own best case, and a coverage claim needs a
    group no ranking has touched. It is assigned by construction from the
    first run, since one cannot be retrofitted.

    `sealed`, where given, is the set of pair keys that are the calibration
    group, and no pair outside it is. A seal drawn from the key alone seals
    the same games every time an archive is re-harvested, so a vector revised
    after a reading would be read against games already read; naming the
    games played since the last reading is the only way round that. Both
    halves of the tuner have to be handed the same set, or the corpus is
    labelled by one split and fitted against another.
    """
    if len(key) < 2:
        raise ValueError(f"a game key that is no sha256: {key!r}")
    try:
        first = int(key[:2], 16)
    except ValueError:
        raise ValueError(f"a game key that is no sha256: {key!r}") from None
    if sealed is None:
        return SLICES[first % len(SLICES)]
    if key in sealed:
        return CALIBRATION
    return OPEN_SLICES[first % len(OPEN_SLICES)]
