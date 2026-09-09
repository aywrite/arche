#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2022-2026 Andrew Wright

"""Score a weight vector against the games, and fit a new one.

`arche terms` prints, for each quiet position of a corpus, the coefficient
every evaluation weight is multiplied by. `scripts/build_corpus.py` prints what
the games those positions came from ended in. This joins the two and does the
two things that pair is for:

    python3 scripts/tune.py loss --terms rows.txt --corpus corpus.epd
    python3 scripts/tune.py fit  --terms rows.txt --corpus corpus.epd --out fit.json

`loss` is the triage instrument. A weight vector is scored over the whole
corpus in one matrix-vector product, which is milliseconds, so a candidate can
be killed before it costs a match. `fit` is the tuner, and it exists so that
the loss has something to be measured against.

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
import hashlib
import json
import math
import sys
from pathlib import Path

import numpy as np

# The weight vector, as `arche-core/src/tune.rs` lays it out: 384 midgame table
# entries, then 128 endgame ones, then the six material values.
MIDGAME_SLOTS = 6 * 64
ENDGAME_SLOTS = 2 * 64
MATERIAL_SLOT = MIDGAME_SLOTS + ENDGAME_SLOTS
SLOTS = MATERIAL_SLOT + 6

# What the opening's pieces come to on the scale the taper is read at, which is
# what the piece square half of a row divides by.
TOTAL_PHASE = 24

# The pieces the phase buckets count: neither pawns nor kings, both colours.
PIECES_COUNTED = "nbrqNBRQ"

# The buckets the game-corpus report stratified by, and the harness after it.
BUCKETS = ("0-6", "7-12", "13+")


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


def held_out(fen):
    """Which side of the split a position falls on: sha256 of the fen, the low
    bit of the first byte. Odd is held out.

    The house pattern, and its property is the one that matters: rows that
    share a fen land on the same side by construction, so the split leaks no
    position across itself.
    """
    return hashlib.sha256(fen.encode("utf-8")).digest()[0] & 1 == 1


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


class Row:
    """One position: what the engine said it scored, and what of."""

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
        if not line or line.startswith("terms "):
            continue
        words = line.split(" ")
        if words[0] == "weights":
            count = int(words[1])
            weights = [int(word) for word in words[2 : 2 + count]]
            if len(weights) != count or count != SLOTS:
                raise ValueError(f"a weights line of {len(weights)}, expected {SLOTS}")
            continue
        if weights is None:
            raise ValueError("a row arrived before the weights line")
        identifier, evaluation, phase, count = (
            words[0],
            int(words[1]),
            int(words[2]),
            int(words[3]),
        )
        coefficients = []
        for word in words[4 : 4 + count]:
            slot, coefficient = word.split(":")
            coefficients.append((int(slot), int(coefficient)))
        fen = " ".join(words[4 + count :])
        rebuilt = reconstruct(coefficients, weights)
        if rebuilt != evaluation:
            raise ValueError(
                f"{identifier} rebuilds to {rebuilt} and the engine says {evaluation}"
            )
        rows.append(Row(identifier, evaluation, phase, coefficients, fen))
    if weights is None:
        raise ValueError("no weights line")
    return weights, rows


def parse_corpus(lines):
    """The label of each position: the game result from the side to move's
    point of view, and how many games it came from, by id.

    Read the way the engine reads epd, which is the point of reading it here
    at all: the first four words are the position and what follows them is
    operations, each an opcode and its operands, ended by a semicolon.
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
        labels[operations["id"]] = (
            float(operations["result"]),
            int(operations.get("count", 1)),
        )
    return labels


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
    """

    def __init__(self, weights, rows, labels):
        kept = [row for row in rows if row.id in labels]
        self.rows = kept
        self.weights = np.array(weights, dtype=np.float64)
        self.evals = np.array([row.eval for row in kept], dtype=np.float64)
        self.results = np.array([labels[row.id][0] for row in kept], dtype=np.float64)
        self.counts = np.array([labels[row.id][1] for row in kept], dtype=np.float64)
        self.holdout = np.array([held_out(row.fen) for row in kept], dtype=bool)
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


def paired_difference(first, second, counts):
    """The mean difference between two weight vectors' per-position errors, and
    its standard error.

    The two are scored on the same positions, so the difference is a paired
    sample and its mean has an interval. A loss difference whose interval
    covers zero is not a difference, which is why this never returns a bare
    delta.
    """
    weight = counts / np.sum(counts)
    difference = second - first
    mean = float(np.sum(weight * difference))
    variance = float(np.sum(weight * (difference - mean) ** 2))
    effective = float(np.sum(counts) ** 2 / np.sum(counts**2))
    return mean, math.sqrt(variance / effective) if effective > 1 else float("nan")


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
    corpus per step, which for 518 weights is a great many passes. The
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
    # the four shared tables read the same array at either end of the taper,
    # so the endgame boardful is the pawn's and the king's endgame tables with
    # those four between them
    endgame = np.vstack(
        [
            tables[MIDGAME_SLOTS : MIDGAME_SLOTS + 64],
            midgame[1:5],
            tables[MIDGAME_SLOTS + 64 : MATERIAL_SLOT],
        ]
    )
    worst = 2 * max(int(midgame.max(axis=0).sum()), int(endgame.max(axis=0).sum()))
    return worst < 32767, worst


def read_weights(path):
    return np.array(
        json.loads(Path(path).read_text(encoding="utf-8")), dtype=np.float64
    )


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
    """The loss table: each weight vector on each side of the split, pooled and
    stratified, with a paired difference against the first."""
    out = sys.stdout if out is None else out
    train = ~corpus.holdout
    holdout = corpus.holdout
    print(
        f"corpus positions {len(corpus)} train {int(train.sum())} "
        f"holdout {int(holdout.sum())} games {int(corpus.counts.sum())}",
        file=out,
    )
    wins = float(np.sum(corpus.counts * corpus.results) / np.sum(corpus.counts))
    draws = float(
        np.sum(corpus.counts * (corpus.results == 0.5)) / np.sum(corpus.counts)
    )
    print(f"results mean {wins:.4f} drawn {100 * draws:.1f}%", file=out)
    for bucket in BUCKETS:
        share = corpus.buckets == bucket
        print(
            f"pieces {bucket} {int(share.sum())} "
            f"({100.0 * share.sum() / max(len(corpus), 1):.1f}%)",
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
            f"holdout mse at own_k "
            f"{mean_squared_error(corpus.scores(np.asarray(weights, dtype=np.float64))[holdout], corpus.results[holdout], corpus.counts[holdout], own):.6f}",
            file=out,
        )
        for side, mask in (("train", train), ("holdout", holdout)):
            numbers = scored(corpus, weights, mask, k)
            print(
                f"{name} {side} mse {numbers['mse']:.6f} log {numbers['log']:.6f} "
                f"quantized_mse {numbers['quantized_mse']:.6f} "
                f"quantized_log {numbers['quantized_log']:.6f}",
                file=out,
            )
            if side == "holdout":
                for bucket in BUCKETS:
                    inside = corpus.buckets[mask] == bucket
                    if not inside.any():
                        continue
                    print(
                        f"{name} holdout pieces {bucket} {int(inside.sum())} mse "
                        f"{mean_squared_error(numbers['scores'][inside], corpus.results[mask][inside], corpus.counts[mask][inside], k):.6f}",
                        file=out,
                    )
                errors = squared_errors(numbers["scores"], corpus.results[mask], k)
                if baseline is None:
                    baseline = (name, errors)
                else:
                    mean, error = paired_difference(
                        baseline[1], errors, corpus.counts[mask]
                    )
                    print(
                        f"{name} against {baseline[0]} holdout mse {mean:+.6f} "
                        f"se {error:.6f}"
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


def load(args):
    weights, rows = parse_terms(
        Path(args.terms).read_text(encoding="utf-8").splitlines()
    )
    labels = parse_corpus(Path(args.corpus).read_text(encoding="utf-8").splitlines())
    corpus = Corpus(weights, rows, labels)
    if not len(corpus):
        raise SystemExit("tune.py: no row of the extraction is in the corpus")
    return corpus


def command_loss(args):
    corpus = load(args)
    train = ~corpus.holdout
    k = args.k or fit_k(
        corpus.scores(corpus.weights)[train],
        corpus.results[train],
        corpus.counts[train],
    )
    named = [("shipped", corpus.weights)]
    for path in args.weights or []:
        named.append((Path(path).stem, read_weights(path)))
    report(corpus, named, k)
    return 0


def command_fit(args):
    corpus = load(args)
    train = ~corpus.holdout
    holdout = corpus.holdout
    k = args.k or fit_k(
        corpus.scores(corpus.weights)[train],
        corpus.results[train],
        corpus.counts[train],
    )
    start = corpus.weights.copy()
    # material is held for the first fit. `eval::material` is read by the delta
    # margin in quiescence, so moving it changes which captures quiescence
    # skips, which changes the tree for a reason that has nothing to do with
    # the evaluation's accuracy
    frozen = np.zeros(SLOTS, dtype=bool)
    if not args.free_material:
        frozen[MATERIAL_SLOT:] = True
    best = None
    for penalty in args.penalties:
        fitted, _ = lbfgs(
            objective_for(corpus, train, k, start, penalty, frozen),
            start,
            args.iterations,
        )
        loss = mean_squared_error(
            corpus.scores(fitted)[holdout],
            corpus.results[holdout],
            corpus.counts[holdout],
            k,
        )
        inside, worst = bounds_hold(quantize(fitted))
        print(
            f"penalty {penalty:g} holdout mse {loss:.6f} "
            f"scale {table_scale(fitted, corpus.weights):.3f} boardful {worst}"
            + ("" if inside else " (outside the packed halves)"),
            file=sys.stderr,
        )
        # a vector the engine's arithmetic cannot carry is no candidate,
        # whatever it scores
        if inside and (best is None or loss < best[0]):
            best = (loss, penalty, fitted)
    if best is None:
        raise SystemExit("tune.py: every penalty on the grid left the tables too large")
    _, penalty, fitted = best
    print(f"chose penalty {penalty:g}", file=sys.stderr)
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
    for name in ("loss", "fit"):
        command = commands.add_parser(name)
        command.add_argument("--terms", required=True, help="an arche terms run")
        command.add_argument("--corpus", required=True, help="the epd it was run over")
        command.add_argument(
            "--k", type=float, help="the scaling constant, if it is known"
        )
    loss = commands.choices["loss"]
    loss.add_argument("--weights", nargs="*", help="candidate vectors, as json arrays")
    fit = commands.choices["fit"]
    fit.add_argument("--out", help="where to write the fitted vector")
    fit.add_argument("--iterations", type=int, default=300)
    fit.add_argument(
        "--penalties",
        type=float,
        nargs="+",
        default=[0.0, 1e-8, 1e-7, 1e-6, 1e-5, 1e-4],
        help="the ridge grid, chosen on the held-out split",
    )
    fit.add_argument(
        "--free-material",
        action="store_true",
        help="let the six material values move, which the first fit does not",
    )
    args = parser.parse_args(argv)
    return command_loss(args) if args.command == "loss" else command_fit(args)


if __name__ == "__main__":
    sys.exit(main())
