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

`loss` is the triage instrument. A weight vector is scored over the whole
corpus in one matrix-vector product, which is milliseconds, so a candidate can
be killed before it costs a match. `cv` is how one way of fitting is compared
with another: it refits over five folds of whole games and scores every row
under the fold that held its game out. `fit` is the tuner, and it chooses its
ridge on the selection group. `final` opens the sealed group: it scores a
frozen vector there once, and the log it appends to is what refuses a second
opening of the same games. `curve` asks whether the corpus is big enough, by
refitting on draws of the training pairs at five sizes and reading every fit
on the same selection group.

The unit of all of it is the game and not the position. Positions inside one
game share a label and are a move apart, so a split that separates positions
still leaves a held-out row's answer sitting beside it in the training set, and
an interval taken over positions counts a game's worth of rows as a game's
worth of evidence. Neither is a detail. Splitting on the position rather than
the game left 1,805 of the corpus's 1,809 games with rows on both sides and
made the interval about four times too narrow, and it chose a ridge two orders
of magnitude off the one whole games choose.

There are three groups and not two. Three fifths of the games train, a fifth
chooses the ridge, and a fifth is not read here at all. The last is what makes
a later coverage claim mean anything: a group a fit has been ranked against
has already had the labels influence it, so the claim has to be made on a group
nothing has looked at. That is not a rule to remember. The calibration rows are
not in the matrices the loss and the fit read, so no command that fits or
scores can reach a calibration row. `final` can, by design and once: it takes a vector
already quantized to the integers that would ship, writes the corpus, its
sealed games, the extraction and the vector it was given into a log by
checksum before it reads anything, and refuses a corpus or a set of sealed
games the log already names. A vector revised after that reading needs sealed
games this corpus did not hold, and the same games under a new filename or a
re-extraction are not that.

No command but `final` reads a sealed row, and no sealed game's result reaches
a label a fit sees. The rows are held apart because they are not in the matrices at
all, and the labels because `build_corpus.py` labels a position from its own
group's games alone: a position that games in different groups reached belongs
to the group of the lowest key, its result and its count are that group's
appearances, and the appearances elsewhere are dropped rather than merged in.
What the dropping costs is those appearances and a little weight on the
positions common enough to recur, which is the price of a group that means what
it says.

The objective is occurrence weighted. A unique position carries the weight of
how many times the corpus reached it, which is what the corpus's `count`
operand says, and every loss, interval and share printed here reads it. The
corpus is scored on the distribution the engine will run on rather than on the
distribution deduplication leaves behind.

Nothing here knows how to evaluate a position. The engine states the
coefficients and states the weights, and `reconstruct` below folds one row back
against the other and has to give the integer the row says the engine gave it.
That is checked on every row read, so a corpus this cannot rebuild stops the
run rather than being fitted.

Three details of that arithmetic are the ones a python reader gets wrong. The
divide truncates toward zero where `//` floors, and on a negative numerator
that does not divide evenly the two differ by a centipawn. The material is
added outside the divide rather than scaled into it. And the phase is capped
before the coefficients are written, which the engine has already done by the
time a row is printed. Each has a test in `scripts/tests/test_tune.py`.

What the numbers are not: there is no loss-to-elo mapping here and this does
not print one. The job is to rank candidates and to reject the ones that cannot
help. The sprt says elo.
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

# The weight vector, as `arche-core/src/tune.rs` lays it out: 384 midgame table
# entries, then 384 endgame ones in the same order, then the six material
# values, then four midgame mobility weights and the same four at the endgame
# end, then the seven shelter weights the same way, then the eight pawn
# structure weights the same way again. A square's two weights are
# MIDGAME_SLOTS apart, a piece kind's two mobility weights MOBILITY_SLOTS
# apart, a shelter count's two SHELTER_SLOTS apart and a pawn count's two
# PAWN_SLOTS apart.
MIDGAME_SLOTS = 6 * 64
ENDGAME_SLOTS = 6 * 64
MATERIAL_SLOT = MIDGAME_SLOTS + ENDGAME_SLOTS
MOBILITY_SLOT = MATERIAL_SLOT + 6
MOBILITY_SLOTS = 4
SHELTER_SLOT = MOBILITY_SLOT + 2 * MOBILITY_SLOTS
SHELTER_SLOTS = 7
PAWN_SLOT = SHELTER_SLOT + 2 * SHELTER_SLOTS
PAWN_SLOTS = 8
SLOTS = PAWN_SLOT + 2 * PAWN_SLOTS

# The layout before a knight, a bishop, a rook and a queen were given an
# endgame table of their own: the same 384 midgame entries, the pawn's endgame
# table and the king's, and the six material values. It is named so that a row
# or a fitted vector written against it is turned away by what it is rather
# than by its length alone.
SHARED_TABLE_SLOTS = 6 * 64 + 2 * 64 + 6

# The layout before mobility: both halves of the tables and the material, and
# nothing after them. Named for the same reason SHARED_TABLE_SLOTS is, since
# every slot it holds still exists here and holds the same weight.
NO_MOBILITY_SLOTS = MOBILITY_SLOT

