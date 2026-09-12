#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2022-2026 Andrew Wright

"""Fit the attention model the deep reduction and the late move pruning gate on.

    arche reductions 8 every 1 cap 2000000 > ledger.txt
    scripts/fit_attention.py ledger.txt

The engine carries thirteen `ATTENTION_*` integers in `arche-core/src/engine.rs`
and reads them as a dot product against a late quiet move's features. This is
where they come from: a logistic regression over the reduction ledger, split by
fen so a position cannot be in both halves, quantized to fixed point at a scale
of 1024 so the gate is integer arithmetic and a compare.

What the model is asked is which reduced scouts deserve attention, meaning the
scout failed high or the replay called its fail low harmful. The rest are the
dead region, and a threshold on the score is what the engine reduces harder or
skips inside. The script prints the same score at four coverage targets, each
threshold chosen on the training half and the coverage and attention rate read
off the holdout half, which is the table `DEEP_REDUCTION_THRESHOLD` was picked
from. It prints a two-feature gate on the index and the history fraction beside
it, because a model is only worth carrying if it beats the obvious rule.

The weights in the engine today were fitted on 193,143 rows recorded at commit
5217271 and are not reproducible from this file alone: the ledger that produced
them was recorded before the deep reduction and the late move pruning existed,
so an engine running the command above now records a different tree. The
command is the documented way to make a new ledger, not a way back to the old
one.

The row format is the one `arche-core/src/reduction.rs` prints today: eighteen
whitespace separated fields with the fen last,

    depth window index searched generated history history_max killer tt
    eval_beta alpha_gap alpha scout cost reference label reduction fen

The ledger at 5217271 had seventeen, without `reduction`, and its history was
never negative. Both have moved since, so this parser reads the current format
and follows the engine in two places: a negative history counts as no history,
the way `engine.rs` clamps it before dividing, and a row the pruning skipped
outright is not a scout and is left out of the fit.

Features, all integers, so that the engine's gate is a dot product and a
compare: depth, index, band8_15, band16p, hist_milli (1000 * history //
history_max, and zero when nothing in the list has any), killer, tt_move,
tt_score_only, eval_beta, alpha_gap, generated, searched.

numpy and nothing else. The fit is Newton's method with an L2 penalty, written
out, which is what the tuner beside this does for the same reason.
"""

import argparse
import hashlib
import sys
from pathlib import Path

import numpy as np

# fixed point scale, 2**10 = 1024, which is the scale engine.rs reads the
# ATTENTION_ constants at
SHIFT = 10

# the feature columns, in the order the engine sums them
FEATURES = [
    "depth",
    "index",
    "band8_15",
    "band16p",
    "hist_milli",
    "killer",
    "tt_move",
    "tt_score_only",
    "eval_beta",
    "alpha_gap",
    "generated",
    "searched",
]

# what each weight is called in arche-core/src/engine.rs, so the fit can be
# read straight across into the constants
CONSTANTS = {name: f"ATTENTION_{name.upper()}" for name in FEATURES}
CONSTANTS["band8_15"] = "ATTENTION_BAND8_15"
CONSTANTS["band16p"] = "ATTENTION_BAND16P"
CONSTANTS["intercept"] = "ATTENTION_INTERCEPT"

# the eighteen fields of a row, up to the fen, which holds the rest of the line
COLUMNS = 18
# the field names, in the order reduction.rs writes them
FIELDS = [
    "depth",
    "window",
    "index",
    "searched",
    "generated",
    "history",
    "history_max",
    "killer",
    "tt",
    "eval_beta",
    "alpha_gap",
    "alpha",
    "scout",
    "cost",
    "reference",
    "label",
    "reduction",
    "fen",
]


