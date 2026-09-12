# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2022-2026 Andrew Wright

"""Tests for the attention model fit.

The thirteen integers in the engine came from a ledger of 193,143 rows, which
is not a test's budget and is not reproducible anyway: it was recorded before
the deep reduction and the late move pruning existed, so the same command on
today's engine records a different tree. What is held here instead is that the
script reads the format it declares and that the fit is a fit.

`reductions_sample.txt` is a real ledger, `arche reductions 8 every 220 cap
800` on the bench suite at ccd1804. It has 791 rows over five depths, 205 of
them skipped by the pruning, 607 with a history the table has marked below
zero, and one row worth attention, which is what a few hundred rows of this
data looks like: the rate is under a percent. So the sample covers the parse
and the run, and the planted case below covers the fit, because a fit on one
positive row says nothing about whether the arithmetic is right.
"""

import subprocess
import sys
from pathlib import Path

import fit_attention
import numpy as np
import pytest
from conftest import SCRIPTS

SCRIPT = SCRIPTS / "fit_attention.py"
SAMPLE = Path(__file__).resolve().parent / "reductions_sample.txt"


def row(index, attention, fen_id, depth=6, history=0, history_max=0):
    """One ledger row, in the eighteen fields reduction.rs prints.

    A fail high is a row worth attention on its own and has no label to give,
    so it prints `-` there the way the engine does.
    """
    scout, label = ("high", "-") if attention else ("low", "harmless")
    return (
        f"{depth} zw {index} {index + 1} 30 {history} {history_max} plain miss"
        f" -20 19 -120 {scout} 139 6 {label} 1"
        f" 8/8/8/8/8/8/8/K6k w - - 0 {fen_id}"
    )


def ledger(rows):
    return "reductions depth 6 every 1 positions 1 events 99 records 9\n" + "\n".join(
        rows
    )


def run(*arguments):
    return subprocess.run(
        [sys.executable, str(SCRIPT), *arguments],
        capture_output=True,
        text=True,
        check=False,
    )


class TestParsing:
    def test_the_sample_reads_as_the_format_the_script_declares(self):
        header, rows, skipped = fit_attention.parse_file(SAMPLE)
        assert header.startswith("reductions depth 8")
        assert len(rows) == 586
        assert skipped == 205
        assert {len(fit_attention.features_of(one)) for one in rows} == {12}

    def test_a_row_the_pruning_skipped_is_not_a_scout(self, tmp_path):
        # a skipped move was never searched, so it is not a scout that failed
        # one way or the other and has no place in what the scouts did
        text = ledger(
            [
                row(4, False, 1),
                row(5, False, 2).replace(" low ", " skipped "),
                row(6, True, 3),
            ]
        )
        path = tmp_path / "ledger.txt"
        path.write_text(text)
        _, rows, skipped = fit_attention.parse_file(path)
        assert [one["index"] for one in rows] == [4, 6]
        assert skipped == 1

    def test_a_row_of_the_wrong_width_stops_the_run(self, tmp_path):
        # the format has moved once already, gaining the reduction column, and
        # a parser that read the old width off the new rows would put every
        # feature in the wrong column and fit something
        path = tmp_path / "ledger.txt"
        path.write_text(ledger([row(4, False, 1).replace(" 1 8/8", " 8/8")]))
        with pytest.raises(SystemExit) as raised:
            fit_attention.parse_file(path)
        assert "not the row format this reads" in str(raised.value)

    def test_anything_but_a_ledger_stops_the_run(self, tmp_path):
        path = tmp_path / "ledger.txt"
        path.write_text("cutoffs depth 8\n")
        with pytest.raises(SystemExit):
            fit_attention.parse_file(path)


