#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2022-2026 Andrew Wright

"""Score a weight vector against the games, and fit a new one.

`arche terms` prints, for each quiet position of a corpus, the coefficient
every evaluation weight is multiplied by. `scripts/build_corpus.py` prints what
the games those positions came from ended in. This joins the two and does what
that pair is for:

    python3 scripts/tune.py loss --terms rows.txt --corpus corpus.epd
    python3 scripts/tune.py cv   --terms rows.txt --corpus corpus.epd
    python3 scripts/tune.py fit  --terms rows.txt --corpus corpus.epd --out fit.json
    python3 scripts/tune.py final --terms rows.txt --corpus corpus.epd \
        --weights fit.json --log final.log
    python3 scripts/tune.py curve --terms rows.txt --corpus corpus.epd --out curve.json

`loss` scores a weight vector over the whole corpus in one matrix-vector
product. `cv` compares ways of fitting by refitting over five folds of whole
games. `fit` is the tuner, and chooses its ridge on the selection group.
`final` opens the sealed group once against a frozen vector, and the log it
appends to refuses a second opening of the same games. `curve` refits on draws
of the training pairs at several sizes, to ask whether the corpus is big
enough.

The unit is the game and not the position. Positions inside one game share a
label and are a move apart, so a split on positions leaves a held-out row's
answer beside it in the training set, and an interval over positions counts
one game's evidence many times. Measured on the 1,809 game corpus this was
written for, a split on positions left 1,805 games with rows on both sides,
made the interval about four times too narrow, and chose a ridge two orders
of magnitude off the one whole games choose; `groups.py` has the rest of that
measurement.

There are three groups: three fifths of the games train, a fifth chooses the
ridge, and a fifth is sealed. The calibration rows are not in the matrices the
loss and the fit read, so no command but `final` can reach one. `final` takes
a vector already quantized to the integers that would ship, logs the corpus,
the sealed games, the extraction and the vector by checksum before it reads
anything, and refuses a corpus or a set of sealed games the log already names.
No sealed game's result reaches a label a fit sees either: `build_corpus.py`
labels a position from its own group's games alone and drops the appearances
in other groups rather than merging them.

The objective is occurrence weighted: a unique position carries the weight of
how many times the corpus reached it, which is the corpus's `count` operand.

Nothing here knows how to evaluate a position. The engine states the
coefficients and the weights, `reconstruct` folds one against the other and
has to give the integer the row says the engine gave, and that is checked on
every row read. Nor does it know where the weights stand: `Layout` reads the
slots off the layout line the run prints. What stays here is `BOUNDS`, the
price of each term against the packed halves.

Three details of the arithmetic a python reader gets wrong: the divide
truncates toward zero where `//` floors, the material is added outside the
divide, and the phase is capped before the coefficients are written. Each has
a test in `scripts/tests/test_tune.py`.

There is no loss-to-elo mapping here. The job is to rank candidates and reject
the ones that cannot help; the sprt says elo.
"""

import argparse
import datetime
import hashlib
import io
import json
import math
import sys
from pathlib import Path

import numpy as np
from groups import CALIBRATION, group_of, sealed_pairs

# The blocks of the vector that are not leaf terms, in the order the layout
# line prints them. Each is a run of slots, not a width per half of the taper.
FIXED_BLOCKS = ("midgame", "endgame", "material")

# The most one side can show of each of a term's counts, which is what a
# weight is priced against in bounds_hold. A term the layout names and this
# does not is refused rather than fitted. One entry per count, so a term that
# grows a count is refused too.
#
# Mobility: one knight, one bishop, one rook and one queen, the twenty seven
# being a queen in the middle of an empty board. Shelter: three pawns on each
# of the five ranks and three files, which the open and half open counts
# share. Pawn structure: eight pawns a side, charged on every count, which is
# loose on the safe side. King attack zone: the most one piece of each kind
# can show on the eight squares round a centred king (a knight reaches two, a
# bishop two and is given three, a rook four, a queen five and is given six).
# A side with two knights shows more, as it does for mobility.
BOUNDS = {
    "mobility": (8, 13, 14, 27),
    "shelter": (3,) * 7,
    "pawn_structure": (8,) * 8,
    "king_attack": (2, 3, 4, 6),
}


class Layout:
    """Where every block of the weight vector stands, read off the layout line
    the run printed. A layout kept here would parse any extraction and price
    its coefficients against weights that stand somewhere else.

    A term's width is its counts per side per half of the taper, so it takes
    twice that in slots, midgame half first. The fixed blocks are stated as
    slots.
    """

    def __init__(self, widths):
        missing = [name for name in FIXED_BLOCKS if name not in widths]
        if missing or list(widths)[: len(FIXED_BLOCKS)] != list(FIXED_BLOCKS):
            raise ValueError(
                "a layout naming {}, where this file expects {} and then the "
                "terms".format(" ".join(widths), " ".join(FIXED_BLOCKS))
            )
        self.widths = dict(widths)
        self.terms = [name for name in widths if name not in FIXED_BLOCKS]
        unpriced = [name for name in self.terms if name not in BOUNDS]
        if unpriced:
            raise ValueError(
                "the run names {}, which this file has no bounds for: a term "
                "has to be priced here before a fit can say whether its "
                "weights stay inside the packed halves".format(" ".join(unpriced))
            )
        mispriced = [name for name in self.terms if len(BOUNDS[name]) != widths[name]]
        if mispriced:
            raise ValueError(
                "the run measures {} in a different number of counts than this "
                "file prices it in, so the bounds are to redo".format(
                    " ".join(mispriced)
                )
            )
        self.start = {}
        slot = 0
        for name, width in widths.items():
            self.start[name] = slot
            slot += width if name in FIXED_BLOCKS else 2 * width
        self.slots = slot

    @classmethod
    def of(cls, line):
        """The layout a run's `layout` line states."""
        words = line.split()[1:]
        if len(words) % 2:
            raise ValueError(f"a layout line with a name and no width: {line}")
        widths = {}
        for name, width in zip(words[::2], words[1::2]):
            if name in widths:
                raise ValueError(f"a layout line naming {name} twice")
            widths[name] = int(width)
        return cls(widths)

    def is_material(self, slot):
        """Whether a slot is a material value, which are the only weights
        `Accumulator::score` adds outside the taper's divide."""
        start = self.start["material"]
        return start <= slot < start + self.widths["material"]

    def block(self, name):
        """The slots a named block holds, as a slice."""
        width = self.widths[name]
        size = width if name in FIXED_BLOCKS else 2 * width
        return slice(self.start[name], self.start[name] + size)

    def check(self, count, what):
        """Refuse a vector of any length but this run's: it is from another
        engine or another extraction."""
        if count != self.slots:
            raise ValueError(f"{what} of {count}, expected {self.slots}")


# What the opening's pieces come to on the taper's scale, which the piece
# square half of a row divides by.
TOTAL_PHASE = 24

# The pieces the phase buckets count: neither pawns nor kings, both colours.
PIECES_COUNTED = "nbrqNBRQ"

# The buckets the game-corpus report stratified by.
BUCKETS = ("0-6", "7-12", "13+")

FOLDS = 5

# The fields of a fen, which a row's reader counts back from.
FEN_FIELDS = 6

# What the header line opens with, in the two shapes `Report`'s `Display`
# writes it. Matched in full rather than on the first word, because an id can
# open with that word too.
HEADERS = ("terms positions ", "terms epd ")

# The counts the header ends with, in the order `Report`'s `Display` writes
# them, read from the right because the suite name in the middle can hold
# anything.
#
# `drawn` is the one that has to be here. A header without it was printed by
# an engine with no drawn material rule, and a fit over its rows would fit an
# evaluation the engine no longer runs. This file cannot find those rows
# without a second copy of the rule, so it refuses the extraction.
HEADER_COUNTS = ("positions", "in_check", "unsettled", "drawn", "kept")

LAYOUT = "layout "