def parse_file(path):
    """One ledger, as its header line and the rows that are scouts.

    A skipped row is a move the pruning never searched. It carries a reference
    and a label like a fail low does, but it is not a scout, and the model is
    fitted on what the scouts did.
    """
    lines = path.read_text(encoding="utf-8").splitlines()
    if not lines or not lines[0].startswith("reductions depth"):
        sys.exit(f"{path}: not a reductions ledger, first line {lines[:1]}")
    rows = []
    skipped = 0
    for number, line in enumerate(lines[1:], 2):
        if not line:
            break  # the blank line before the summary
        # the fen is last and holds the rest of the line, so a row short of a
        # column still splits into eighteen and every field after the missing
        # one is read as the one before it. The width is not the check; the
        # fields either side of the fen are
        words = line.split(" ", COLUMNS - 1)
        if len(words) != COLUMNS:
            sys.exit(f"{path}:{number}: {len(words)} fields, not {COLUMNS}: {line}")
        row = dict(zip(FIELDS, words))
        try:
            numbers = {
                name: int(row[name])
                for name in (
                    "depth",
                    "index",
                    "searched",
                    "generated",
                    "history",
                    "history_max",
                    "eval_beta",
                    "alpha_gap",
                    "reduction",
                )
            }
        except ValueError as wrong:
            sys.exit(f"{path}:{number}: not the row format this reads ({wrong})")
        if "/" not in row["fen"]:
            sys.exit(f"{path}:{number}: the last field is not a fen: {row['fen']}")
        if row["scout"] == "skipped":
            skipped += 1
            continue
        rows.append(
            {
                **numbers,
                "killer": 1 if row["killer"] == "killer" else 0,
                "tt": row["tt"],
                "scout": row["scout"],
                "label": row["label"],
                "fen": row["fen"],
            }
        )
    return lines[0], rows, skipped


def attention(row):
    """Whether this scout deserved attention: it failed high, or it failed low
    and the replay called that fail low harmful."""
    if row["scout"] == "high":
        return 1
    return 1 if row["label"] == "harmful" else 0


def fen_parity(fen):
    """Which half a position belongs to. By the fen and not by the row, so that
    two rows from one position cannot land on opposite sides and let the
    holdout score a position the fit has already seen."""
    return hashlib.sha256(fen.encode("utf-8")).digest()[0] & 1


def features_of(row):
    """The feature vector, computed the way engine.rs computes it at the gate.

    A history below zero is a move the table has marked down. The engine takes
    the larger of the score and nought before dividing, so nothing in the list
    being liked and everything in it being disliked are the same nothing here.
    """
    hist_milli = 0
    if row["history_max"] > 0:
        hist_milli = 1000 * max(row["history"], 0) // row["history_max"]
    return [
        row["depth"],
        row["index"],
        1 if 8 <= row["index"] <= 15 else 0,
        1 if row["index"] >= 16 else 0,
        hist_milli,
        row["killer"],
        1 if row["tt"] == "move" else 0,
        1 if row["tt"] == "score_only" else 0,
        row["eval_beta"],
        row["alpha_gap"],
        row["generated"],
        row["searched"],
    ]


def fit_logistic(X, y, l2=1.0, iters=500):
    """Newton's method with an L2 penalty, on standardized columns.

    The penalty is off the intercept: a prior on how large a weight should be
    has nothing to say about the base rate. The standardization is folded back
    out at the end, so the weights returned are on the raw integer scale the
    engine reads.
    """
    mu = X.mean(axis=0)
    sd = X.std(axis=0)
    sd[sd == 0] = 1.0
    Z = (X - mu) / sd
    n, d = Z.shape
    Zb = np.hstack([Z, np.ones((n, 1))])
    w = np.zeros(d + 1)
    reg = np.full(d + 1, l2)
    reg[-1] = 0.0
    for _ in range(iters):
        p = 1.0 / (1.0 + np.exp(-Zb @ w))
        g = Zb.T @ (p - y) + reg * w
        weights = p * (1 - p)
        hessian = (Zb * weights[:, None]).T @ Zb + np.diag(reg)
        step = np.linalg.solve(hessian, g)
        w -= step
        if np.max(np.abs(step)) < 1e-10:
            break
    return w[:-1] / sd, w[-1] - np.sum(w[:-1] * mu / sd)


def auc_of(scores, y):
    """The area under the roc curve, by ranks, with ties sharing a rank.

    It is nan when every row is one class, which a small ledger can be: no
    ordering separates one class from an empty one.
    """
    order = np.argsort(scores, kind="mergesort")
    ranks = np.empty(len(scores), dtype=float)
    ranks[order] = np.arange(1, len(scores) + 1)
    sorted_scores = scores[order]
    at = 0
    while at < len(sorted_scores):
        last = at
        while (
            last + 1 < len(sorted_scores)
            and sorted_scores[last + 1] == sorted_scores[at]
        ):
            last += 1
        if last > at:
            ranks[order[at : last + 1]] = (at + last + 2) / 2.0
        at = last + 1
    positive = y == 1
    ones = positive.sum()
    zeros = len(y) - ones
    if ones == 0 or zeros == 0:
        return float("nan")
    return (ranks[positive].sum() - ones * (ones + 1) / 2) / (ones * zeros)