class TestFeatures:
    def one(self, **fields):
        made = {
            "depth": 6,
            "index": 4,
            "searched": 5,
            "generated": 30,
            "history": 0,
            "history_max": 0,
            "killer": 0,
            "tt": "miss",
            "eval_beta": 0,
            "alpha_gap": 0,
            "scout": "low",
            "label": "harmless",
            "fen": "8/8/8/8/8/8/8/K6k w - - 0 1",
        }
        made.update(fields)
        return made

    def milli(self, **fields):
        at = fit_attention.FEATURES.index("hist_milli")
        return fit_attention.features_of(self.one(**fields))[at]

    def test_a_history_the_table_likes_is_a_fraction_of_the_largest(self):
        assert self.milli(history=250, history_max=1000) == 250

    def test_a_history_below_zero_counts_as_none(self):
        # engine.rs takes the larger of the score and nought before dividing,
        # so a move the table has marked down and a move it knows nothing
        # about read alike at the gate. 607 rows of the sample are this case
        assert self.milli(history=-4000, history_max=1000) == 0

    def test_nothing_in_the_list_having_any_divides_nothing(self):
        assert self.milli(history=0, history_max=0) == 0

    def test_attention_is_a_fail_high_or_a_harmful_fail_low(self):
        assert fit_attention.attention(self.one(scout="high", label="-")) == 1
        assert fit_attention.attention(self.one(scout="low", label="harmful")) == 1
        assert fit_attention.attention(self.one(scout="low", label="harmless")) == 0

    def test_the_two_halves_are_split_by_position_and_not_by_row(self):
        # two rows of one position must land on the same side, or the holdout
        # scores a position the fit has already seen
        fen = "8/8/8/8/8/8/8/K6k w - - 0 1"
        assert fit_attention.fen_parity(fen) == fit_attention.fen_parity(fen)


class TestFitting:
    def test_a_planted_signal_comes_back_with_its_sign(self, tmp_path):
        # attention iff the move is early, which is the shape the real ledger
        # has, so the index weight has to come back negative and the model
        # has to order the holdout better than a coin
        rows = [row(at % 20, at % 20 < 4, at) for at in range(400)]
        path = tmp_path / "ledger.txt"
        path.write_text(ledger(rows))
        _, read, _ = fit_attention.parse_file(path)
        X = np.array([fit_attention.features_of(one) for one in read], dtype=float)
        y = np.array([fit_attention.attention(one) for one in read], dtype=float)
        parity = np.array([fit_attention.fen_parity(one["fen"]) for one in read])
        weights, intercept = fit_attention.fit_logistic(X[parity == 0], y[parity == 0])
        assert weights[fit_attention.FEATURES.index("index")] < 0
        scores = X @ weights + intercept
        assert fit_attention.auc_of(scores[parity == 1], y[parity == 1]) > 0.9

    def test_the_quantisation_is_the_scale_the_engine_reads(self):
        # engine.rs sums the ATTENTION_ constants as integers, so the weights
        # have to arrive as integers at 1024 times their float value
        assert fit_attention.SHIFT == 10
        assert list(fit_attention.quantize([1.0, -0.5, 0.0])) == [1024, -512, 0]

    def test_one_class_alone_has_no_area_under_its_curve(self):
        # a few hundred rows can hold no attention at all, and an ordering
        # that separates one class from an empty one is not a score of one
        alone = fit_attention.auc_of(np.array([1.0, 2.0, 3.0]), np.zeros(3))
        assert np.isnan(alone)


class TestCommandLine:
    def test_the_sample_runs_and_prints_the_weights_and_the_table(self):
        done = run(str(SAMPLE))
        assert done.returncode == 0, done.stderr
        out = done.stdout
        assert "fixed point at a scale of 2^10 = 1024" in out
        # the thirteen the engine carries: twelve features and the intercept
        named = [name for name in fit_attention.CONSTANTS.values() if name in out]
        assert len(named) == 13
        assert "operating table" in out
        for target in ("    90 ", "    75 ", "    50 ", "    25 "):
            assert target in out
        assert "205 skipped rows left out" in out

    def test_the_halves_and_the_weights_are_written_when_asked(self, tmp_path):
        done = run(str(SAMPLE), "--out-dir", str(tmp_path))
        assert done.returncode == 0, done.stderr
        written = (tmp_path / "weights.txt").read_text()
        assert "feature,float_weight,quantized_weight" in written
        assert len(written.splitlines()) == 3 + len(fit_attention.FEATURES) + 1
        for name in ("train.csv", "holdout.csv"):
            assert (tmp_path / name).read_text().startswith("depth,index,")

    def test_a_ledger_with_no_scouts_says_so_rather_than_fitting_nothing(
        self, tmp_path
    ):
        path = tmp_path / "ledger.txt"
        path.write_text(ledger([row(4, False, 1).replace(" low ", " skipped ")]))
        done = run(str(path))
        assert done.returncode != 0
        assert "no scouts" in done.stderr