# The layout after mobility and before the king's shelter, and the layout
# before the shelter grew the pawn storm. Both are nearer mistakes than the two
# above: they are one term or half a term back rather than two, and every slot
# either holds still means here what it meant there.
NO_SHELTER_SLOTS = SHELTER_SLOT
NO_STORM_SLOTS = SHELTER_SLOT + 2 * 4

# The layout after the shelter and before the pawn structure, which is the
# most recent mistake of the five and so the likeliest: a row extracted or a
# vector fitted one term back, when every slot it holds still means here what
# it meant there.
NO_PAWN_SLOTS = PAWN_SLOT

# The most one knight, one bishop, one rook and one queen can each cover, which
# is what a mobility weight is priced against in bounds_hold. A queen in the
# middle of an empty board is the twenty seven.
MAX_COUNT = np.array([8, 13, 14, 27])

# The most of each shelter count one side can show, which is what a shelter
# weight is priced against in bounds_hold. Three pawns on each of the five
# ranks counted, its own two and the storm's three, and three files, which the
# open and half open counts share rather than reach each.
MAX_SHELTER = 3

# The most of each pawn count one side can show, which is what a pawn
# structure weight is priced against in bounds_hold. Eight, since that is how
# many pawns a side has. Charging all eight counts at eight is far past any
# position, since eight pawns cannot fill sixty four counts between them, and
# the looseness is on the safe side.
MAX_PAWNS = 8

# What the opening's pieces come to on the scale the taper is read at, which is
# what the piece square half of a row divides by.
TOTAL_PHASE = 24

# The pieces the phase buckets count: neither pawns nor kings, both colours.
PIECES_COUNTED = "nbrqNBRQ"

# The buckets the game-corpus report stratified by, and the harness after it.
BUCKETS = ("0-6", "7-12", "13+")

# How many folds a cross validated comparison uses. Five, so each fit reads
# four fifths of the games and every row is scored once.
FOLDS = 5

# The fields of a fen: the board, the side to move, the castling rights, the
# en passant square and the two clocks. What a row's reader counts back from.
FEN_FIELDS = 6

# What the run's header line opens with, in the two shapes `Report`'s
# `Display` writes it: the bench's own suite reads as absent and any other
# file is named. Matched in full rather than on the word alone, because an id
# can open with that word too and a row skipped for looking like a header
# would leave the corpus a position short with nothing said about it.
HEADERS = ("terms positions ", "terms epd ")


def check_layout(count, what):
    """Refuse a vector of any length but this file's, and say what changed when
    it is a length the layout had before.

    The layout is a contract between this file and `arche-core/src/tune.rs`,
    and the two are edited together. A vector of an earlier length is the wrong
    length a bare count would not explain: it parses, every slot it names
    exists here, and its numbers land on weights they were not fitted for.
    """
    if count == SLOTS:
        return
    if count == SHARED_TABLE_SLOTS:
        raise ValueError(
            f"{what} of {count}, which is the layout from before a knight, a "
            f"bishop, a rook and a queen were given an endgame table. The "
            f"vector is {SLOTS} now, so extract the rows again and refit"
        )
    if count == NO_MOBILITY_SLOTS:
        raise ValueError(
            f"{what} of {count}, which is the layout from before mobility. The "
            f"vector is {SLOTS} now, so extract the rows again and refit"
        )
    if count == NO_SHELTER_SLOTS:
        raise ValueError(
            f"{what} of {count}, which is the layout from before the king's "
            f"shelter was measured. The vector is {SLOTS} now, so extract the "
            f"rows again and refit"
        )
    if count == NO_STORM_SLOTS:
        raise ValueError(
            f"{what} of {count}, which is the layout from before the pawn "
            f"storm joined the king's shelter. The vector is {SLOTS} now, so "
            f"extract the rows again and refit"
        )
    if count == NO_PAWN_SLOTS:
        raise ValueError(
            f"{what} of {count}, which is the layout from before the pawn "
            f"structure was measured. The vector is {SLOTS} now, so extract "
            f"the rows again and refit"
        )
    raise ValueError(f"{what} of {count}, expected {SLOTS}")


def trunc_div(numerator, denominator):
    """Rust's integer `/`, which truncates toward zero where python's `//`
    floors. On a negative numerator that does not divide evenly the two differ
    by one, which is a centipawn of evaluation."""
    quotient = abs(numerator) // denominator
    return quotient if numerator >= 0 else -quotient


def is_material(slot):
    """Whether a slot is one of the six material values, which are the only
    weights added outside the taper's divide. Everything else is inside it,
    the tables and both leaf terms alike, which is what `Accumulator::score`
    does with them."""
    return MATERIAL_SLOT <= slot < MOBILITY_SLOT


def reconstruct(coefficients, weights):
    """The evaluation a row states, folded back against the weights.

    The material is added outside the divide and not scaled into it. Folding it
    in gives a different integer: `trunc((24 * 1 + -5) / 24)` is 0 where
    `1 + trunc(-5 / 24)` is 1.
    """
    material = 0
    numerator = 0
    for slot, coefficient in coefficients:
        product = coefficient * weights[slot]
        if is_material(slot):
            material += product
        else:
            numerator += product
    return material + trunc_div(numerator, TOTAL_PHASE)


