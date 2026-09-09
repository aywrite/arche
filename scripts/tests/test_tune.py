# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2022-2026 Andrew Wright

"""Tests for the tuner and its loss harness.

The seam is what most of these are about. The engine states a position's
coefficients and states the weights, and this file's job is to fold the two
back together and get the integer the engine got. Three details of that
arithmetic are the ones a python reader gets wrong, and each has a test that
says so by naming a case where getting it wrong gives a different answer.

The rest pin the formats the run parses, which is the stated reason the other
script tests exist, and the two claims the harness makes about its own
numbers: that a difference is never printed without its interval, and that a
weight vector the engine's arithmetic cannot carry is refused.
"""

import json

import numpy as np
import pytest
import tune

# A pawn, a knight, a bishop, a rook, a queen and a king, which is what the
# material slots hold today.
MATERIAL = [100, 310, 320, 500, 900, 10000]


def weights(entries=None):
    """A weight vector in the engine's layout, tables at nothing unless the
    caller names an entry."""
    vector = [0] * tune.SLOTS
    vector[tune.MATERIAL_SLOT :] = MATERIAL
    for slot, value in (entries or {}).items():
        vector[slot] = value
    return vector


def row(
    identifier, coefficients, vector, phase=24, fen="4k3/8/8/8/8/8/8/4K3 w - - 0 1"
):
    """One row in the shape `arche terms` prints it, with the evaluation it
    states worked out from the weights the same way the engine does."""
    evaluation = tune.reconstruct(coefficients, vector)
    terms = " ".join(f"{slot}:{coefficient}" for slot, coefficient in coefficients)
    return f"{identifier} {evaluation} {phase} {len(coefficients)} {terms} {fen}"


def extraction(rows, vector):
    header = f"terms positions {len(rows)} in_check 0 unsettled 0 kept {len(rows)}"
    line = "weights {} {}".format(tune.SLOTS, " ".join(str(value) for value in vector))
    return [header, line, *rows]


def test_the_divide_truncates_toward_zero():
    """Rust's `/` truncates and python's `//` floors, so on a negative
    numerator that does not divide evenly the two are a centipawn apart. This
    is the one a python reader is most likely to get wrong."""
    assert tune.trunc_div(-980, 24) == -40
    assert -980 // 24 == -41
    assert tune.trunc_div(980, 24) == 40
    # and they agree wherever the divide is exact, which is why a test that
    # picked its numerator carelessly would say nothing
    assert tune.trunc_div(-720, 24) == -720 // 24


def test_material_is_added_outside_the_divide():
    """Folding the material into the numerator by scaling it up gives a
    different integer."""
    vector = weights({0: -1})
    # a knight and a pawn's worth of material, and a piece square numerator of
    # minus one, which does not divide by the taper
    coefficients = [(0, 1), (tune.MATERIAL_SLOT, 1)]
    assert tune.reconstruct(coefficients, vector) == 100 + tune.trunc_div(-1, 24)
    folded = (100 * 24 - 1) // 24
    assert folded != tune.reconstruct(coefficients, vector)


def test_a_row_that_does_not_rebuild_stops_the_run():
    """The seam's whole claim is that this file and the engine agree on every
    row. A row that does not rebuild means they have parted company, so it
    raises rather than being dropped and fitted around."""
    vector = weights({0: 30})
    good = row("a", [(0, 24), (tune.MATERIAL_SLOT, 1)], vector)
    _, rows = tune.parse_terms(extraction([good], vector))
    assert len(rows) == 1
    words = good.split(" ")
    words[1] = str(int(words[1]) + 1)
    with pytest.raises(ValueError, match="rebuilds to"):
        tune.parse_terms(extraction([" ".join(words)], vector))


def test_a_row_reads_left_to_right_with_the_fen_last():
    """The count says where the coefficients stop, so the field that can hold
    spaces holds the rest of the line."""
    vector = weights({5: 12, 400: -7})
    fen = "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1"
    line = row(
        "start", [(5, 24), (400, -3), (tune.MATERIAL_SLOT + 4, -1)], vector, 18, fen
    )
    _, rows = tune.parse_terms(extraction([line], vector))
    assert rows[0].id == "start"
    assert rows[0].phase == 18
    assert rows[0].fen == fen
    assert rows[0].coefficients == [(5, 24), (400, -3), (tune.MATERIAL_SLOT + 4, -1)]


def test_a_weights_line_of_the_wrong_length_is_refused():
    """The layout is a contract between two files, and a vector of another
    length means one of them has changed and the other has not."""
    with pytest.raises(ValueError, match="weights line"):
        tune.parse_terms(["weights 3 1 2 3"])
    with pytest.raises(ValueError, match="no weights line"):
        tune.parse_terms(["terms positions 0 in_check 0 unsettled 0 kept 0"])