# What the line opens with that says the pair term is on, at what rank and
# scale: `factors 8 64`. Its rows carry the term's score as a fourth number
# after the count.
FACTORS = "factors "


def check_header(line):
    """Refuse a header this engine did not print, and say what it counted."""
    words = line.split()
    named = tuple(words[-2 * len(HEADER_COUNTS) :: 2])
    if named == HEADER_COUNTS:
        return
    raise ValueError(
        "a terms header counting {}, where this file expects {}: it was "
        "printed by an engine whose evaluation differs from the one these "
        "weights are for, and the extraction is to redo".format(
            " ".join(named), " ".join(HEADER_COUNTS)
        )
    )


def no_layout(why):
    """Why an extraction with no layout line is refused: it was printed by an
    engine older than the line, and this file would have to assume a layout."""
    return (
        f"a terms run whose header carries no layout line ({why}): it was printed "
        "by an engine that states no layout, so where its weights stand "
        "cannot be read here and the extraction is to redo"
    )


def trunc_div(numerator, denominator):
    """Rust's integer `/`, which truncates toward zero where python's `//`
    floors; on a negative numerator that does not divide evenly the two differ
    by a centipawn."""
    quotient = abs(numerator) // denominator
    return quotient if numerator >= 0 else -quotient


def reconstruct(coefficients, weights, layout, machine=0):
    """The evaluation a row states, folded back against the weights.

    The material is added outside the divide and not scaled into it. Folding it
    in gives a different integer: `trunc((24 * 1 + -5) / 24)` is 0 where
    `1 + trunc(-5 / 24)` is 1. The pair term is added outside it too, since it
    is not tapered, and it is the engine's own integer.
    """
    material = 0
    numerator = 0
    for slot, coefficient in coefficients:
        product = coefficient * weights[slot]
        if layout.is_material(slot):
            material += product
        else:
            numerator += product
    return material + trunc_div(numerator, TOTAL_PHASE) + machine


def fold_of(key, folds=FOLDS):
    """Which fold a game falls in, by the second byte of its key. The first
    byte is what `groups.py` assigns the group by, and folding on it would
    make one fold the selection group and leave another empty."""
    if len(key) < 4:
        raise ValueError(f"a game key that is no sha256: {key!r}")
    return int(key[2:4], 16) % folds


def phase_bucket(fen):
    """How many pieces that are neither pawns nor kings the position holds, as
    one of three buckets."""
    board = fen.split(" ", 1)[0]
    pieces = sum(board.count(piece) for piece in PIECES_COUNTED)
    if pieces <= 6:
        return BUCKETS[0]
    if pieces <= 12:
        return BUCKETS[1]
    return BUCKETS[2]


def split_row(words, machine=False):
    """The fields of one `arche terms` row, read from its right hand end.

    A row is `id eval phase n slot:coefficient... fen`, and `id eval phase n
    machine slot:coefficient... fen` when the run has a `factors` line. An id
    can hold spaces (the bench names positions "ruy lopez", and a line with no
    id is called by its own fen), so the fields are found from the end whose
    width is fixed: the fen is the last six, the coefficients the run of
    `slot:coefficient` in front of them, and what is left before the numbers
    is the id. The walk back cannot reach into the id because the numbers
    carry no colon, and `n` is held against the run so the two ends have to
    agree.
    """
    numbers = 4 if machine else 3
    if len(words) < FEN_FIELDS + numbers + 1:
        raise ValueError(f"a row of {len(words)} fields: {' '.join(words)!r}")
    head, fen = words[:-FEN_FIELDS], " ".join(words[-FEN_FIELDS:])
    start = len(head)
    while start > 0 and ":" in head[start - 1]:
        start -= 1
    if start < numbers + 1:
        raise ValueError(f"a row whose fields do not line up: {' '.join(words)!r}")
    identifier = " ".join(head[: start - numbers])
    read = [int(word) for word in head[start - numbers : start]]
    evaluation, phase, count = read[:3]
    score = read[3] if machine else 0
    coefficients = []
    for word in head[start:]:
        slot, coefficient = word.split(":")
        coefficients.append((int(slot), int(coefficient)))
    if count != len(coefficients):
        raise ValueError(
            f"{identifier} says {count} coefficients and prints {len(coefficients)}"
        )
    return identifier, evaluation, phase, coefficients, fen, score


class Row:
    """One position: what the engine said it scored, and what of. Which game
    it belongs to is the corpus's to say, not the extraction's."""

    def __init__(self, identifier, evaluation, phase, coefficients, fen, machine=0):
        self.id = identifier
        self.eval = evaluation
        self.phase = phase
        self.coefficients = coefficients
        self.fen = fen
        # the pair term's score, which no weight here moves
        self.machine = machine


def parse_terms(lines):
    """The layout, the weights and the rows of an `arche terms` run.

    Every row is rebuilt from the weights and held against the evaluation it
    states. A row that does not rebuild means this file's arithmetic and the
    engine's have parted company, so it raises rather than dropping the row.
    """
    layout = None
    weights = None
    machine = False
    rows = []
    for line in lines:
        line = line.strip()
        if not line:
            continue
        if line.startswith(HEADERS):
            check_header(line)
            continue
        if line.startswith(LAYOUT):
            layout = Layout.of(line)
            continue
        words = line.split()
        # the line is three words and a row is at least ten, so an id that
        # opens with the word is still a row
        if line.startswith(FACTORS) and len(words) == 3:
            if rows:
                raise ValueError("a factors line after the rows it is about")
            int(words[1]), int(words[2])
            machine = True
            continue
        if words[0] == "weights":
            if layout is None:
                raise ValueError(no_layout("the weights line comes first"))
            count = int(words[1])
            weights = [int(word) for word in words[2 : 2 + count]]
            if len(weights) != count:
                raise ValueError(
                    f"a weights line saying {count} with {len(weights)} on it"
                )
            layout.check(count, "a weights line")
            continue
        if weights is None:
            raise ValueError("a row arrived before the weights line")
        identifier, evaluation, phase, coefficients, fen, score = split_row(
            words, machine
        )
        rebuilt = reconstruct(coefficients, weights, layout, score)
        if rebuilt != evaluation:
            raise ValueError(
                f"{identifier} rebuilds to {rebuilt} and the engine says {evaluation}"
            )
        rows.append(Row(identifier, evaluation, phase, coefficients, fen, score))
    if layout is None:
        raise ValueError(no_layout("there is none"))
    if weights is None:
        raise ValueError("no weights line")
    return layout, weights, rows


class Label:
    """What the corpus says about one position: the game result from the side
    to move's point of view, how many times the corpus reached it, which game
    it belongs to and which pair that game is half of."""

    def __init__(self, result, count, game, pair):
        self.result = result
        self.count = count
        self.game = game
        self.pair = pair


def parse_corpus(lines):
    """The label of each position, by id, read the way the engine reads epd:
    four words of position, then operations ended by semicolons.

    A row with no `game` or no `pair` operand is refused rather than given one
    of its own: a corpus from before either existed would be split into one
    game per position, which is the leak the game split closes.
    """
    labels = {}
    for line in lines:
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        words = line.split()
        operations = {}
        for operation in " ".join(words[4:]).split(";"):
            operation = operation.strip()
            if " " in operation:
                opcode, operands = operation.split(" ", 1)
                operations[opcode] = operands.strip().strip('"')
        if "id" not in operations or "result" not in operations:
            continue
        for operand, what in (("game", "cluster on"), ("pair", "split on")):
            if operand not in operations:
                raise ValueError(
                    f"the corpus row {operations['id']} names no {operand} to {what}"
                )
        labels[operations["id"]] = Label(
            float(operations["result"]),
            int(operations.get("count", 1)),
            operations["game"],
            operations["pair"],
        )
    return labels