def fold_of(key, folds=FOLDS):
    """Which fold a game falls in, by the second byte of its key. Whole games,
    for the same reason the groups are whole games.

    The second byte and not the first, because the first is what `groups.py`
    put the game in its group by. Folding on it would make one fold the
    selection group and leave another empty.
    """
    if len(key) < 4:
        raise ValueError(f"a game key that is no sha256: {key!r}")
    return int(key[2:4], 16) % folds


def phase_bucket(fen):
    """How many pieces that are neither pawns nor kings the position holds,
    as one of three buckets. Counted off the fen's board field, which is a
    label for reading the loss by and not an opinion about the position."""
    board = fen.split(" ", 1)[0]
    pieces = sum(board.count(piece) for piece in PIECES_COUNTED)
    if pieces <= 6:
        return BUCKETS[0]
    if pieces <= 12:
        return BUCKETS[1]
    return BUCKETS[2]


def split_row(words):
    """The fields of one `arche terms` row, read from its right hand end.

    A row is `id eval phase n slot:coefficient... fen`, and both ends of it
    can hold spaces. A fen is six fields, and an epd id is whatever the file
    put in the quotes: the bench's own suite names positions "ruy lopez" and
    "king and pawn", and a line that names no id is called by its own fen, so
    an id can be six fields itself. So the fields are found from the end whose
    width is fixed. The fen is the last six, the coefficients are the run of
    `slot:coefficient` in front of them, and what is left before the three
    numbers is the id. The engine squeezes a name's whitespace to single
    spaces before it ever prints one, so those fields joined back up are the
    name the epd held.

    The run of coefficients cannot walk back into the id whatever the id
    holds, because the three numbers between them carry no colon. `n` is held
    against the run rather than counted forward from, so the two ends of the
    row have to agree.
    """
    if len(words) < FEN_FIELDS + 4:
        raise ValueError(f"a row of {len(words)} fields: {' '.join(words)!r}")
    head, fen = words[:-FEN_FIELDS], " ".join(words[-FEN_FIELDS:])
    start = len(head)
    while start > 0 and ":" in head[start - 1]:
        start -= 1
    if start < 4:
        raise ValueError(f"a row whose fields do not line up: {' '.join(words)!r}")
    identifier = " ".join(head[: start - 3])
    evaluation, phase, count = (int(word) for word in head[start - 3 : start])
    coefficients = []
    for word in head[start:]:
        slot, coefficient = word.split(":")
        coefficients.append((int(slot), int(coefficient)))
    if count != len(coefficients):
        raise ValueError(
            f"{identifier} says {count} coefficients and prints {len(coefficients)}"
        )
    return identifier, evaluation, phase, coefficients, fen


class Row:
    """One position: what the engine said it scored, and what of.

    Which game it belongs to is not here. The extraction knows the position and
    the corpus knows the game, and reading a game out of a row's name would be
    reading it from the file that does not hold it.
    """

    def __init__(self, identifier, evaluation, phase, coefficients, fen):
        self.id = identifier
        self.eval = evaluation
        self.phase = phase
        self.coefficients = coefficients
        self.fen = fen


def parse_terms(lines):
    """The weights and the rows of an `arche terms` run.

    Every row is rebuilt from the weights and held against the evaluation it
    states. A row that does not rebuild means this file's arithmetic and the
    engine's have parted company, which is the one failure the seam exists to
    catch, so it raises rather than dropping the row.
    """
    weights = None
    rows = []
    for line in lines:
        line = line.strip()
        if not line or line.startswith(HEADERS):
            continue
        words = line.split()
        if words[0] == "weights":
            count = int(words[1])
            weights = [int(word) for word in words[2 : 2 + count]]
            if len(weights) != count:
                raise ValueError(
                    f"a weights line saying {count} with {len(weights)} on it"
                )
            check_layout(count, "a weights line")
            continue
        if weights is None:
            raise ValueError("a row arrived before the weights line")
        identifier, evaluation, phase, coefficients, fen = split_row(words)
        rebuilt = reconstruct(coefficients, weights)
        if rebuilt != evaluation:
            raise ValueError(
                f"{identifier} rebuilds to {rebuilt} and the engine says {evaluation}"
            )
        rows.append(Row(identifier, evaluation, phase, coefficients, fen))
    if weights is None:
        raise ValueError("no weights line")
    return weights, rows


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
    """The label of each position, by id.

    Read the way the engine reads epd, which is the point of reading it here
    at all: the first four words are the position and what follows them is
    operations, each an opcode and its operands, ended by a semicolon.

    A row with no `game` or no `pair` operand is refused rather than given one
    of its own. The pair is what the three groups are assigned from and the
    game is what every interval is taken over, and a corpus built before either
    existed would be split into one game per position, which is the leak the
    game split was written to close arriving quietly through the back door.
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

    Its rows are held apart rather than masked out. A mask is a convention: it
    works while every caller remembers it, and the one that forgets is the one
    that spends the group. These rows are not in the corpus's matrices at all,
    so `loss`, `cv` and `fit` are handed a corpus that does not contain them
    and cannot reach them by accident. Deleting the calibration rows from the
    corpus file changes nothing any of the three prints, and a test says so.

    What a run may say about it is how big it is, which is what the header
    prints and what says the group exists. `unseal` is what the arm that holds
    final weights calls, through `final`, once per set of sealed games, with
    the log to say so.

    The labels are held apart as well, and upstream of here. A position that a
    training game and a calibration game both reached is labelled by whichever
    of the two groups owns it and by that group's appearances alone, so no
    result crosses the seal in either direction. What that costs is the dropped
    appearances, which `build_corpus.py` counts.
    """

    def __init__(self, rows, labels, weights):
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
        """The sha256 of the sealed pair keys, sorted: what names the sealed
        games apart from the file they came in, so a re-extraction that adds
        a run is still the same sealed games."""
        keys = sorted({self._labels[row.id].pair for row in self._rows})
        return hashlib.sha256("\n".join(keys).encode("utf-8")).hexdigest()

    @property
    def appearances(self):
        return sum(self._labels[row.id].count for row in self._rows)

    def unseal(self):
        """The rows, loaded the way the corpus loads its own so they can be
        scored, for the arm whose weights are final."""
        opened = Corpus.__new__(Corpus)
        opened.sealed = None
        opened._load(
            self._weights,
            list(self._rows),
            self._labels,
            dict.fromkeys((row.id for row in self._rows), CALIBRATION),
        )
        return opened