def test_a_corpus_line_is_read_the_way_the_engine_reads_epd():
    """Four fields and then operations, so the id is not looked for among the
    words of the position."""
    line = (
        "r1bqk2r/p3bppp/2n1pn2/2pp4/Pp2P3/3P1NP1/1PPN1PBP/R1BQ1RK1 w kq - "
        'id "g00001p016"; result "0.2500"; count "2";'
    )
    assert tune.parse_corpus([line]) == {"g00001p016": (0.25, 2)}
    # a line with no label is not a row to fit
    assert tune.parse_corpus(['4k3/8/8/8/8/8/8/4K3 w - - id "solo";']) == {}


def test_a_position_lands_on_one_side_of_the_split_however_often_it_appears():
    """Fen-hash parity, so rows that share a fen land on the same side by
    construction and the split leaks no position across itself."""
    fens = [
        "4k3/8/8/8/8/8/8/4K3 w - - 0 1",
        "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1",
        "8/8/8/4k3/8/8/8/4K3 b - - 0 1",
    ]
    for fen in fens:
        assert tune.held_out(fen) == tune.held_out(fen)
    # and the two sides are both reached, or the split would be no split
    assert len({tune.held_out(fen) for fen in fens}) == 2


def test_a_position_is_bucketed_by_what_is_left_on_the_board():
    """Neither pawns nor kings, both colours, which is what the game-corpus
    report stratified by."""
    assert (
        tune.phase_bucket("rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1")
        == "13+"
    )
    assert (
        tune.phase_bucket("r3k2r/pppppppp/8/8/8/8/PPPPPPPP/R3K2R w KQkq - 0 1") == "0-6"
    )
    assert (
        tune.phase_bucket("rn2k1nr/pppppppp/8/8/8/8/PPPPPPPP/RN2K1NR w KQkq - 0 1")
        == "7-12"
    )


def corpus_of(rows, vector, labels):
    _, parsed = tune.parse_terms(extraction(rows, vector))
    return tune.Corpus(vector, parsed, labels)


def test_the_integer_score_is_the_evaluation_the_engine_gave():
    """The harness scores a vector two ways: real valued for the fit, and
    integer with the truncation put back for the measurement that has to match
    the engine. At the shipped weights the second is the row's own column."""
    vector = weights({0: -30, 64: 17, 400: 5})
    rows = [
        row("a", [(0, 7), (64, -13), (tune.MATERIAL_SLOT, 1)], vector, 7),
        row("b", [(0, -11), (400, 3), (tune.MATERIAL_SLOT + 3, -2)], vector, 11),
    ]
    corpus = corpus_of(rows, vector, {"a": (1.0, 1), "b": (0.0, 2)})
    assert list(corpus.integer_scores(np.array(vector))) == list(corpus.evals)
    # and the real valued one is within the centipawn the truncation costs
    assert np.all(
        np.abs(corpus.scores(np.array(vector, dtype=float)) - corpus.evals) < 1.0
    )


def test_a_slots_support_is_how_many_rows_it_appears_in():
    """A weight the corpus barely constrains says so before it ships."""
    vector = weights({0: 5})
    rows = [
        row("a", [(0, 24), (tune.MATERIAL_SLOT, 1)], vector),
        row("b", [(tune.MATERIAL_SLOT, 1)], vector),
    ]
    corpus = corpus_of(rows, vector, {"a": (1.0, 1), "b": (0.5, 1)})
    support = corpus.support()
    assert support[0] == 1
    assert support[tune.MATERIAL_SLOT] == 2
    assert support[1] == 0


def test_the_scaling_constant_is_found_where_it_was_put():
    """K is fitted once by a search over the training split, so a corpus
    generated at a known K has to hand it back."""
    generator = np.random.default_rng(11)
    scores = generator.normal(0.0, 200.0, 20000)
    truth = 1.4
    results = (generator.random(20000) < tune.sigmoid(scores, truth)).astype(float)
    counts = np.ones_like(results)
    assert tune.fit_k(scores, results, counts) == pytest.approx(truth, abs=0.1)


def test_a_difference_is_never_printed_without_its_interval():
    """Two vectors are scored on the same positions, so the difference is a
    paired sample and its mean has a standard error. A difference whose
    interval covers zero is not a difference."""
    first = np.array([0.10, 0.20, 0.30, 0.40])
    counts = np.ones(4)
    mean, error = tune.paired_difference(first, first, counts)
    assert mean == 0.0 and error == 0.0
    mean, error = tune.paired_difference(first, first - 0.05, counts)
    assert mean == pytest.approx(-0.05)
    assert error == pytest.approx(0.0, abs=1e-12)
    # a difference that varies from position to position carries an interval
    mean, error = tune.paired_difference(
        first, first + np.array([0.1, -0.1, 0.1, -0.1]), counts
    )
    assert mean == pytest.approx(0.0)
    assert error > 0.0