def rate(values):
    """The share of `values` that are true, as a percentage, or nan when there
    are none of them. A rate over no rows is not a zero."""
    return float(values.mean() * 100) if len(values) else float("nan")


def operating_table(train_scores, hold_scores, hold_y, deep, targets=(90, 75, 50, 25)):
    """What the dead region holds, at four coverage targets.

    The threshold is a percentile of the training scores and everything read
    off it is the holdout's, so the coverage is a promise made on one half and
    checked on the other. `deep` marks the holdout rows at depth four and up,
    which is where the engine's gate can reach.
    """
    table = []
    for target in targets:
        threshold = float(np.percentile(train_scores, target))
        dead = hold_scores < threshold
        dead_deep = dead & deep
        table.append(
            {
                "target": target,
                "threshold": threshold,
                "coverage": rate(dead),
                "attention": rate(hold_y[dead]),
                "dead": int(dead.sum()),
                "attention_n": int(hold_y[dead].sum()),
                "deep_coverage": (
                    100 * dead_deep.sum() / deep.sum() if deep.sum() else float("nan")
                ),
                "deep_attention": rate(hold_y[dead_deep]),
                "deep_dead": int(dead_deep.sum()),
                "deep_attention_n": int(hold_y[dead_deep].sum()),
            }
        )
    return table


def print_table(name, table):
    print(f"\n{name}")
    print(
        "target%  threshold   cov%  attn%   n_dead  attn_n"
        " | depth 4+: cov%  attn%   n_dead  attn_n"
    )
    for row in table:
        print(
            f"{row['target']:>6}  {row['threshold']:>9.1f}"
            f"  {row['coverage']:>5.2f}  {row['attention']:>5.3f}"
            f"  {row['dead']:>7}  {row['attention_n']:>6}"
            f" |           {row['deep_coverage']:>5.2f}  {row['deep_attention']:>5.3f}"
            f"  {row['deep_dead']:>7}  {row['deep_attention_n']:>6}"
        )


def print_trivial_gate(X, y, train, hold):
    """The obvious rule, for the model to be worth more than: dead when the
    move is late enough and the history table thinks little enough of it."""
    index = X[:, FEATURES.index("index")]
    history = X[:, FEATURES.index("hist_milli")]
    print(
        "\ntrivial gate (index >= I and hist_milli <= H), chosen on train:"
        "\ntarget%   I     H  cov%(train) | holdout: cov%  attn%   n_dead  attn_n"
    )
    found = []
    for cut in (4, 5, 6, 7, 8, 10, 12, 16, 20):
        for floor in (0, 10, 25, 50, 100, 200, 300, 500, 750, 1000):
            dead = (index[train] >= cut) & (history[train] <= floor)
            found.append((cut, floor, rate(dead), rate(y[train][dead])))
    for target in (90, 75, 50, 25):
        usable = [one for one in found if one[2] >= target and not np.isnan(one[3])]
        if not usable:
            print(f"{target:>6}   none reachable")
            continue
        # the lowest attention rate among the gates that cover the target, and
        # the tightest coverage of those, since a gate that covers far more
        # than it was asked for is not the gate that was asked for
        cut, floor, coverage, _ = min(usable, key=lambda one: (one[3], one[2]))
        dead = (index[hold] >= cut) & (history[hold] <= floor)
        print(
            f"{target:>6}  {cut:>2}  {floor:>4}  {coverage:>11.2f}"
            f" |          {rate(dead):>5.2f}"
            f"  {rate(y[hold][dead]):>5.3f}"
            f"  {int(dead.sum()):>7}  {int(y[hold][dead].sum()):>6}"
        )