class Corpus:
    """The joined rows, in the shape the loss and the fit read them.

    The coefficients are held as three flat arrays, which is a sparse matrix
    without a library: for each non-zero, which row it belongs to, which slot,
    and what it is. Scoring is then one multiply and one `bincount`, and the
    gradient is the same two the other way round, which is what makes scoring a
    weight vector over the whole corpus a matter of milliseconds.

    The piece square coefficients and the material ones are kept apart, because
    the two enter the evaluation differently: the piece square half divides by
    the taper and the material half does not.

    The rows also carry which game each came from and which pair that game is
    half of, because the pair is the unit the split is taken over and the game
    the unit every interval is, and it is the corpus that says so rather than
    the extraction.

    The calibration group is not here. It is in `sealed`, which holds its rows
    apart and scores them for `final` alone.
    """

    def __init__(self, weights, rows, labels, sealed=None):
        joined = [row for row in rows if row.id in labels]
        # a position appears in exactly one row, because build_corpus.py
        # deduplicates by fen before it labels and gives the row the lowest key
        # of the games that reached it, so no position is in two groups. a
        # corpus that was not deduplicated could be, and would be the leak the
        # fen split had in a new place, so it is refused rather than fitted
        # around. checked over every row and not only the ones that are kept,
        # because a fen in both a training game and the sealed group is the
        # same fault
        first = {}
        for row in joined:
            owner = first.setdefault(row.fen, labels[row.id].game)
            if owner != labels[row.id].game:
                raise ValueError(
                    f"the position {row.fen} is in {owner} and in {labels[row.id].game}"
                )
        groups = {row.id: group_of(labels[row.id].pair, sealed) for row in joined}
        if sealed is not None:
            # a named pair that reached no row of this extraction is a game
            # the seal was drawn from and the corpus does not hold, so the
            # group is smaller than the file says it is. Silence there would
            # be the one failure the seal cannot afford: a reading reported
            # over games nobody can name
            missing = sealed - {labels[row.id].pair for row in joined}
            if missing:
                raise ValueError(
                    f"{len(missing)} sealed pairs reach no row of this "
                    f"extraction, the first being {min(missing)}"
                )
        self.sealed = Sealed(
            [row for row in joined if groups[row.id] == CALIBRATION], labels, weights
        )
        kept = [row for row in joined if groups[row.id] != CALIBRATION]
        self._load(weights, kept, labels, groups)

    def _load(self, weights, kept, labels, groups):
        """The arrays over one set of rows: the corpus's own, or the sealed
        group's the once it is opened."""
        self.rows = kept
        self.weights = np.array(weights, dtype=np.float64)
        self.evals = np.array([row.eval for row in kept], dtype=np.float64)
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
        # the two sides of the taper's divide. Material is added outside it
        # and every other weight, mobility included, is inside
        tapered, material = [], []
        for index, row in enumerate(kept):
            for slot, coefficient in row.coefficients:
                (material if is_material(slot) else tapered).append(
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

        The truncating divide is dropped here. It is at most a centipawn and
        the fit is not sensitive to it, and `integer_scores` is what puts it
        back for the measurement that has to match the engine.
        """
        rows, slots, values = self.tapered
        numerator = np.bincount(rows, values * weights[slots], minlength=len(self))
        rows, slots, values = self.material
        material = np.bincount(rows, values * weights[slots], minlength=len(self))
        return material + numerator / TOTAL_PHASE

    def integer_scores(self, weights):
        """The same, at integer weights and with the truncation put back, which
        is the evaluation the engine would give.

        The divide is done on integers and toward zero, not by rounding a
        float, so this is `trunc_div` a row at a time and not something near
        it.
        """
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
        return material + np.sign(numerator) * (np.abs(numerator) // TOTAL_PHASE)

    def scatter(self, per_row):
        """A per-row quantity spread back over the slots, which is the gradient
        of anything that reads the corpus through `scores`."""
        rows, slots, values = self.tapered
        gradient = np.bincount(
            slots, values * per_row[rows] / TOTAL_PHASE, minlength=SLOTS
        )
        rows, slots, values = self.material
        return gradient + np.bincount(slots, values * per_row[rows], minlength=SLOTS)

    def support(self, mask=None):
        """How many of the rows each slot appears in. A weight the corpus
        barely constrains says so here rather than after it has shipped."""
        counts = np.zeros(SLOTS, dtype=np.int64)
        for rows, slots, _ in (self.tapered, self.material):
            picked = slots if mask is None else slots[mask[rows]]
            counts += np.bincount(picked, minlength=SLOTS).astype(np.int64)
        return counts


def sigmoid(scores, k):
    """Texel's, so that the number is comparable with published practice: the
    logistic that turns a centipawn score into an expected result."""
    return 1.0 / (1.0 + np.power(10.0, -k * scores / 400.0))


def mean_squared_error(scores, results, counts, k):
    predicted = sigmoid(scores, k)
    return float(np.sum(counts * (results - predicted) ** 2) / np.sum(counts))


def log_loss(scores, results, counts, k):
    """The other scoring rule, printed beside the first. If the two disagree
    about a candidate that is worth seeing, which is the only reason both are
    here."""
    predicted = np.clip(sigmoid(scores, k), 1e-12, 1 - 1e-12)
    terms = results * np.log(predicted) + (1 - results) * np.log(1 - predicted)
    return float(-np.sum(counts * terms) / np.sum(counts))


def fit_k(scores, results, counts, low=0.1, high=4.0, steps=60):
    """The scaling constant, by a golden section search over the training
    split at the shipped weights.

    Fitted once and held for the rest of a run. K and the overall scale of the
    weights are one degree of freedom, and the scale is not free: the reverse
    futility margin, the delta margin and the ledger's eval column all read the
    evaluation on the assumption that a pawn is about a hundred, so a fit at
    liberty to rescale would retune all three without touching them.
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
    """The per-position squared error, which is what a paired difference
    between two weight vectors is taken over."""
    return (results - sigmoid(scores, k)) ** 2


def weighted_quantile(values, weights, quantile):
    """The smallest value with at least the given share of the weight at or
    under it, the weight being the appearances, so a position the corpus
    reached often counts for what it is. No interpolation: the value returned
    is one the corpus holds."""
    order = np.argsort(values)
    cumulative = np.cumsum(weights[order])
    index = int(np.searchsorted(cumulative, quantile * cumulative[-1]))
    return float(values[order][min(index, len(values) - 1)])


def paired_difference(first, second, counts, games):
    """The mean difference between two weight vectors' per-position errors, its
    standard error taken over the games, the one a reader would get over the
    positions, and the ratio of the two.

    The two are scored on the same positions, so the difference is a paired
    sample and its mean has an interval. A loss difference whose interval
    covers zero is not a difference, which is why this never returns a bare
    delta.

    The interval is taken over games and not over positions. A game's hundred
    odd positions share a result and differ by a move, so they move together,
    and treating them as a hundred independent draws counts one game's evidence
    a hundred times. The sum of each game's contributions to the mean is what
    varies from game to game, so that is what the spread is taken of, which is
    the usual cluster-robust interval with the game as the cluster.

    Both are returned, because the naive one is worth printing rather than
    describing. Their ratio is the design factor: it says what treating the
    positions as independent would have claimed, and if it ever comes back near
    one then the games were carrying no more dependence than the positions and
    holding them out cost more than it bought.
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

    Both ways round, which is not the textbook default and is what this
    problem needs. The loss is a mean over a hundred thousand positions and
    the gradient with respect to one weight is of the order of a hundred
    thousandth, so the first useful step is many times longer than one, and a
    search that only ever backtracks would spend its whole budget getting
    there. It steps out while the loss keeps falling and halves back when it
    does not.
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
    """L-BFGS on the closed-form gradient.

    The classic Texel local search walks one weight at a time over the whole
    corpus per step, which for a vector this long is a great many passes. The
    evaluation is linear in its weights, so the gradient is closed form and
    none of that is needed.
    """
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
    picks, with a ridge toward the weights the arm started from.

    Toward the shipped weights and not toward zero. It makes a re-tune
    literally what the fit does; it leaves alone the one direction the corpus
    cannot see, which is a constant added to both king tables and cancelling
    between the colours; and it holds the slots with no support at all, the two
    back ranks of the pawn tables, at exactly the zeroes they already are.
    """
    results = corpus.results[mask]
    counts = corpus.counts[mask]
    total = np.sum(counts)
    scale = k * math.log(10.0) / 400.0
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

    def scores_of(weights):
        numerator = np.bincount(
            tapered_part[0], tapered_part[2] * weights[tapered_part[1]], minlength=size
        )
        material = np.bincount(
            material_part[0],
            material_part[2] * weights[material_part[1]],
            minlength=size,
        )
        return material + numerator / TOTAL_PHASE

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
            minlength=SLOTS,
        ) + np.bincount(
            material_part[1],
            material_part[2] * per_row[material_part[0]],
            minlength=SLOTS,
        )
        gradient += 2.0 * penalty * np.where(frozen, 0.0, slack)
        gradient[frozen] = 0.0
        return value, gradient

    return objective