class Sealed:
    """The calibration group, which nothing here reads but `final`.

    Its rows are held apart rather than masked out: a mask works while every
    caller remembers it. These rows are not in the corpus's matrices at all,
    so `loss`, `cv` and `fit` cannot reach them by accident, and deleting them
    from the corpus file changes nothing those three print. What a run may say
    about the group is how big it is.
    """

    def __init__(self, layout, rows, labels, weights):
        self._layout = layout
        self._rows = rows
        self._labels = labels
        self._weights = weights

    @property
    def positions(self):
        return len(self._rows)

    @property
    def games(self):
        return len({self._labels[row.id].game for row in self._rows})

    @property
    def pairs(self):
        return len({self._labels[row.id].pair for row in self._rows})

    def checksum(self):
        """The sha256 of the sorted sealed pair keys, which names the sealed
        games apart from the file they came in."""
        keys = sorted({self._labels[row.id].pair for row in self._rows})
        return hashlib.sha256("\n".join(keys).encode("utf-8")).hexdigest()

    @property
    def appearances(self):
        return sum(self._labels[row.id].count for row in self._rows)

    def unseal(self):
        """The rows, loaded the way the corpus loads its own so they can be
        scored."""
        opened = Corpus.__new__(Corpus)
        opened.sealed = None
        opened.layout = self._layout
        opened._load(
            self._weights,
            list(self._rows),
            self._labels,
            dict.fromkeys((row.id for row in self._rows), CALIBRATION),
        )
        return opened