def write_csvs(out_dir, X, y, depth, rows, train, hold, weights, intercept):
    """The two halves and the fitted weights, for reading back by hand."""
    out_dir.mkdir(parents=True, exist_ok=True)
    header = ",".join([*FEATURES, "attention", "depth", "fen"])
    for mask, name in ((train, "train.csv"), (hold, "holdout.csv")):
        with (out_dir / name).open("w", encoding="utf-8") as out:
            out.write(header + "\n")
            for at in np.where(mask)[0]:
                cells = ",".join(str(int(value)) for value in X[at])
                out.write(f'{cells},{int(y[at])},{depth[at]},"{rows[at]["fen"]}"\n')
    with (out_dir / "weights.txt").open("w", encoding="utf-8") as out:
        out.write("# logistic regression on the reduction ledger\n")
        out.write(f"# fixed point scale 2^{SHIFT} = {1 << SHIFT}\n")
        out.write("feature,float_weight,quantized_weight\n")
        for name, raw, fixed in zip(FEATURES, weights, quantize(weights)):
            out.write(f"{name},{raw:.8f},{fixed}\n")
        out.write(f"intercept,{intercept:.8f},{quantize([intercept])[0]}\n")
    print(f"\nwrote train.csv, holdout.csv and weights.txt under {out_dir}")


def quantize(weights):
    """Float weights as the integers engine.rs reads, at a scale of 2**SHIFT."""
    return np.round(np.asarray(weights) * (1 << SHIFT)).astype(np.int64)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "ledger",
        type=Path,
        nargs="+",
        help="what `arche reductions` printed, one file or several",
    )
    parser.add_argument(
        "--out-dir",
        type=Path,
        help="where to write the two halves and the weights, if anywhere",
    )
    args = parser.parse_args(argv)

    rows = []
    for path in args.ledger:
        header, read, skipped = parse_file(path)
        print(f"{path.name}: {header}")
        print(f"  {len(read)} scouts, {skipped} skipped rows left out")
        rows.extend(read)
    if not rows:
        sys.exit("no scouts in the ledger, nothing to fit")

    print("\nper depth:")
    depths = {}
    for row in rows:
        seen = depths.setdefault(row["depth"], [0, 0, 0])
        seen[0] += 1
        seen[1] += row["scout"] == "high"
        seen[2] += attention(row)
    for depth in sorted(depths):
        rows_at, high, attention_at = depths[depth]
        print(f"  depth {depth}: {rows_at} rows, {high} high, {attention_at} attention")

    X = np.array([features_of(row) for row in rows], dtype=float)
    y = np.array([attention(row) for row in rows], dtype=float)
    depth = np.array([row["depth"] for row in rows])
    parity = np.array([fen_parity(row["fen"]) for row in rows])
    train = parity == 0
    hold = parity == 1
    print(
        f"\nsplit by fen: train {train.sum()} rows"
        f" ({int(y[train].sum())} attention),"
        f" holdout {hold.sum()} rows ({int(y[hold].sum())} attention)"
    )
    if not train.any() or not hold.any():
        sys.exit("one half of the split is empty, the ledger is too small to fit")

    weights, intercept = fit_logistic(X[train], y[train])
    fixed = quantize(weights)
    fixed_intercept = quantize([intercept])[0]
    print(f"\nweights, fixed point at a scale of 2^{SHIFT} = {1 << SHIFT}:")
    for name, raw, one in zip(FEATURES, weights, fixed):
        print(f"  {CONSTANTS[name]:<26} {one:>8d}    ({raw:.6f})")
    print(f"  {CONSTANTS['intercept']:<26} {fixed_intercept:>8d}    ({intercept:.6f})")

    scores = (X.astype(np.int64) @ fixed + fixed_intercept).astype(float)
    print(
        f"\nholdout auc: {auc_of(scores[hold], y[hold]):.4f}"
        f"   train auc: {auc_of(scores[train], y[train]):.4f}"
    )
    print(f"holdout attention rate over every row: {rate(y[hold]):.3f}%")

    deep = depth[hold] >= 4
    print_table(
        "operating table: thresholds from the train percentiles, rates on the holdout",
        operating_table(scores[train], scores[hold], y[hold], deep),
    )
    print_trivial_gate(X, y, train, hold)

    if args.out_dir:
        write_csvs(args.out_dir, X, y, depth, rows, train, hold, weights, intercept)
    return 0


if __name__ == "__main__":
    sys.exit(main())