def quantize(weights):
    """Round to nearest. The table entries are `i16` in the engine and the
    material values `u32`, and what a rounded vector costs is measured rather
    than assumed."""
    return np.rint(np.asarray(weights)).astype(np.int64)


def bounds_hold(weights):
    """Whether a quantized vector stays inside what the engine's arithmetic
    can carry: each half of a packed pair is an `i16`, a boardful of them is
    summed into one, and an evaluation over the mate threshold would be read as
    a forced mate.

    A one-sided boardful, both colours, against the sixteen bits the halves
    have to stay inside.

    All three leaf terms are in the same sum, so they are priced here too
    rather than left out of a figure that reads as the whole vector. A piece of
    each kind at its widest is the mobility price, three of each of its seven
    counts is the shelter's, and eight of each of its eight is the pawn
    structure's. All are screens rather than proofs: a side that promoted could
    cover more, the worst a board can be arranged into comes to 313 squares
    against the 62 mobility charges, a side's open and half open files come to
    three between them rather than three each, and eight pawns cannot fill the
    pawn term's sixty four charges between them. Over
    the 1,809 positions of the three suites the largest one-sided mobility
    difference was 37, so at the centipawn weights a fit produces none of the
    figures is near the sixteen bits. What this catches is a vector that has
    gone somewhere else entirely.
    """
    weights = np.abs(np.asarray(weights))
    tables = weights[:MATERIAL_SLOT]
    midgame = tables[:MIDGAME_SLOTS].reshape(6, 64)
    endgame = tables[MIDGAME_SLOTS:MATERIAL_SLOT].reshape(6, 64)
    worst = 2 * max(int(midgame.max(axis=0).sum()), int(endgame.max(axis=0).sum()))
    mobility = weights[MOBILITY_SLOT:SHELTER_SLOT].reshape(2, MOBILITY_SLOTS)
    worst += 2 * int((mobility * MAX_COUNT).sum(axis=1).max())
    shelter = weights[SHELTER_SLOT:PAWN_SLOT].reshape(2, SHELTER_SLOTS)
    worst += 2 * MAX_SHELTER * int(shelter.sum(axis=1).max())
    pawns = weights[PAWN_SLOT:].reshape(2, PAWN_SLOTS)
    worst += 2 * MAX_PAWNS * int(pawns.sum(axis=1).max())
    return worst < 32767, worst


