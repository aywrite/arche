#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2022-2026 Andrew Wright

"""Score a weight vector against the games, and fit a new one.

`arche terms` prints, for each quiet position of a corpus, the coefficient
every evaluation weight is multiplied by. `scripts/build_corpus.py` prints what
the games those positions came from ended in. This joins the two and does the
two things that pair is for:

    python3 scripts/tune.py loss --terms rows.txt --corpus corpus.epd
    python3 scripts/tune.py cv   --terms rows.txt --corpus corpus.epd
    python3 scripts/tune.py fit  --terms rows.txt --corpus corpus.epd --out fit.json

`loss` is the triage instrument. A weight vector is scored over the whole
corpus in one matrix-vector product, which is milliseconds, so a candidate can
be killed before it costs a match. `cv` is how one way of fitting is compared
with another: it refits over five folds of whole games and scores every row
under the fold that held its game out. `fit` is the tuner, and it chooses its
ridge on the selection group.

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
not in the matrices the loss and the fit read, so the three commands cannot
reach a calibration row.

No command here reads a sealed row, and no sealed game's result reaches a label
a fit sees. The rows are held apart because they are not in the matrices at
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
import json
import math
import sys
from pathlib import Path

import numpy as np
from groups import CALIBRATION, group_of

# The weight vector, as `arche-core/src/tune.rs` lays it out: 384 midgame table
# entries, then 384 endgame ones in the same order, then the six material
# values. A square's two weights are MIDGAME_SLOTS apart.
MIDGAME_SLOTS = 6 * 64
ENDGAME_SLOTS = 6 * 64
MATERIAL_SLOT = MIDGAME_SLOTS + ENDGAME_SLOTS
SLOTS = MATERIAL_SLOT + 6

# The layout before a knight, a bishop, a rook and a queen were given an
# endgame table of their own: the same 384 midgame entries, the pawn's endgame
# table and the king's, and the six material values. It is named so that a row
# or a fitted vector written against it is turned away by what it is rather
# than by its length alone.
SHARED_TABLE_SLOTS = 6 * 64 + 2 * 64 + 6

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
    it is the length the layout had before.

    The layout is a contract between this file and `arche-core/src/tune.rs`,
    and the two are edited together. A vector of the old length is the one
    wrong length a bare count would not explain: it parses, every slot it names
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
    raise ValueError(f"{what} of {count}, expected {SLOTS}")


def trunc_div(numerator, denominator):
    """Rust's integer `/`, which truncates toward zero where python's `//`
    floors. On a negative numerator that does not divide evenly the two differ
    by one, which is a centipawn of evaluation."""
    quotient = abs(numerator) // denominator
    return quotient if numerator >= 0 else -quotient


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
        if slot >= MATERIAL_SLOT:
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
    """The calibration group, which nothing here reads.

    Its rows are held apart rather than masked out. A mask is a convention: it
    works while every caller remembers it, and the one that forgets is the one
    that spends the group. These rows are not in the corpus's matrices at all,
    so `loss`, `cv` and `fit` are handed a corpus that does not contain them
    and cannot reach them by accident. Deleting the calibration rows from the
    corpus file changes nothing any of the three prints, and a test says so.

    What a run may say about it is how big it is, which is what the header
    prints and what says the group exists. `unseal` is the door the arm that
    holds final weights walks through, and nothing in this file calls it.

    The labels are held apart as well, and upstream of here. A position that a
    training game and a calibration game both reached is labelled by whichever
    of the two groups owns it and by that group's appearances alone, so no
    result crosses the seal in either direction. What that costs is the dropped
    appearances, which `build_corpus.py` counts.
    """

    def __init__(self, rows, labels):
        self._rows = rows
        self._labels = labels

    @property
    def positions(self):
        return len(self._rows)

    @property
    def games(self):
        return len({self._labels[row.id].game for row in self._rows})

    @property
    def pairs(self):
        return len({self._labels[row.id].pair for row in self._rows})

    @property
    def appearances(self):
        return sum(self._labels[row.id].count for row in self._rows)

    def unseal(self):
        """The rows, for the arm whose weights are final."""
        return list(self._rows)


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
    and no way to score them.
    """

    def __init__(self, weights, rows, labels):
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
        groups = {row.id: group_of(labels[row.id].pair) for row in joined}
        self.sealed = Sealed(
            [row for row in joined if groups[row.id] == CALIBRATION], labels
        )
        kept = [row for row in joined if groups[row.id] != CALIBRATION]
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
        psqt, material = [], []
        for index, row in enumerate(kept):
            for slot, coefficient in row.coefficients:
                (material if slot >= MATERIAL_SLOT else psqt).append(
                    (index, slot, coefficient)
                )
        self.psqt = self._arrays(psqt)
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
        rows, slots, values = self.psqt
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
        for rows, slots, values in (self.psqt, self.material):
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
        rows, slots, values = self.psqt
        gradient = np.bincount(
            slots, values * per_row[rows] / TOTAL_PHASE, minlength=SLOTS
        )
        rows, slots, values = self.material
        return gradient + np.bincount(slots, values * per_row[rows], minlength=SLOTS)

    def support(self, mask=None):
        """How many of the rows each slot appears in. A weight the corpus
        barely constrains says so here rather than after it has shipped."""
        counts = np.zeros(SLOTS, dtype=np.int64)
        for rows, slots, _ in (self.psqt, self.material):
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
    corpus per step, which for 774 weights is a great many passes. The
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
    rows, slots, values = corpus.psqt
    material_rows, material_slots, material_values = corpus.material
    picked = mask[rows]
    material_picked = mask[material_rows]
    renumber = np.cumsum(mask) - 1
    psqt_part = (renumber[rows[picked]], slots[picked], values[picked])
    material_part = (
        renumber[material_rows[material_picked]],
        material_slots[material_picked],
        material_values[material_picked],
    )
    size = int(np.sum(mask))

    def scores_of(weights):
        numerator = np.bincount(
            psqt_part[0], psqt_part[2] * weights[psqt_part[1]], minlength=size
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
            psqt_part[1],
            psqt_part[2] * per_row[psqt_part[0]] / TOTAL_PHASE,
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
    """
    tables = np.abs(np.asarray(weights)[:MATERIAL_SLOT])
    midgame = tables[:MIDGAME_SLOTS].reshape(6, 64)
    endgame = tables[MIDGAME_SLOTS:MATERIAL_SLOT].reshape(6, 64)
    worst = 2 * max(int(midgame.max(axis=0).sum()), int(endgame.max(axis=0).sum()))
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


def choose_penalty(corpus, penalties, start, frozen, iterations, k, out=None):
    """The ridge, chosen on the selection group.

    Every penalty on the grid is fitted on the training games alone and scored
    on the selection games, which no fit read. The lowest selection loss wins,
    and a fit whose tables outgrew what the packed halves carry is no candidate
    whatever it scores.

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
            objective_for(corpus, corpus.train, k, start, penalty, frozen),
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


def load(args):
    weights, rows = parse_terms(
        Path(args.terms).read_text(encoding="utf-8").splitlines()
    )
    labels = parse_corpus(Path(args.corpus).read_text(encoding="utf-8").splitlines())
    corpus = Corpus(weights, rows, labels)
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


def frozen_slots(free_material):
    """Which weights a fit holds where they are.

    Material is held for a first fit. `eval::material` is read by the delta
    margin in quiescence, so moving a material value changes which captures
    quiescence skips, which changes the tree for a reason that has nothing to
    do with the evaluation's accuracy.
    """
    frozen = np.zeros(SLOTS, dtype=bool)
    if not free_material:
        frozen[MATERIAL_SLOT:] = True
    return frozen


def command_cv(args):
    corpus = load(args)
    start = corpus.weights.copy()
    frozen = frozen_slots(args.free_material)
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
    frozen = frozen_slots(args.free_material)
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


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    for name in ("loss", "cv", "fit"):
        command = commands.add_parser(name)
        command.add_argument("--terms", required=True, help="an arche terms run")
        command.add_argument("--corpus", required=True, help="the epd it was run over")
    for name in ("loss", "fit"):
        # cv fits K per fold on that fold's training games, so there is no one
        # constant for a caller to name
        commands.choices[name].add_argument(
            "--k", type=float, help="the scaling constant, if it is known"
        )
    loss = commands.choices["loss"]
    loss.add_argument("--weights", nargs="*", help="candidate vectors, as json arrays")
    for name in ("cv", "fit"):
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
    fit = commands.choices["fit"]
    fit.add_argument("--out", help="where to write the fitted vector")
    args = parser.parse_args(argv)
    return {"loss": command_loss, "cv": command_cv, "fit": command_fit}[args.command](
        args
    )


if __name__ == "__main__":
    sys.exit(main())