class Corpus:
    """The joined rows, in the shape the loss and the fit read them.

    The coefficients are held as three flat arrays (row, slot, value), a sparse
    matrix without a library, so scoring is one multiply and one `bincount`
    and the gradient the same two the other way round. The tapered
    coefficients and the material ones are kept apart because the material
    half does not divide by the taper.

    The calibration group is not here but in `sealed`.
    """

    def __init__(self, layout, weights, rows, labels, sealed=None):
        self.layout = layout
        joined = [row for row in rows if row.id in labels]
        # build_corpus.py deduplicates by fen before it labels, so no position
        # is in two groups. A corpus that was not deduplicated would be the
        # fen split's leak in a new place, so it is refused. Checked over
        # every row, since a fen in both a training game and the sealed group
        # is the same fault
        first = {}
        for row in joined:
            owner = first.setdefault(row.fen, labels[row.id].game)
            if owner != labels[row.id].game:
                raise ValueError(
                    f"the position {row.fen} is in {owner} and in {labels[row.id].game}"
                )
        groups = {row.id: group_of(labels[row.id].pair, sealed) for row in joined}
        self.sealed_without_rows = 0
        if sealed is not None:
            # a named pair the corpus does not hold at all is a seal drawn
            # against another archive
            absent = sealed - {label.pair for label in labels.values()}
            if absent:
                raise ValueError(
                    f"{len(absent)} sealed pairs are in no game of this "
                    f"corpus, the first being {min(absent)}; the seal was "
                    f"drawn against a different archive"
                )
            # a named pair the corpus holds that reaches no row is ordinary
            # once the archive is large: every position its games saw was
            # claimed by a lower key or filtered out of the extraction. It is
            # sealed and contributes nothing, which is counted, not a fault
            self.sealed_without_rows = len(
                sealed - {labels[row.id].pair for row in joined}
            )
        self.sealed = Sealed(
            layout,
            [row for row in joined if groups[row.id] == CALIBRATION],
            labels,
            weights,
        )
        kept = [row for row in joined if groups[row.id] != CALIBRATION]
        self._load(weights, kept, labels, groups)

    def _load(self, weights, kept, labels, groups):
        """The arrays over one set of rows: the corpus's own, or the sealed
        group's once it is opened."""
        self.rows = kept
        self.weights = np.array(weights, dtype=np.float64)
        self.evals = np.array([row.eval for row in kept], dtype=np.float64)
        self.machine = np.array([row.machine for row in kept], dtype=np.float64)
        self.results = np.array(
            [labels[row.id].result for row in kept], dtype=np.float64
        )
        self.counts = np.array([labels[row.id].count for row in kept], dtype=np.float64)
        self.games = np.array([labels[row.id].game for row in kept])
        self.pairs = np.array([labels[row.id].pair for row in kept])
        self.groups = np.array([groups[row.id] for row in kept])
        self.train = self.groups == "train"
        self.selection = self.groups == "selection"
        self.buckets = np.array([phase_bucket(row.fen) for row in kept])
        tapered, material = [], []
        for index, row in enumerate(kept):
            for slot, coefficient in row.coefficients:
                (material if self.layout.is_material(slot) else tapered).append(
                    (index, slot, coefficient)
                )
        self.tapered = self._arrays(tapered)
        self.material = self._arrays(material)

    @staticmethod
    def _arrays(triples):
        if not triples:
            return (
                np.empty(0, dtype=np.int64),
                np.empty(0, dtype=np.int64),
                np.empty(0, dtype=np.float64),
            )
        index, slot, coefficient = zip(*triples)
        return (
            np.array(index, dtype=np.int64),
            np.array(slot, dtype=np.int64),
            np.array(coefficient, dtype=np.float64),
        )

    def __len__(self):
        return len(self.rows)

    def scores(self, weights):
        """The evaluation of every position under the weights, real valued.
        The truncating divide is at most a centipawn and the fit is not
        sensitive to it; `integer_scores` puts it back."""
        rows, slots, values = self.tapered
        numerator = np.bincount(rows, values * weights[slots], minlength=len(self))
        rows, slots, values = self.material
        material = np.bincount(rows, values * weights[slots], minlength=len(self))
        return material + numerator / TOTAL_PHASE + self.machine

    def integer_scores(self, weights):
        """The same at integer weights with the truncation put back, which is
        the evaluation the engine would give. The divide is done on integers
        toward zero, so this is `trunc_div` a row at a time."""
        weights = np.asarray(weights, dtype=np.int64)
        totals = []
        for rows, slots, values in (self.tapered, self.material):
            products = values.astype(np.int64) * weights[slots]
            totals.append(
                np.bincount(
                    rows, products.astype(np.float64), minlength=len(self)
                ).astype(np.int64)
            )
        numerator, material = totals
        return (
            material
            + np.sign(numerator) * (np.abs(numerator) // TOTAL_PHASE)
            + self.machine.astype(np.int64)
        )

    def scatter(self, per_row):
        """A per-row quantity spread back over the slots: the gradient of
        anything that reads the corpus through `scores`."""
        rows, slots, values = self.tapered
        slot_count = self.layout.slots
        gradient = np.bincount(
            slots, values * per_row[rows] / TOTAL_PHASE, minlength=slot_count
        )
        rows, slots, values = self.material
        return gradient + np.bincount(
            slots, values * per_row[rows], minlength=slot_count
        )

    def support(self, mask=None):
        """How many of the rows each slot appears in, so a weight the corpus
        barely constrains is seen before it ships."""
        counts = np.zeros(self.layout.slots, dtype=np.int64)
        for rows, slots, _ in (self.tapered, self.material):
            picked = slots if mask is None else slots[mask[rows]]
            counts += np.bincount(picked, minlength=self.layout.slots).astype(np.int64)
        return counts


def sigmoid(scores, k):
    """Texel's logistic from a centipawn score to an expected result."""
    return 1.0 / (1.0 + np.power(10.0, -k * scores / 400.0))


def mean_squared_error(scores, results, counts, k):
    predicted = sigmoid(scores, k)
    return float(np.sum(counts * (results - predicted) ** 2) / np.sum(counts))


def log_loss(scores, results, counts, k):
    """The other scoring rule, printed beside the first so a candidate the two
    disagree about is seen."""
    predicted = np.clip(sigmoid(scores, k), 1e-12, 1 - 1e-12)
    terms = results * np.log(predicted) + (1 - results) * np.log(1 - predicted)
    return float(-np.sum(counts * terms) / np.sum(counts))


def fit_k(scores, results, counts, low=0.1, high=4.0, steps=60):
    """The scaling constant, by a golden section search over the training
    split at the shipped weights.

    Fitted once and held for the run. K and the overall scale of the weights
    are one degree of freedom, and the scale is not free: the reverse futility
    margin, the delta margin and the ledger's eval column all assume a pawn is
    about a hundred, so a fit free to rescale would retune all three.
    """
    ratio = (math.sqrt(5.0) - 1.0) / 2.0
    left, right = low, high
    inner_left = right - ratio * (right - left)
    inner_right = left + ratio * (right - left)
    at_left = mean_squared_error(scores, results, counts, inner_left)
    at_right = mean_squared_error(scores, results, counts, inner_right)
    for _ in range(steps):
        if at_left < at_right:
            right, inner_right, at_right = inner_right, inner_left, at_left
            inner_left = right - ratio * (right - left)
            at_left = mean_squared_error(scores, results, counts, inner_left)
        else:
            left, inner_left, at_left = inner_left, inner_right, at_right
            inner_right = left + ratio * (right - left)
            at_right = mean_squared_error(scores, results, counts, inner_right)
    return (left + right) / 2.0


def squared_errors(scores, results, k):
    """The per-position squared error a paired difference is taken over."""
    return (results - sigmoid(scores, k)) ** 2


def weighted_quantile(values, weights, quantile):
    """The smallest value with at least the given share of the weight at or
    under it. No interpolation: the value returned is one the corpus holds."""
    order = np.argsort(values)
    cumulative = np.cumsum(weights[order])
    index = int(np.searchsorted(cumulative, quantile * cumulative[-1]))
    return float(values[order][min(index, len(values) - 1)])


def paired_difference(first, second, counts, games):
    """The mean difference between two weight vectors' per-position errors, its
    standard error taken over the games, the one over the positions, and the
    ratio of the two.

    The two are scored on the same positions, so the difference is a paired
    sample and its mean has an interval; a difference whose interval covers
    zero is not a difference. The interval is the cluster-robust one with the
    game as the cluster, since a game's positions share a result and move
    together. The naive interval is returned too, and the ratio is the design
    factor: near one, the games carried no more dependence than the positions.
    """
    weight = counts / np.sum(counts)
    difference = second - first
    mean = float(np.sum(weight * difference))
    slack = weight * (difference - mean)
    index = np.unique(games, return_inverse=True)[1]
    played = int(index.max()) + 1 if len(index) else 0
    if played < 2:
        return mean, float("nan"), float("nan"), float("nan")
    per_game = np.bincount(index, slack, minlength=played)
    clustered = math.sqrt(float(per_game @ per_game) * played / (played - 1))
    variance = float(np.sum(weight * (difference - mean) ** 2))
    effective = float(np.sum(counts) ** 2 / np.sum(counts**2))
    naive = math.sqrt(variance / effective) if effective > 1 else float("nan")
    design = clustered / naive if naive > 0 else float("nan")
    return mean, clustered, naive, design


def line_search(objective, x, direction, value, slope, length):
    """A step along the direction that lowers the loss, or none.

    Both ways round: the loss is a mean over a hundred thousand positions and
    the gradient per weight is of the order of a hundred thousandth, so the
    first useful step is many times longer than one and a search that only
    backtracks would spend its budget getting there. It steps out while the
    loss keeps falling and halves back when it does not.
    """
    best = None
    for _ in range(60):
        candidate = x + length * direction
        trial, gradient = objective(candidate)
        if trial <= value + 1e-4 * length * slope:
            best = (candidate, trial, gradient, length)
            break
        length /= 2.0
    if best is None:
        return None
    while True:
        length *= 2.0
        candidate = x + length * direction
        trial, gradient = objective(candidate)
        if trial >= best[1] or not np.isfinite(trial):
            return best[:3]
        best = (candidate, trial, gradient, length)


def lbfgs(objective, start, iterations=300, history=10, tolerance=1e-12):
    """L-BFGS on the closed-form gradient, which the evaluation being linear in
    its weights allows; Texel's one-weight-at-a-time walk is not needed."""
    x = np.array(start, dtype=np.float64)
    value, gradient = objective(x)
    olds, news, rhos = [], [], []
    for _ in range(iterations):
        direction = -gradient
        alphas = []
        for step, change, rho in zip(reversed(olds), reversed(news), reversed(rhos)):
            alpha = rho * float(step @ direction)
            alphas.append(alpha)
            direction = direction - alpha * change
        if olds:
            scale = float(olds[-1] @ news[-1]) / float(news[-1] @ news[-1])
            direction = direction * scale
        for step, change, rho, alpha in zip(olds, news, rhos, reversed(alphas)):
            beta = rho * float(change @ direction)
            direction = direction + step * (alpha - beta)
        slope = float(gradient @ direction)
        if slope >= 0:
            direction = -gradient
            slope = float(gradient @ direction)
        norm = float(np.linalg.norm(direction))
        if norm == 0.0:
            break
        stepped = line_search(
            objective, x, direction, value, slope, 1.0 if olds else 1.0 / norm
        )
        if stepped is None:
            break
        candidate, trial, trial_gradient = stepped
        step = candidate - x
        change = trial_gradient - gradient
        curvature = float(step @ change)
        if curvature > 1e-16:
            olds.append(step)
            news.append(change)
            rhos.append(1.0 / curvature)
            if len(olds) > history:
                olds.pop(0)
                news.pop(0)
                rhos.pop(0)
        improvement = value - trial
        x, value, gradient = candidate, trial, trial_gradient
        if improvement <= tolerance:
            break
    return x, value


def objective_for(corpus, mask, k, start, penalty, frozen):
    """The loss and its gradient at a weight vector, over the rows the mask
    picks, with a ridge toward the shipped weights.

    Toward the shipped weights and not toward zero: that leaves alone the one
    direction the corpus cannot see (a constant on both king tables, which
    cancels between the colours) and holds the slots with no support (the pawn
    tables' back ranks) at the zeroes they already are.
    """
    results = corpus.results[mask]
    counts = corpus.counts[mask]
    total = np.sum(counts)
    scale = k * math.log(10.0) / 400.0
    slot_count = corpus.layout.slots
    rows, slots, values = corpus.tapered
    material_rows, material_slots, material_values = corpus.material
    picked = mask[rows]
    material_picked = mask[material_rows]
    renumber = np.cumsum(mask) - 1
    tapered_part = (renumber[rows[picked]], slots[picked], values[picked])
    material_part = (
        renumber[material_rows[material_picked]],
        material_slots[material_picked],
        material_values[material_picked],
    )
    size = int(np.sum(mask))
    machine = corpus.machine[mask]

    def scores_of(weights):
        numerator = np.bincount(
            tapered_part[0], tapered_part[2] * weights[tapered_part[1]], minlength=size
        )
        material = np.bincount(
            material_part[0],
            material_part[2] * weights[material_part[1]],
            minlength=size,
        )
        return material + numerator / TOTAL_PHASE + machine

    def objective(weights):
        scores = scores_of(weights)
        predicted = sigmoid(scores, k)
        residual = results - predicted
        value = float(np.sum(counts * residual**2) / total)
        slack = weights - start
        value += penalty * float(slack[~frozen] @ slack[~frozen])
        per_row = -2.0 * counts * residual * scale * predicted * (1 - predicted) / total
        gradient = np.bincount(
            tapered_part[1],
            tapered_part[2] * per_row[tapered_part[0]] / TOTAL_PHASE,
            minlength=slot_count,
        ) + np.bincount(
            material_part[1],
            material_part[2] * per_row[material_part[0]],
            minlength=slot_count,
        )
        gradient += 2.0 * penalty * np.where(frozen, 0.0, slack)
        gradient[frozen] = 0.0
        return value, gradient

    return objective


def quantize(weights):
    """Round to nearest; what a rounded vector costs is measured, not assumed."""
    return np.rint(np.asarray(weights)).astype(np.int64)


def bounds_hold(weights, layout):
    """Whether a quantized vector stays inside what the engine's arithmetic
    can carry: each half of a packed pair is an `i16` and a boardful of them
    is summed into one. Priced as a one-sided boardful, both colours, with
    every leaf term charged at what `BOUNDS` says a side can show.

    A screen rather than a proof: a side that promoted could cover more, and
    the charges are loose (the worst board comes to 313 squares against the 62
    mobility charges, open and half open files are three between them rather
    than three each, eight pawns cannot fill the pawn term's sixty four).
    Over the 1,809 positions of the three suites the largest one-sided
    mobility difference was 37, so at centipawn weights nothing is near the
    sixteen bits; what this catches is a vector that has gone somewhere else
    entirely.
    """
    weights = np.abs(np.asarray(weights))
    tables = weights[: layout.start["material"]]
    midgame = tables[: layout.widths["midgame"]].reshape(6, 64)
    endgame = tables[layout.widths["midgame"] :].reshape(6, 64)
    worst = 2 * max(int(midgame.max(axis=0).sum()), int(endgame.max(axis=0).sum()))
    for name in layout.terms:
        halves = weights[layout.block(name)].reshape(2, layout.widths[name])
        worst += 2 * int((halves * np.array(BOUNDS[name])).sum(axis=1).max())
    return worst < 32767, worst


def read_weights(path, layout):
    weights = json.loads(Path(path).read_text(encoding="utf-8"))
    layout.check(len(weights), f"{path} holds a vector")
    return np.array(weights, dtype=np.float64)


def table_scale(weights, shipped, layout):
    """How much larger the table entries have grown, as the ratio of their
    root mean squares. Material anchors the pawn at a hundred, but the tables
    are free to grow against it, and a fit given enough licence will spend
    the loss on the scale rather than the shape.
    """
    fitted = np.asarray(weights)[: layout.start["material"]]
    before = np.asarray(shipped)[: layout.start["material"]]
    return float(
        math.sqrt(float(fitted @ fitted))
        / max(math.sqrt(float(before @ before)), 1e-12)
    )


def scored(corpus, weights, mask, k):
    """One weight vector's numbers over one side of the split: the two losses
    at real weights, and the same at rounded ones."""
    scores = corpus.scores(np.asarray(weights, dtype=np.float64))[mask]
    integers = corpus.integer_scores(quantize(weights))[mask].astype(np.float64)
    results, counts = corpus.results[mask], corpus.counts[mask]
    return {
        "mse": mean_squared_error(scores, results, counts, k),
        "log": log_loss(scores, results, counts, k),
        "quantized_mse": mean_squared_error(integers, results, counts, k),
        "quantized_log": log_loss(integers, results, counts, k),
        "scores": scores,
        "integers": integers,
    }


def report(corpus, named, k, out=None):
    """The loss table: each weight vector on the training and selection groups,
    pooled and stratified, with a paired difference against the first. The
    sealed group is named with its size and left alone.
    """
    out = sys.stdout if out is None else out
    train, selection = corpus.train, corpus.selection
    print(
        f"corpus positions {len(corpus)} train {int(train.sum())} "
        f"selection {int(selection.sum())} appearances {int(corpus.counts.sum())}",
        file=out,
    )
    print(
        f"games {len(np.unique(corpus.games)) + corpus.sealed.games} "
        f"train {len(np.unique(corpus.games[train]))} "
        f"selection {len(np.unique(corpus.games[selection]))} "
        f"calibration {corpus.sealed.games}",
        file=out,
    )
    print(
        f"pairs {len(np.unique(corpus.pairs)) + corpus.sealed.pairs} "
        f"train {len(np.unique(corpus.pairs[train]))} "
        f"selection {len(np.unique(corpus.pairs[selection]))} "
        f"calibration {corpus.sealed.pairs}",
        file=out,
    )
    print(
        f"calibration positions {corpus.sealed.positions} "
        f"appearances {corpus.sealed.appearances} sealed, not read here",
        file=out,
    )
    if corpus.sealed_without_rows:
        print(
            f"calibration pairs {corpus.sealed_without_rows} named and "
            "reaching no row of the extraction, so sealed and contributing "
            "nothing",
            file=out,
        )
    wins = float(np.sum(corpus.counts * corpus.results) / np.sum(corpus.counts))
    draws = float(
        np.sum(corpus.counts * (corpus.results == 0.5)) / np.sum(corpus.counts)
    )
    print(f"results mean {wins:.4f} drawn {100 * draws:.1f}%", file=out)
    appearances = max(float(corpus.counts.sum()), 1.0)
    for bucket in BUCKETS:
        share = corpus.buckets == bucket
        # of the appearances, which is what the loss weights by
        seen = float(corpus.counts[share].sum())
        print(
            f"pieces {bucket} positions {int(share.sum())} "
            f"appearances {int(seen)} ({100.0 * seen / appearances:.1f}%)",
            file=out,
        )
    print(f"k {k:.4f}", file=out)
    baseline = None
    for name, weights in named:
        # K refitted for this vector alone says whether a fit bought shape or
        # only scale; every other number holds K so it means the same thing
        # for every vector
        own = fit_k(
            corpus.scores(np.asarray(weights, dtype=np.float64))[train],
            corpus.results[train],
            corpus.counts[train],
        )
        print(
            f"{name} scale {table_scale(weights, corpus.weights, corpus.layout):.3f} "
            f"own_k {own:.4f} "
            f"selection mse at own_k "
            f"{mean_squared_error(corpus.scores(np.asarray(weights, dtype=np.float64))[selection], corpus.results[selection], corpus.counts[selection], own):.6f}",
            file=out,
        )
        for side, mask in (("train", train), ("selection", selection)):
            numbers = scored(corpus, weights, mask, k)
            print(
                f"{name} {side} mse {numbers['mse']:.6f} log {numbers['log']:.6f} "
                f"quantized_mse {numbers['quantized_mse']:.6f} "
                f"quantized_log {numbers['quantized_log']:.6f}",
                file=out,
            )
            if side == "selection":
                for bucket in BUCKETS:
                    inside = corpus.buckets[mask] == bucket
                    if not inside.any():
                        continue
                    print(
                        f"{name} selection pieces {bucket} "
                        f"{int(corpus.counts[mask][inside].sum())} mse "
                        f"{mean_squared_error(numbers['scores'][inside], corpus.results[mask][inside], corpus.counts[mask][inside], k):.6f}",
                        file=out,
                    )
                errors = squared_errors(numbers["scores"], corpus.results[mask], k)
                if baseline is None:
                    baseline = (name, errors)
                else:
                    mean, error, naive, design = paired_difference(
                        baseline[1],
                        errors,
                        corpus.counts[mask],
                        corpus.games[mask],
                    )
                    print(
                        f"{name} against {baseline[0]} selection mse {mean:+.6f} "
                        f"se {error:.6f} per position {naive:.6f} design {design:.1f}"
                        + ("" if abs(mean) > 2 * error else " (inside its interval)"),
                        file=out,
                    )
    counts = corpus.support(train)
    empty = int(np.sum(counts == 0))
    print(
        f"support least {int(counts.min())} median {int(np.median(counts))} "
        f"most {int(counts.max())} unsupported {empty}",
        file=out,
    )


def cross_validate(corpus, penalties, start, frozen, iterations, out=None):
    """Cross validation with whole games held out.

    Five folds of the training and selection games (the corpus handed in does
    not hold the sealed rows). Each fold refits on four fifths and is scored
    on the fifth, so every row is scored once by a fit that never read its
    game. K is fitted per fold on that fold's training games at the shipped
    weights. This is not what the fit chooses its ridge on; it asks the wider
    question of whether a way of fitting is worth anything, with every row
    scored rather than a fifth of them.

    Returns the per-row squared error of each recipe on its held-out fold,
    keyed by penalty with the shipped weights under `shipped`, and the
    penalties whose fits outgrew the packed halves.
    """
    out = sys.stdout if out is None else out
    which = np.array([fold_of(game) for game in corpus.games])
    errors = {name: np.zeros(len(corpus)) for name in ("shipped", *penalties)}
    outside = set()
    for index in range(FOLDS):
        held = which == index
        train = ~held
        if not held.any() or not train.any():
            raise SystemExit(f"tune.py: fold {index} is empty, so the corpus is small")
        k = fit_k(
            corpus.scores(corpus.weights)[train],
            corpus.results[train],
            corpus.counts[train],
        )
        # printed before the fits, which take a while
        print(
            f"fold {index} games {len(np.unique(corpus.games[held]))} "
            f"positions {int(held.sum())} k {k:.4f}",
            file=out,
            flush=True,
        )
        results = corpus.results[held]
        errors["shipped"][held] = squared_errors(
            corpus.scores(corpus.weights)[held], results, k
        )
        for penalty in penalties:
            fitted, _ = lbfgs(
                objective_for(corpus, train, k, start, penalty, frozen),
                start,
                iterations,
            )
            if not bounds_hold(quantize(fitted), corpus.layout)[0]:
                outside.add(penalty)
            errors[penalty][held] = squared_errors(
                corpus.scores(fitted)[held], results, k
            )
    return errors, outside


def cross_validated(corpus, errors, outside, out=None):
    """The cross validated loss of each recipe, and what it bought over the
    shipped weights, with the interval taken over the games. Returns the best
    penalty the engine's arithmetic can carry, or none.
    """
    out = sys.stdout if out is None else out
    weight = corpus.counts / np.sum(corpus.counts)
    print(f"shipped cv mse {float(weight @ errors['shipped']):.6f}", file=out)
    best = None
    for penalty in (name for name in errors if name != "shipped"):
        loss = float(weight @ errors[penalty])
        mean, error, naive, design = paired_difference(
            errors["shipped"], errors[penalty], corpus.counts, corpus.games
        )
        refused = penalty in outside
        print(
            f"penalty {penalty:g} cv mse {loss:.6f} vs shipped {mean:+.6f} "
            f"se {error:.6f} per position {naive:.6f} design {design:.1f}"
            + ("" if abs(mean) > 2 * error else " (inside its interval)")
            + (" (outside the packed halves)" if refused else ""),
            file=out,
        )
        if not refused and (best is None or loss < best[0]):
            best = (loss, penalty)
    return None if best is None else best[1]


def choose_penalty(
    corpus, penalties, start, frozen, iterations, k, out=None, train=None
):
    """The ridge, chosen on the selection group.

    Every penalty on the grid is fitted on the training games and scored on
    the selection games. The lowest selection loss wins, and a fit whose
    tables outgrew the packed halves is no candidate whatever it scores.
    `train` is the rows to fit on in place of the training group, which is
    what the learning curve moves. The loss reported on the selection group
    afterwards is the fit's own best case; an honest interval on a final
    vector comes from the sealed group.

    Returns the chosen penalty and the vector it fitted, or none and none.
    """
    out = sys.stdout if out is None else out
    train = corpus.train if train is None else train
    shipped = squared_errors(
        corpus.scores(corpus.weights)[corpus.selection],
        corpus.results[corpus.selection],
        k,
    )
    counts = corpus.counts[corpus.selection]
    weight = counts / np.sum(counts)
    print(f"shipped selection mse {float(weight @ shipped):.6f}", file=out)
    best = None
    for penalty in penalties:
        fitted, _ = lbfgs(
            objective_for(corpus, train, k, start, penalty, frozen),
            start,
            iterations,
        )
        errors = squared_errors(
            corpus.scores(fitted)[corpus.selection],
            corpus.results[corpus.selection],
            k,
        )
        loss = float(weight @ errors)
        mean, error, naive, design = paired_difference(
            shipped, errors, counts, corpus.games[corpus.selection]
        )
        refused = not bounds_hold(quantize(fitted), corpus.layout)[0]
        print(
            f"penalty {penalty:g} selection mse {loss:.6f} vs shipped {mean:+.6f} "
            f"se {error:.6f} per position {naive:.6f} design {design:.1f}"
            + ("" if abs(mean) > 2 * error else " (inside its interval)")
            + (" (outside the packed halves)" if refused else ""),
            file=out,
            flush=True,
        )
        if not refused and (best is None or loss < best[0]):
            best = (loss, penalty, fitted)
    return (None, None) if best is None else (best[1], best[2])


# The sizes the learning curve fits at, as shares of the training pairs, and
# how many independent draws each size below the whole gets. The spread
# between draws is what says whether the curve's shape is real.
CURVE_SHARES = (0.125, 0.25, 0.5, 0.75, 1.0)
CURVE_DRAWS = 5


def curve_shares(shares, draws):
    """The shares a curve fits at, sorted and deduplicated, each in (0, 1].
    Refused rather than clamped."""
    kept = sorted({float(share) for share in shares})
    if not kept or kept[0] <= 0.0 or kept[-1] > 1.0:
        raise SystemExit(
            "tune.py: a share is a fraction of the training pairs in (0, 1]"
        )
    if draws < 1:
        raise SystemExit("tune.py: a curve needs at least one draw at each share")
    return kept


def learning_curve(
    corpus,
    shares,
    draws,
    penalties,
    start,
    frozen,
    iterations,
    k,
    seed,
    out=None,
):
    """Held-out loss against the number of pairs it was fitted on.

    The training pool is drawn by pair, the independent unit, and each draw
    is taken afresh from the whole pool. The selection group is fixed and
    every fit is read on it. Nothing else moves: the ridge is ranked as `fit`
    ranks it, and K is one number for every fit, since a K refitted per draw
    would let a smaller draw change the scale as well as the tables.

    Each fit is recorded with the pairs it drew, its penalty, its selection
    loss at real and at integer weights, and the paired difference against
    the shipped weights clustered on the game. The spread between draws says
    whether the shape is real; the interval within a draw says whether that
    draw beat the shipped weights.
    """
    out = sys.stdout if out is None else out
    pairs = np.unique(corpus.pairs[corpus.train])
    generator = np.random.default_rng(seed)
    selection = corpus.selection
    shipped = squared_errors(
        corpus.scores(corpus.weights)[selection], corpus.results[selection], k
    )
    counts = corpus.counts[selection]
    weight = counts / np.sum(counts)
    print(
        f"curve training pairs {len(pairs)} games "
        f"{len(np.unique(corpus.games[corpus.train]))} positions "
        f"{int(corpus.train.sum())} selection positions {int(selection.sum())} "
        f"k {k:.4f} seed {seed}",
        file=out,
    )
    print(f"shipped selection mse {float(weight @ shipped):.6f}", file=out)
    fits = []
    for share in shares:
        size = len(pairs) if share >= 1.0 else max(1, round(len(pairs) * share))
        for draw in range(1 if share >= 1.0 else draws):
            chosen = (
                pairs if share >= 1.0 else generator.choice(pairs, size, replace=False)
            )
            train = corpus.train & np.isin(corpus.pairs, chosen)
            penalty, fitted = choose_penalty(
                corpus,
                penalties,
                start,
                frozen,
                iterations,
                k,
                out=io.StringIO(),
                train=train,
            )
            record = {
                "share": share,
                "draw": draw,
                "pairs": int(size),
                "games": len(np.unique(corpus.games[train])),
                "positions": int(train.sum()),
                "appearances": int(corpus.counts[train].sum()),
                "chosen": sorted(str(pair) for pair in chosen),
            }
            heading = (
                f"share {share:g} draw {draw} pairs {size} games {record['games']} "
                f"positions {record['positions']}"
            )
            if penalty is None:
                record["refused"] = True
                print(f"{heading} every penalty outgrew the packed halves", file=out)
                fits.append(record)
                continue
            errors = squared_errors(
                corpus.scores(fitted)[selection], corpus.results[selection], k
            )
            integers = corpus.integer_scores(quantize(fitted))[selection]
            mean, error, naive, design = paired_difference(
                shipped, errors, counts, corpus.games[selection]
            )
            record.update(
                {
                    "penalty": float(penalty),
                    "selection_mse": float(weight @ errors),
                    "quantized_mse": mean_squared_error(
                        integers.astype(np.float64),
                        corpus.results[selection],
                        counts,
                        k,
                    ),
                    "vs_shipped": mean,
                    "se": error,
                    "per_position": naive,
                    "design": design,
                }
            )
            fits.append(record)
            print(
                f"{heading} penalty {penalty:g} selection mse "
                f"{record['selection_mse']:.6f} quantized_mse "
                f"{record['quantized_mse']:.6f} vs shipped {mean:+.6f} "
                f"se {error:.6f} design {design:.1f}",
                file=out,
                flush=True,
            )
    summary = []
    for share in shares:
        drawn = [fit for fit in fits if fit["share"] == share]
        scored_fits = [fit for fit in drawn if "selection_mse" in fit]
        row = {
            "share": share,
            "pairs": drawn[0]["pairs"],
            "draws": len(scored_fits),
            "refused": len(drawn) - len(scored_fits),
        }
        if scored_fits:
            losses = [fit["selection_mse"] for fit in scored_fits]
            row.update(
                {
                    "mean_mse": float(np.mean(losses)),
                    "least_mse": float(min(losses)),
                    "most_mse": float(max(losses)),
                    "mean_se": float(np.mean([fit["se"] for fit in scored_fits])),
                }
            )
        summary.append(row)
    print("share pairs draws refused mean_mse least_mse most_mse mean_se", file=out)
    for row in summary:
        numbers = (
            f"{row['mean_mse']:.6f} {row['least_mse']:.6f} {row['most_mse']:.6f} "
            f"{row['mean_se']:.6f}"
            if "mean_mse" in row
            else "- - - -"
        )
        print(
            f"{row['share']:g} {row['pairs']} {row['draws']} {row['refused']} {numbers}",
            file=out,
        )
    return fits, summary


def check_holds(held, terms):
    """Refuse a `--hold` that names neither the tables nor a leaf term this run
    has, and say what it could have named."""
    unknown = [name for name in held if name != "tables" and name not in terms]
    if unknown:
        raise SystemExit(
            "tune.py: a hold of {}, where a hold is tables or one of {}".format(
                " ".join(unknown), " ".join(terms)
            )
        )


def holds_offered(lines):
    """The leaf terms an extraction's layout line names, taken off the line
    rather than a parsed `Layout` so a mistyped hold is refused before the rows
    are read. Rebuilding every row is the longest thing a run does before it
    prints anything, and a hold it will refuse should not wait on it.

    `None` where the head states no layout these names can be read off, which
    leaves every refusal an extraction has earned to `parse_terms` and its
    words.

    What comes back is what the line offers a hold of and not a list of holds
    that all work: the names are the line's and the prices are `BOUNDS`', so a
    term nobody has priced is offered here and refused by `Layout` a moment
    later, along with the rest of the run.
    """
    for line in lines:
        line = line.strip()
        if line.startswith(LAYOUT):
            words = line.split()[1:]
            if len(words) % 2:
                return None
            return [name for name in words[::2] if name not in FIXED_BLOCKS]
        if line.split()[:1] == ["weights"]:
            break
    return None


def load(args):
    lines = Path(args.terms).read_text(encoding="utf-8").splitlines()
    offered = holds_offered(lines)
    if offered is not None:
        check_holds(getattr(args, "hold", ()), offered)
    layout, weights, rows = parse_terms(lines)
    labels = parse_corpus(Path(args.corpus).read_text(encoding="utf-8").splitlines())
    sealed = sealed_pairs(args.sealed) if getattr(args, "sealed", None) else None
    corpus = Corpus(layout, weights, rows, labels, sealed)
    if not len(corpus):
        raise SystemExit("tune.py: no row of the extraction is in the corpus")
    for name, mask in (("train", corpus.train), ("selection", corpus.selection)):
        if not mask.any():
            raise SystemExit(f"tune.py: no game of the corpus is in the {name} group")
    return corpus


def command_loss(args):
    corpus = load(args)
    k = args.k or fit_k(
        corpus.scores(corpus.weights)[corpus.train],
        corpus.results[corpus.train],
        corpus.counts[corpus.train],
    )
    named = [("shipped", corpus.weights)]
    for path in args.weights or []:
        named.append((Path(path).stem, read_weights(path, corpus.layout)))
    report(corpus, named, k)
    return 0


def frozen_slots(layout, free_material, held=()):
    """Which weights a fit holds where they are.

    Material is held unless freed: the delta margin in quiescence reads
    `eval::material`, so moving it changes the tree for a reason that has
    nothing to do with the evaluation's accuracy. The material block alone; a
    freeze that ran to the end of the vector would hold every leaf term at
    zero and print a null result.

    `held` is names, not positions: `tables` for the two piece square blocks
    together, or one of the leaf terms the layout line names after material.
    Each holds the block this run's layout gives it, so a term added to the
    evaluation is holdable with no edit here. It still has to be priced in
    `BOUNDS` before any fit of it runs, which is `Layout`'s refusal and not
    this one's.

    Each term earns a hold as it is fitted, and nothing is held that the caller
    did not name. A fit of the newest term names every hold below it, so a
    shelter fit passes `--hold tables --hold mobility`, a pawn structure fit
    adds `--hold shelter` and a king attack fit adds `--hold pawn_structure`.
    Holding the tables but not mobility through a shelter fit refits mobility
    beside the shelter: `1b0862a` found half of the king safety fit's apparent
    gain to be that. A refit of an older term holds the newer terms too, which
    is what naming `pawn_structure` or `king_attack` on a mobility fit is for.
    """
    check_holds(held, layout.terms)
    frozen = np.zeros(layout.slots, dtype=bool)
    if not free_material:
        frozen[layout.block("material")] = True
    for name in held:
        if name == "tables":
            frozen[layout.block("midgame")] = True
            frozen[layout.block("endgame")] = True
        else:
            frozen[layout.block(name)] = True
    return frozen


def command_cv(args):
    corpus = load(args)
    start = corpus.weights.copy()
    frozen = frozen_slots(corpus.layout, args.free_material, args.hold)
    errors, outside = cross_validate(
        corpus, args.penalties, start, frozen, args.iterations
    )
    chosen = cross_validated(corpus, errors, outside)
    # said in full so it is not pasted into `fit --penalties` as the ridge the
    # fit would have picked
    print(
        "no penalty on the grid left the tables small enough"
        if chosen is None
        else f"best penalty over the folds {chosen:g} "
        "(fit chooses on the selection group instead)"
    )
    return 0


def command_fit(args):
    corpus = load(args)
    start = corpus.weights.copy()
    frozen = frozen_slots(corpus.layout, args.free_material, args.hold)
    k = args.k or fit_k(
        corpus.scores(corpus.weights)[corpus.train],
        corpus.results[corpus.train],
        corpus.counts[corpus.train],
    )
    penalty, fitted = choose_penalty(
        corpus, args.penalties, start, frozen, args.iterations, k
    )
    if penalty is None:
        raise SystemExit("tune.py: every penalty on the grid left the tables too large")
    print(f"chose penalty {penalty:g}")
    rounded = quantize(fitted)
    _, worst = bounds_hold(rounded, corpus.layout)
    if args.out:
        Path(args.out).write_text(
            json.dumps([int(value) for value in rounded]),
            encoding="utf-8",
            newline="\n",
        )
    report(corpus, [("shipped", corpus.weights), ("fitted", fitted)], k)
    print(f"boardful {worst} of 32767")
    moved = rounded - quantize(corpus.weights)
    print(
        f"moved slots {int(np.sum(moved != 0))} largest {int(np.abs(moved).max())} "
        f"mean {float(np.abs(moved).mean()):.2f}"
    )
    return 0


def sha256_of(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def opened_before(log, corpus_sha, sealed_sha):
    """The log line that says these sealed games were opened, if one is there.
    A line is `opened <when> corpus <sha256> sealed <sha256> ...`, and either
    checksum matching is a match: a renamed file is the same corpus, and a
    re-extraction with a run appended is the same sealed group."""
    if not log.is_file():
        return None
    for line in log.read_text(encoding="utf-8").splitlines():
        words = line.split()
        named = dict(zip(words[2::2], words[3::2])) if words[:1] == ["opened"] else {}
        if named.get("corpus") == corpus_sha or named.get("sealed") == sealed_sha:
            return line
    return None


def command_curve(args):
    corpus = load(args)
    shares = curve_shares(args.shares, args.draws)
    start = corpus.weights.copy()
    frozen = frozen_slots(corpus.layout, args.free_material, args.hold)
    k = args.k or fit_k(
        corpus.scores(corpus.weights)[corpus.train],
        corpus.results[corpus.train],
        corpus.counts[corpus.train],
    )
    fits, summary = learning_curve(
        corpus,
        shares,
        args.draws,
        args.penalties,
        start,
        frozen,
        args.iterations,
        k,
        args.seed,
    )
    if args.out:
        Path(args.out).write_text(
            json.dumps(
                {
                    "k": k,
                    "seed": args.seed,
                    "penalties": args.penalties,
                    "fits": fits,
                    "summary": summary,
                },
                indent=1,
            ),
            encoding="utf-8",
            newline="\n",
        )
    return 0


def command_final(args):
    """Open the sealed group once, against a frozen vector.

    The order matters: the vector is checked to be integers, the log is
    checked and the line written, and only then is a sealed row loaded, so a
    run that opened the group and then failed has still said so. A second
    reading of the same sealed games is refused whatever file they arrive in.
    """
    corpus = load(args)
    frozen = read_weights(args.weights, corpus.layout)
    if np.any(frozen != np.rint(frozen)):
        raise SystemExit(
            "tune.py: the frozen vector is not the integers that would ship; "
            "quantize it first"
        )
    frozen = frozen.astype(np.int64)
    holds, worst = bounds_hold(frozen, corpus.layout)
    if not holds:
        raise SystemExit(f"tune.py: the frozen vector's boardful is {worst} of 32767")
    corpus_sha = sha256_of(args.corpus)
    sealed_sha = corpus.sealed.checksum()
    log = Path(args.log)
    before = opened_before(log, corpus_sha, sealed_sha)
    if before is not None:
        raise SystemExit(
            f"tune.py: these sealed games were opened before ({before}); "
            "a second reading needs sealed games this corpus did not hold"
        )
    if not corpus.sealed.positions:
        raise SystemExit("tune.py: no row of the extraction is in the sealed group")
    k = args.k or fit_k(
        corpus.scores(corpus.weights)[corpus.train],
        corpus.results[corpus.train],
        corpus.counts[corpus.train],
    )
    # logged before a sealed row is loaded
    when = datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    with log.open("a", encoding="utf-8", newline="\n") as handle:
        handle.write(
            f"opened {when} corpus {corpus_sha} sealed {sealed_sha} "
            f"terms {sha256_of(args.terms)} weights {sha256_of(args.weights)} "
            f"k {k:.4f} positions {corpus.sealed.positions} "
            f"games {corpus.sealed.games} appearances {corpus.sealed.appearances}\n"
        )
    opened = corpus.sealed.unseal()
    print(
        f"calibration positions {len(opened)} games {len(np.unique(opened.games))} "
        f"pairs {len(np.unique(opened.pairs))} appearances {int(opened.counts.sum())} "
        f"opened, logged to {log}"
    )
    print(f"k {k:.4f}")
    everything = np.ones(len(opened), dtype=bool)
    errors = {}
    for name, weights in (("shipped", corpus.weights), ("frozen", frozen)):
        numbers = scored(opened, weights, everything, k)
        print(
            f"{name} calibration mse {numbers['mse']:.6f} log {numbers['log']:.6f} "
            f"quantized_mse {numbers['quantized_mse']:.6f} "
            f"quantized_log {numbers['quantized_log']:.6f}"
        )
        errors[name] = squared_errors(numbers["integers"], opened.results, k)
        for bucket in BUCKETS:
            inside = opened.buckets == bucket
            if not inside.any():
                continue
            print(
                f"{name} calibration pieces {bucket} "
                f"{int(opened.counts[inside].sum())} quantized_mse "
                f"{mean_squared_error(numbers['integers'][inside], opened.results[inside], opened.counts[inside], k):.6f}"
            )
    mean, error, naive, design = paired_difference(
        errors["shipped"], errors["frozen"], opened.counts, opened.games
    )
    print(
        f"frozen against shipped calibration quantized_mse {mean:+.6f} "
        f"se {error:.6f} per position {naive:.6f} design {design:.1f} "
        "(clustered on the game)"
        + ("" if abs(mean) > 2 * error else " (inside its interval)")
    )
    integers = scored(opened, frozen, everything, k)["integers"]
    residual = opened.results - sigmoid(integers, k)
    signed = " ".join(
        f"p{int(100 * q)} {weighted_quantile(residual, opened.counts, q):+.4f}"
        for q in (0.5, 0.9, 0.95, 0.99)
    )
    absolute = " ".join(
        f"p{int(100 * q)} {weighted_quantile(np.abs(residual), opened.counts, q):.4f}"
        for q in (0.5, 0.9, 0.95, 0.99)
    )
    print(f"frozen residual signed {signed}")
    print(f"frozen residual absolute {absolute}")
    print(
        "residuals are result less predicted, weighted by appearances, each "
        "quantile the smallest residual with at least that share at or under "
        "it: an empirical diagnostic, not a coverage claim"
    )
    return 0


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    for name in ("loss", "cv", "fit", "final", "curve"):
        command = commands.add_parser(name)
        command.add_argument("--terms", required=True, help="an arche terms run")
        command.add_argument("--corpus", required=True, help="the epd it was run over")
        command.add_argument(
            "--sealed",
            help="the file naming the sealed pairs the corpus was built with; "
            "without it the sealed group is drawn from the keys, and a corpus "
            "built with one and read without it holds out the wrong games",
        )
    for name in ("loss", "fit", "final", "curve"):
        # cv fits K per fold, so there is no one constant to name
        commands.choices[name].add_argument(
            "--k", type=float, help="the scaling constant, if it is known"
        )
    loss = commands.choices["loss"]
    loss.add_argument("--weights", nargs="*", help="candidate vectors, as json arrays")
    for name in ("cv", "fit", "curve"):
        command = commands.choices[name]
        command.add_argument("--iterations", type=int, default=300)
        command.add_argument(
            "--penalties",
            type=float,
            nargs="+",
            default=[0.0, 1e-8, 1e-7, 1e-6, 1e-5, 1e-4],
            help="the ridge grid, ranked on the selection games",
        )
        command.add_argument(
            "--free-material",
            action="store_true",
            help="let the six material values move, which the first fit does not",
        )
        command.add_argument(
            "--hold",
            action="append",
            metavar="TERM",
            default=[],
            help="hold a term's weights where they are, given once for each "
            "term held: tables for the two piece square blocks, or one of the "
            "leaf terms the run's layout line names after material. A fit for "
            "a term added after another holds that one, so a match can say "
            "which of the two it measured",
        )
    fit = commands.choices["fit"]
    fit.add_argument("--out", help="where to write the fitted vector")
    curve = commands.choices["curve"]
    curve.add_argument(
        "--shares",
        type=float,
        nargs="+",
        default=list(CURVE_SHARES),
        help="the sizes to fit at, as shares of the training pairs",
    )
    curve.add_argument(
        "--draws",
        type=int,
        default=CURVE_DRAWS,
        help="independent draws at each share below the whole",
    )
    curve.add_argument(
        "--seed", type=int, default=0, help="what the draws are drawn with"
    )
    curve.add_argument("--out", help="where to write every fit's numbers as json")
    final = commands.choices["final"]
    final.add_argument(
        "--weights",
        required=True,
        help="the frozen vector, as a json array of integers",
    )
    final.add_argument(
        "--log",
        required=True,
        help="the access log, appended to before any sealed row is read",
    )
    args = parser.parse_args(argv)
    return {
        "loss": command_loss,
        "cv": command_cv,
        "fit": command_fit,
        "final": command_final,
        "curve": command_curve,
    }[args.command](args)


if __name__ == "__main__":
    sys.exit(main())