def read_weights(path):
    weights = json.loads(Path(path).read_text(encoding="utf-8"))
    check_layout(len(weights), f"{path} holds a vector")
    return np.array(weights, dtype=np.float64)


def table_scale(weights, shipped):
    """How much larger the table entries have grown, as the ratio of their
    root mean squares.

    The figure a reader of a fit has to see. Material is held, so it anchors
    the pawn at a hundred, but the tables are free to grow against it, and a
    piece square half twice the size it was is a different evaluation for the
    reverse futility margin and the delta margin to be read against. K and the
    overall scale are one degree of freedom, and a fit given enough licence
    will spend the loss on the scale rather than on the shape.
    """
    fitted = np.asarray(weights)[:MATERIAL_SLOT]
    before = np.asarray(shipped)[:MATERIAL_SLOT]
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
    """The loss table: each weight vector on each group it may be scored on,
    pooled and stratified, with a paired difference against the first.

    Two groups are scored and the third is named and left alone. Naming it is
    what says it exists and how big it is, which is the whole of what a run may
    say about a group whose value is that nothing has read it.
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
    wins = float(np.sum(corpus.counts * corpus.results) / np.sum(corpus.counts))
    draws = float(
        np.sum(corpus.counts * (corpus.results == 0.5)) / np.sum(corpus.counts)
    )
    print(f"results mean {wins:.4f} drawn {100 * draws:.1f}%", file=out)
    appearances = max(float(corpus.counts.sum()), 1.0)
    for bucket in BUCKETS:
        share = corpus.buckets == bucket
        # the share is of the appearances and not of the unique positions,
        # because the loss weights a position by how often the corpus reached
        # it and a share read the other way describes a corpus nothing scores
        seen = float(corpus.counts[share].sum())
        print(
            f"pieces {bucket} positions {int(share.sum())} "
            f"appearances {int(seen)} ({100.0 * seen / appearances:.1f}%)",
            file=out,
        )
    print(f"k {k:.4f}", file=out)
    baseline = None
    for name, weights in named:
        # the same loss with K refitted for this vector alone, which is the
        # diagnostic that says whether a fit bought shape or only scale. K is
        # held for every number beside it, because K and the overall scale of
        # the weights are one degree of freedom and the reported figure has to
        # mean the same thing for every vector
        own = fit_k(
            corpus.scores(np.asarray(weights, dtype=np.float64))[train],
            corpus.results[train],
            corpus.counts[train],
        )
        print(
            f"{name} scale {table_scale(weights, corpus.weights):.3f} own_k {own:.4f} "
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
    """Cross validation with whole games held out, which is how one way of
    fitting is compared with another.

    Five folds of the games the run may read, which is the training and
    selection groups and not the sealed one: the corpus this is handed does not
    hold the calibration rows, so no fold can contain one. Each fold refits on
    four fifths of those games and is scored on the fifth, so every row is
    scored by a fit that never read its game, and every row is scored once
    rather than half of them being scored at all. K is fitted per fold on that
    fold's training games at the shipped weights and held for the vectors
    scored in it, which is the rule a single split already follows.

    This is not what the fit chooses its ridge on. The selection group is, and
    it is a fifth of the games set aside for it. This answers the wider
    question, which is whether a way of fitting is worth anything at all over
    the corpus the run may read, and it answers it with every row scored rather
    than a fifth of them.

    Returns the per-row squared error each recipe earned on the fold that held
    its game out, keyed by penalty, with the shipped weights under `shipped`,
    and the penalties whose fits outgrew what the engine's arithmetic carries.
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
        # a fold is a fit for every penalty on the grid, so the line says what
        # is starting rather than what has finished
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
            if not bounds_hold(quantize(fitted))[0]:
                outside.add(penalty)
            errors[penalty][held] = squared_errors(
                corpus.scores(fitted)[held], results, k
            )
    return errors, outside