def test_the_optimiser_finds_the_bottom_of_a_bowl():
    """L-BFGS on the closed-form gradient, checked against a problem whose
    answer is known. The step it has to take on the real loss is many times
    longer than one, so a search that only backtracked would stall, and the
    bowl below is scaled to say that."""

    centre = np.arange(tune.SLOTS, dtype=float)

    def objective(x):
        slack = (x - centre) * 1e-4
        return float(slack @ slack), 2e-8 * (x - centre)

    found, value = tune.lbfgs(objective, np.zeros(tune.SLOTS))
    assert value < 1e-12
    assert np.max(np.abs(found - centre)) < 1e-3


def test_a_vector_the_engine_could_not_carry_is_refused():
    """Each half of a packed pair is an `i16` and a boardful of them is summed
    into one, so a fit that grew the tables past that is no candidate whatever
    it scores."""
    inside, worst = tune.bounds_hold(np.array(weights()))
    assert inside and worst == 0
    huge = np.array(weights({slot: 400 for slot in range(tune.MATERIAL_SLOT)}))
    inside, worst = tune.bounds_hold(huge)
    assert not inside
    assert worst == 2 * 64 * 400


def test_quantizing_rounds_to_nearest():
    assert list(tune.quantize([1.4, 1.6, -1.4, -1.6, 2.5])) == [1, 2, -1, -2, 2]


def fixture_run(tmp_path, vector, rows, labels):
    terms = tmp_path / "rows.txt"
    terms.write_text("\n".join(extraction(rows, vector)) + "\n", encoding="utf-8")
    corpus = tmp_path / "corpus.epd"
    corpus.write_text(
        "\n".join(
            f'4k3/8/8/8/8/8/8/4K3 w - - id "{name}"; result "{result}"; count "{count}";'
            for name, (result, count) in labels.items()
        )
        + "\n",
        encoding="utf-8",
    )
    return terms, corpus


def test_a_loss_run_reports_both_sides_of_the_split(tmp_path, capsys):
    """The header names the corpus, the split and the result distribution
    before any loss, so a number is never read without knowing what it is a
    number over."""
    vector = weights({0: 20, 64: -30})
    rows = []
    labels = {}
    for index in range(40):
        name = f"p{index:03d}"
        fen = f"4k3/8/8/8/8/8/{index}p/4K3 w - - 0 1"
        rows.append(
            row(name, [(0, 24), (tune.MATERIAL_SLOT, index % 3 - 1)], vector, 24, fen)
        )
        labels[name] = (float(index % 2), 1)
    terms, corpus = fixture_run(tmp_path, vector, rows, labels)
    assert tune.main(["loss", "--terms", str(terms), "--corpus", str(corpus)]) == 0
    printed = capsys.readouterr().out
    assert "corpus positions 40 train" in printed
    assert "shipped holdout mse" in printed
    assert "support least" in printed


def test_a_candidate_is_scored_against_the_shipped_weights(tmp_path, capsys):
    """A candidate vector is read from json and reported beside the shipped
    one, with the paired difference and its interval and never a bare
    delta."""
    vector = weights({0: 20})
    rows = []
    labels = {}
    for index in range(40):
        name = f"p{index:03d}"
        fen = f"4k3/8/8/8/8/8/{index}p/4K3 w - - 0 1"
        rows.append(row(name, [(0, 24), (tune.MATERIAL_SLOT, 1)], vector, 24, fen))
        labels[name] = (float(index % 2), 1)
    terms, corpus = fixture_run(tmp_path, vector, rows, labels)
    candidate = tmp_path / "candidate.json"
    candidate.write_text(json.dumps(weights({0: 25})), encoding="utf-8")
    assert (
        tune.main(
            [
                "loss",
                "--terms",
                str(terms),
                "--corpus",
                str(corpus),
                "--weights",
                str(candidate),
            ]
        )
        == 0
    )
    printed = capsys.readouterr().out
    assert "candidate against shipped holdout mse" in printed
    assert " se " in printed


def test_a_fit_holds_the_material_values_unless_it_is_told_not_to(tmp_path, capsys):
    """`eval::material` is read by the delta margin in quiescence, so moving
    it changes which captures quiescence skips, which changes the tree for a
    reason that has nothing to do with the evaluation's accuracy."""
    vector = weights({0: 20})
    rows = []
    labels = {}
    for index in range(60):
        name = f"p{index:03d}"
        fen = f"4k3/8/8/8/8/8/{index}p/4K3 w - - 0 1"
        rows.append(
            row(
                name,
                [(0, 24), (tune.MATERIAL_SLOT, 1 if index % 2 else -1)],
                vector,
                24,
                fen,
            )
        )
        labels[name] = (1.0 if index % 2 else 0.0, 1)
    terms, corpus = fixture_run(tmp_path, vector, rows, labels)
    out = tmp_path / "fit.json"
    assert (
        tune.main(
            [
                "fit",
                "--terms",
                str(terms),
                "--corpus",
                str(corpus),
                "--out",
                str(out),
                "--penalties",
                "1e-6",
            ]
        )
        == 0
    )
    fitted = json.loads(out.read_text(encoding="utf-8"))
    assert fitted[tune.MATERIAL_SLOT :] == MATERIAL
    assert len(fitted) == tune.SLOTS
