# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2022-2026 Andrew Wright

"""Which of the tuner's three groups a pair of games falls in.

Both halves of the tuner read this. `scripts/build_corpus.py` reads it because
a position's label and its weight are taken from its own group's games alone,
so the corpus cannot be written without knowing where each game went.
`scripts/tune.py` reads it because it is what puts a row in the training group,
the selection group or the sealed one. Two copies of the mapping would be a
corpus built to one split and fitted against another, and nothing in either run
would print a word about it.

The unit is the pair rather than the game. A strength run plays every opening
twice with the colours reversed, so the two games share their first moves and
lean on each other's result, and a split that put them in different groups
would hold half an opening out. `build_corpus.py` keys the two together and
writes the pair's key on every row as the `pair` operand; a game with no
partner is a pair of one and its pair key is its own.

A file of its own rather than one script importing the other, because the two
have no dependency in common: the corpus builder reads pgn through
python-chess and the tuner fits in numpy, and neither should have to install
the other's half to read a modulo.
"""

from pathlib import Path

# The five slices of a game key, and which group each one is. Three fifths
# train, a fifth chooses the ridge, and a fifth is sealed.
SLICES = (
    "train",
    "train",
    "train",
    "selection",
    "calibration",
)

# The four slices a pair falls in when the sealed group is named rather than
# drawn from the key: the calibration slice is gone and the rest stand as they
# are, so three quarters of what is not named trains and a quarter chooses the
# ridge.
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
    """The pair keys a file names, one to a line.

    Blank lines are skipped and so is anything after a hash, so the file can
    say which runs its games came from and when it was drawn. Every other line
    has to be a pair key, because a typo that quietly named no pair is a game
    quietly not sealed, and the whole point of naming them is that nobody has
    to take the seal on trust.
    """
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
    """Which group a pair falls in: the first byte of its key, modulo five, or
    the group `sealed` names it.

    The key is the sha256 `build_corpus.py` writes into every row's `pair`
    operand: the two games' movetext keys sorted and joined, or the one game's
    own key where it has no partner. The game is the unit because the label
    is. Every position of a game carries that game's result, and
    consecutive positions are one move apart, so a row held out while its
    neighbours are trained on is a row whose answer the fit has already been
    shown. Splitting on the position rather than the game hides that rather
    than preventing it: measured on the corpus this was written for, 1,805 of
    1,809 games had rows on both sides and 53.1% of the held-out rows had the
    position a ply away, same game and same label, in the training set. What
    the loss then reports is interpolation inside games the fit has seen, which
    is not what a held-out loss is read as.

    Three groups rather than two, because a fifth of the games is worth more
    unread than read. The ridge is ranked on the selection group, which makes
    the loss reported there the fit's own best case; a distribution-free
    coverage claim needs a group that no ranking has touched, and the
    calibration group is it. Retrofitting one later cannot work, so it is
    assigned by construction from the first run.

    `sealed`, where it is given, is the set of pair keys that are the
    calibration group, and no pair outside it is in that group. A seal drawn
    from the key alone cannot be drawn a second time: which pairs it names is
    a property of the pairs, so re-harvesting an archive seals the same games
    again, and a vector revised after a reading would be read against games
    that have already been read. The archive only ever grows, so the games
    that settle that are the ones played since the last reading, and naming
    them is the only way to say so. `tune.py final` still refuses the same
    sealed games twice, by checksum over the keys of whichever group it is
    handed.

    Both halves of the tuner have to be handed the same set. A corpus built
    with one seal and fitted against another would label rows by one group and
    hold out a different one, which is the leak the groups exist to prevent
    wearing the clothes of the thing that prevents it.
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