def cross_validated(corpus, errors, outside, out=None):
    """The cross validated loss of each recipe, and what it bought over the
    shipped weights, with the interval taken over the games.

    Returns the penalty that scored best among those the engine's arithmetic
    can carry, or none if that is all of them.
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
        # a vector the engine's arithmetic cannot carry is no candidate,
        # whatever it scores
        if not refused and (best is None or loss < best[0]):
            best = (loss, penalty)
    return None if best is None else best[1]


def choose_penalty(
    corpus, penalties, start, frozen, iterations, k, out=None, train=None
):
    """The ridge, chosen on the selection group.

    Every penalty on the grid is fitted on the training games alone and scored
    on the selection games, which no fit read. The lowest selection loss wins,
    and a fit whose tables outgrew what the packed halves carry is no candidate
    whatever it scores. `train` is the rows to fit on in place of the training
    group, which is what the learning curve moves; the rows scored are the
    selection group's whichever rows were fitted.

    The selection group and never the calibration group. Ranking a grid is
    model selection, and a group the labels have already influenced cannot
    carry a distribution-free coverage claim afterwards. That is not enforced
    here by remembering it: the calibration rows are not in this corpus at all.

    What it costs is that the loss reported on the selection group afterwards
    is the fit's own best case, since it is the number the grid was ranked on.
    The sealed group is what an honest interval on a final vector comes from.

    Returns the chosen penalty and the vector it fitted, or none and none if
    the grid left nothing the engine can carry.
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
        refused = not bounds_hold(quantize(fitted))[0]
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
# how many independent draws each size below the whole gets. One draw is one
# sample of a random variable, and the spread between draws is what says
# whether the curve's shape is real.
CURVE_SHARES = (0.125, 0.25, 0.5, 0.75, 1.0)
CURVE_DRAWS = 5


def curve_shares(shares, draws):
    """The shares a curve fits at, sorted and deduplicated, each in (0, 1].
    Refused rather than clamped: a share past the whole would fit the whole
    again under another name, and one at or under nothing would draw nothing."""
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

    The training pool is drawn by pair and never by position. The pair is the
    independent unit: the two games of an opening share their first moves,
    and a game's positions share a result. Each draw is taken afresh from the
    whole pool, so a smaller draw is not a prefix of a larger one. The
    selection group is fixed and every fit is read on it, which is what makes
    the points comparable. The whole is fitted once, since there is nothing
    to draw.

    Nothing else moves. The ridge is ranked on the selection group as `fit`
    ranks it, the weighting is by appearances, and K is one number for every
    fit: the one given, or the one fitted on the whole training group at the
    shipped weights. A K refitted per draw would let a smaller draw change the
    scale as well as the tables.

    Each fit is recorded with the pairs it drew, its chosen penalty, its
    selection loss at real weights and at the integers that would ship, and
    the paired difference against the shipped weights with the interval
    clustered on the game. The summary per share carries the mean loss over
    the draws, the least and the most, and the mean interval. The spread
    between draws says whether the shape is real; the interval within a draw
    says whether that draw beat the shipped weights. They are different
    questions. What the curve does not say is anything about elo.
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


def load(args):
    weights, rows = parse_terms(
        Path(args.terms).read_text(encoding="utf-8").splitlines()
    )
    labels = parse_corpus(Path(args.corpus).read_text(encoding="utf-8").splitlines())
    sealed = sealed_pairs(args.sealed) if getattr(args, "sealed", None) else None
    corpus = Corpus(weights, rows, labels, sealed)
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
        named.append((Path(path).stem, read_weights(path)))
    report(corpus, named, k)
    return 0


def frozen_slots(
    free_material, held_tables=False, held_mobility=False, held_shelter=False
):
    """Which weights a fit holds where they are.

    Material is held for a first fit. `eval::material` is read by the delta
    margin in quiescence, so moving a material value changes which captures
    quiescence skips, which changes the tree for a reason that has nothing to
    do with the evaluation's accuracy. The material block alone: nothing else
    after it is read by the search, and a freeze that ran to the end of the
    vector would hold every leaf term's weights at zero through a fit and print
    a null result with nothing saying why.

    The tables are held when the fit is for a term added after them. They were
    fitted on these same games, so refitting them beside a new term leaves a
    match unable to say which of the two it measured. Held, the new weights are
    the only thing that moved and the only thing the match can be reading.

    Mobility is a hold of its own for that same reason, since it was fitted
    after the tables and before the shelter, and the shelter is one for the
    same reason again. Each term earns a hold as it is fitted, and a fit of
    the newest term names every hold below it, so a shelter fit passes
    `--hold-tables --hold-mobility` and a pawn structure fit passes
    `--hold-tables --hold-mobility --hold-shelter`. Holding the tables alone
    leaves the eight mobility weights free, which is a refit of mobility
    beside the newer term and the attribution the holds exist to keep. That is
    not hypothetical: `1b0862a` found half of the king safety fit's apparent
    gain to be a mobility refit that no hold had stopped.
    """
    frozen = np.zeros(SLOTS, dtype=bool)
    if not free_material:
        frozen[MATERIAL_SLOT:MOBILITY_SLOT] = True
    if held_tables:
        frozen[:MATERIAL_SLOT] = True
    if held_mobility:
        frozen[MOBILITY_SLOT:SHELTER_SLOT] = True
    if held_shelter:
        frozen[SHELTER_SLOT:PAWN_SLOT] = True
    return frozen


def command_cv(args):
    corpus = load(args)
    start = corpus.weights.copy()
    frozen = frozen_slots(
        args.free_material, args.hold_tables, args.hold_mobility, args.hold_shelter
    )
    errors, outside = cross_validate(
        corpus, args.penalties, start, frozen, args.iterations
    )
    chosen = cross_validated(corpus, errors, outside)
    # named so a reader cannot paste it into `fit --penalties` and think it is
    # the ridge the fit would have picked: this is best over the folds, and the
    # fit ranks the same grid on the selection games instead
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
    frozen = frozen_slots(
        args.free_material, args.hold_tables, args.hold_mobility, args.hold_shelter
    )
    k = args.k or fit_k(
        corpus.scores(corpus.weights)[corpus.train],
        corpus.results[corpus.train],
        corpus.counts[corpus.train],
    )
    # the vector that ships is fitted on the training games alone and the ridge
    # above it is ranked on the selection games, so neither has read the third
    # group. that is what the third group is for
    penalty, fitted = choose_penalty(
        corpus, args.penalties, start, frozen, args.iterations, k
    )
    if penalty is None:
        raise SystemExit("tune.py: every penalty on the grid left the tables too large")
    print(f"chose penalty {penalty:g}")
    rounded = quantize(fitted)
    _, worst = bounds_hold(rounded)
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
    """The log line that says these sealed games were opened, if one is
    there. A line is `opened <when> corpus <sha256> sealed <sha256> ...`, and
    both checksums are read: the corpus's, so a renamed file is the same
    corpus, and the sealed games', so a re-extraction with a run appended is
    the same sealed group."""
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
    frozen = frozen_slots(
        args.free_material, args.hold_tables, args.hold_mobility, args.hold_shelter
    )
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

    The order matters. The vector is checked to be the integers that would
    ship, the log is checked for this corpus and these sealed games and the
    line is written, and only then is a sealed row loaded, so a run that
    opened the group and then failed has still said so. What it prints is the
    frozen vector against the shipped one on the sealed rows, with the
    interval clustered on the game, and the residual quantiles the tail claim
    is made from. A second reading of the same sealed games is refused,
    whatever file they arrive in.
    """
    corpus = load(args)
    frozen = read_weights(args.weights)
    if np.any(frozen != np.rint(frozen)):
        raise SystemExit(
            "tune.py: the frozen vector is not the integers that would ship; "
            "quantize it first"
        )
    frozen = frozen.astype(np.int64)
    holds, worst = bounds_hold(frozen)
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
    # the line goes in before a sealed row is loaded, and the sizes it
    # carries are the ones the header may print without opening the group
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
        # the integer scores, which are the evaluation the engine would give
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
        # cv fits K per fold on that fold's training games, so there is no one
        # constant for a caller to name
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
            "--hold-tables",
            action="store_true",
            help="hold the 768 piece square entries, so a fit moves the term "
            "added after them and nothing else",
        )
        command.add_argument(
            "--hold-mobility",
            action="store_true",
            help="hold the eight mobility weights, which a fit for a term "
            "added after them passes alongside --hold-tables",
        )
        command.add_argument(
            "--hold-shelter",
            action="store_true",
            help="hold the fourteen king shelter weights, which a fit for a "
            "term added after them passes alongside the two holds above",
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
