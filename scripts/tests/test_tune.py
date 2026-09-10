# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2022-2026 Andrew Wright

"""Tests for the tuner and its loss harness.

The seam is what most of these are about. The engine states a position's
coefficients and states the weights, and this file's job is to fold the two
back together and get the integer the engine got. Three details of that
arithmetic are the ones a python reader gets wrong, and each has a test that
says so by naming a case where getting it wrong gives a different answer.

The rest pin the formats the run parses, which is the stated reason the other
script tests exist, and the claims the harness makes about its own numbers:
that a difference is never printed without its interval, that a weight vector
the engine's arithmetic cannot carry is refused, that the objective weights a
position by how often the corpus reached it, and that the calibration group is
not read by anything here.
"""

import json
import math

import groups
import numpy as np
import pytest
import tune

# A pawn, a knight, a bishop, a rook, a queen and a king, which is what the
# material slots hold today.
MATERIAL = [100, 310, 320, 500, 900, 10000]

# The five slices of a game key, by the group each falls in.
TRAIN, SELECTION, CALIBRATION = 0, 3, 4


def key(index, slice_):
    """A game key in the shape `build_corpus.py` writes: sixty-four hex
    characters, the first byte of which says which group the game is in and the
    second which fold it falls in. Both are read off a real sha256 the same
    way."""
    return f"{slice_:02x}{index % 256:02x}{index:060x}"


def labels_of(mapping):
    """The labels a corpus file gives, as `parse_corpus` hands them over."""
    return {
        name: tune.Label(result, count, game)
        for name, (result, count, game) in mapping.items()
    }


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


def test_a_vector_of_the_layout_before_the_endgame_tables_is_refused(tmp_path):
    """518 is the one wrong length that would otherwise read as a right one:
    every slot it names exists in the layout that replaced it, so its numbers
    would land on the wrong weights rather than failing to parse. Both doors a
    vector comes through say what changed."""
    assert tune.SHARED_TABLE_SLOTS == 518
    assert tune.SLOTS == 774
    old = [0] * tune.SHARED_TABLE_SLOTS
    with pytest.raises(ValueError, match="given an endgame table"):
        tune.parse_terms(
            ["weights {} {}".format(len(old), " ".join(str(w) for w in old))]
        )
    written = tmp_path / "fitted.json"
    written.write_text(json.dumps(old), encoding="utf-8")
    with pytest.raises(ValueError, match="given an endgame table"):
        tune.read_weights(written)


def test_a_corpus_line_is_read_the_way_the_engine_reads_epd():
    """Four fields and then operations, so the id is not looked for among the
    words of the position."""
    line = (
        "r1bqk2r/p3bppp/2n1pn2/2pp4/Pp2P3/3P1NP1/1PPN1PBP/R1BQ1RK1 w kq - "
        f'id "g00001p016"; game "{key(1, TRAIN)}"; result "0.2500"; count "2";'
    )
    label = tune.parse_corpus([line])["g00001p016"]
    assert (label.result, label.count, label.game) == (0.25, 2, key(1, TRAIN))
    # a line with no label is not a row to fit
    assert tune.parse_corpus(['4k3/8/8/8/8/8/8/4K3 w - - id "solo";']) == {}


def test_a_corpus_that_names_no_game_is_refused():
    """The three groups are assigned from the game operand. A corpus built
    before it existed would be split into one game per position, which is the
    leak the game split closed arriving through the back door, so it is refused
    rather than read."""
    line = '4k3/8/8/8/8/8/8/4K3 w - - id "g00001p020"; result "1.0000"; count "1";'
    with pytest.raises(ValueError, match="names no game"):
        tune.parse_corpus([line])


def test_a_corpus_that_repeats_a_position_across_games_is_refused():
    """A position is one row, because the corpus is deduplicated by fen before
    it is labelled and takes the lowest key of the games that reached it, so no
    position is in two groups. A corpus that was not deduplicated could be, and
    would be the leak the fen split had in a new place."""
    vector = weights({0: 7})
    fen = "4k3/8/8/8/8/8/8/4K3 w - - 0 1"
    rows = [
        row("g00001p020", [(0, 24), (tune.MATERIAL_SLOT, 1)], vector, 24, fen),
        row("g00002p031", [(0, 24), (tune.MATERIAL_SLOT, 1)], vector, 24, fen),
    ]
    labels = {
        "g00001p020": (1.0, 1, key(1, TRAIN)),
        "g00002p031": (0.0, 1, key(2, SELECTION)),
    }
    with pytest.raises(ValueError, match="is in .* and in "):
        corpus_of(rows, vector, labels)


def games_and_labels(vector, games, plies=4):
    """A corpus of whole games, each of which is a slice of the key and a
    handful of positions that are its and no other game's."""
    rows = []
    labels = {}
    for index, slice_ in enumerate(games):
        for ply in range(plies):
            name = f"g{index:05d}p{ply:03d}"
            fen = f"4k3/8/8/8/8/{index}p/{ply}p/4K3 w - - 0 1"
            rows.append(row(name, [(0, 24), (tune.MATERIAL_SLOT, 1)], vector, 24, fen))
            labels[name] = (float(index % 2), 1, key(index, slice_))
    return rows, labels


def test_no_games_rows_straddle_the_groups():
    """The property the split is for, read off a corpus rather than off the
    key: no game has rows in two groups."""
    vector = weights({0: 20})
    rows, labels = games_and_labels(vector, [index % 5 for index in range(60)])
    corpus = corpus_of(rows, vector, labels)
    groups = {}
    for index in range(len(corpus)):
        groups.setdefault(corpus.games[index], set()).add(corpus.groups[index])
    assert all(len(group) == 1 for group in groups.values())
    assert {next(iter(group)) for group in groups.values()} == {"train", "selection"}
    # and the fifth that is neither is sealed rather than dropped
    assert corpus.sealed.games == 12
    assert corpus.sealed.positions == 48


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
    return tune.Corpus(vector, parsed, labels_of(labels))


def test_the_integer_score_is_the_evaluation_the_engine_gave():
    """The harness scores a vector two ways: real valued for the fit, and
    integer with the truncation put back for the measurement that has to match
    the engine. At the shipped weights the second is the row's own column."""
    vector = weights({0: -30, 64: 17, 400: 5})
    rows = [
        row("a", [(0, 7), (64, -13), (tune.MATERIAL_SLOT, 1)], vector, 7, "a w - -"),
        row(
            "b",
            [(0, -11), (400, 3), (tune.MATERIAL_SLOT + 3, -2)],
            vector,
            11,
            "b w - -",
        ),
    ]
    corpus = corpus_of(
        rows,
        vector,
        {"a": (1.0, 1, key(1, TRAIN)), "b": (0.0, 2, key(2, TRAIN))},
    )
    assert list(corpus.integer_scores(np.array(vector))) == list(corpus.evals)
    # and the real valued one is within the centipawn the truncation costs
    assert np.all(
        np.abs(corpus.scores(np.array(vector, dtype=float)) - corpus.evals) < 1.0
    )


def test_the_occurrence_count_weights_the_loss():
    """A unique position carries the weight of how many times the corpus
    reached it. The objective is the distribution the engine runs on rather
    than the one deduplication leaves behind, so a position two games reached
    pulls the loss towards its own result."""
    vector = weights({0: 20})
    rows = [
        row("a", [(0, 24), (tune.MATERIAL_SLOT, 1)], vector, 24, "a w - -"),
        row("b", [(0, 24), (tune.MATERIAL_SLOT, 1)], vector, 24, "b w - -"),
    ]
    # two positions scored alike and labelled oppositely, so the only thing a
    # weight can move is which of the two the loss listens to
    once = corpus_of(
        rows, vector, {"a": (1.0, 1, key(1, TRAIN)), "b": (0.0, 1, key(2, TRAIN))}
    )
    twice = corpus_of(
        rows, vector, {"a": (1.0, 3, key(1, TRAIN)), "b": (0.0, 1, key(2, TRAIN))}
    )
    assert int(once.counts.sum()) == 2
    assert int(twice.counts.sum()) == 4
    predicted = tune.sigmoid(120.0, 1.0)
    flat = tune.scored(once, vector, once.train, 1.0)["mse"]
    weighted = tune.scored(twice, vector, twice.train, 1.0)["mse"]
    assert flat == pytest.approx(((1 - predicted) ** 2 + predicted**2) / 2)
    assert weighted == pytest.approx((3 * (1 - predicted) ** 2 + predicted**2) / 4)
    # the row the evaluation is right about is the one weighted up, so three
    # appearances of it is a lower loss and not merely a different one
    assert weighted < flat


def test_a_slots_support_is_how_many_rows_it_appears_in():
    """A weight the corpus barely constrains says so before it ships."""
    vector = weights({0: 5})
    rows = [
        row("a", [(0, 24), (tune.MATERIAL_SLOT, 1)], vector, 24, "a w - -"),
        row("b", [(tune.MATERIAL_SLOT, 1)], vector, 24, "b w - -"),
    ]
    corpus = corpus_of(
        rows,
        vector,
        {"a": (1.0, 1, key(1, TRAIN)), "b": (0.5, 1, key(2, TRAIN))},
    )
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
    games = np.array(["a", "b", "c", "d"])
    mean, error, _, _ = tune.paired_difference(first, first, counts, games)
    assert mean == 0.0 and error == 0.0
    mean, error, _, _ = tune.paired_difference(first, first - 0.05, counts, games)
    assert mean == pytest.approx(-0.05)
    assert error == pytest.approx(0.0, abs=1e-12)
    # a difference that varies from position to position carries an interval
    mean, error, _, _ = tune.paired_difference(
        first, first + np.array([0.1, -0.1, 0.1, -0.1]), counts, games
    )
    assert mean == pytest.approx(0.0)
    assert error > 0.0


def test_the_interval_is_taken_over_the_games():
    """Positions inside one game share a label and are a move apart, so they
    move together, and counting them as independent draws counts one game's
    evidence as many. Two games of a hundred positions each, differing by game
    and not within one, are two draws and not two hundred."""
    counts = np.ones(200)
    games = np.array(["a"] * 100 + ["b"] * 100)
    first = np.zeros(200)
    second = np.concatenate([np.full(100, 0.02), np.full(100, -0.02)])
    mean, clustered, naive, design = tune.paired_difference(
        first, second, counts, games
    )
    assert mean == pytest.approx(0.0)
    # the whole spread is between the games, so the two-game interval is the
    # full half-swing and the per-position one is a fourteenth of it. both are
    # returned, because the spec asks for the naive figure printed beside the
    # honest one rather than only their ratio
    assert clustered == pytest.approx(0.02, rel=1e-6)
    assert naive == pytest.approx(0.02 / math.sqrt(200), rel=1e-6)
    assert design == pytest.approx(clustered / naive, rel=1e-6)
    # and where every row is a game of its own, which is the independence a
    # per-position interval assumes, the two agree but for the correction a
    # sample of two hundred carries
    alone = np.array([str(index) for index in range(200)])
    _, clustered, naive, design = tune.paired_difference(first, second, counts, alone)
    assert design == pytest.approx(math.sqrt(200 / 199), rel=1e-9)


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


def sample(vector, count=30, plies=4):
    """A corpus of whole games spread across the five slices of the key.

    Six games to a slice, so eighteen train, six choose the ridge and six are
    sealed. The group is the key's first byte and the fold is its second, and
    the two are moved independently here for the reason the run reads them
    apart: a fixture whose folds followed its groups would leave two folds
    empty.
    """
    rows, labels = [], {}
    for index in range(count):
        result = 1.0 if index % 2 else 0.0
        for ply in range(plies):
            name = f"g{index:05d}p{ply:03d}"
            fen = f"4k3/8/8/8/8/{index}p/{ply}p/4K3 w - - 0 1"
            rows.append(
                row(
                    name,
                    [(0, 24), (tune.MATERIAL_SLOT, 1 if index % 2 else -1)],
                    vector,
                    24,
                    fen,
                )
            )
            labels[name] = (result, 1, key(index, index * len(groups.SLICES) // count))
    return rows, labels


def fixture_run(tmp_path, vector, rows, labels, name="corpus.epd", drop=()):
    """The two files a run reads. `drop` names groups to leave out of the
    corpus file, which is how a test asks what the run would have printed had
    those rows never been extracted."""
    terms = tmp_path / "rows.txt"
    terms.write_text("\n".join(extraction(rows, vector)) + "\n", encoding="utf-8")
    corpus = tmp_path / name
    corpus.write_text(
        "\n".join(
            f'4k3/8/8/8/8/8/8/4K3 w - - id "{identifier}"; game "{game}"; '
            f'result "{result}"; count "{count}";'
            for identifier, (result, count, game) in labels.items()
            if groups.group_of(game) not in drop
        )
        + "\n",
        encoding="utf-8",
    )
    return terms, corpus


def test_a_loss_run_reports_the_groups_and_names_the_sealed_one(tmp_path, capsys):
    """The header names the corpus, the three groups and the result
    distribution before any loss, so a number is never read without knowing
    what it is a number over. The sealed group is named and not scored, which
    is the whole of what a run may say about it."""
    vector = weights({0: 20, 64: -30})
    rows, labels = sample(vector)
    terms, corpus = fixture_run(tmp_path, vector, rows, labels)
    assert tune.main(["loss", "--terms", str(terms), "--corpus", str(corpus)]) == 0
    printed = capsys.readouterr().out
    # eighteen games train and six choose the ridge, at four positions a game
    assert "corpus positions 96 train 72 selection 24" in printed
    # and the games beside the positions, because the games are what the split
    # and every interval are taken over
    assert "games 30 train 18 selection 6 calibration 6" in printed
    assert "calibration positions 24 appearances 24 sealed, not read here" in printed
    assert "shipped selection mse" in printed
    assert "support least" in printed


def test_a_candidate_is_scored_against_the_shipped_weights(tmp_path, capsys):
    """A candidate vector is read from json and reported beside the shipped
    one, with the paired difference and its interval and never a bare
    delta."""
    vector = weights({0: 20})
    rows, labels = sample(vector)
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
    assert "candidate against shipped selection mse" in printed
    assert " se " in printed
    # the naive per-position interval beside the honest one, and the ratio of
    # the two, so a reader can see what treating the positions as independent
    # would have claimed
    assert " per position " in printed
    assert " design " in printed


def test_a_fit_holds_the_material_values_unless_it_is_told_not_to(tmp_path, capsys):
    """`eval::material` is read by the delta margin in quiescence, so moving
    it changes which captures quiescence skips, which changes the tree for a
    reason that has nothing to do with the evaluation's accuracy."""
    vector = weights({0: 20})
    rows, labels = sample(vector)
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


def test_a_ridge_is_chosen_on_the_selection_games(tmp_path, capsys):
    """The grid is fitted on the training games and ranked on the selection
    games, which is what the third group frees the calibration games from
    having to do. Every penalty is printed with what it bought and what its
    interval was, and the chosen one is named."""
    vector = weights({0: 20})
    rows, labels = sample(vector)
    terms, corpus = fixture_run(tmp_path, vector, rows, labels)
    assert (
        tune.main(
            [
                "fit",
                "--terms",
                str(terms),
                "--corpus",
                str(corpus),
                "--penalties",
                "1e-6",
                "1e-4",
            ]
        )
        == 0
    )
    printed = capsys.readouterr().out
    assert "shipped selection mse" in printed
    assert "penalty 1e-06 selection mse" in printed
    assert "penalty 0.0001 selection mse" in printed
    assert "chose penalty" in printed


def test_a_cross_validation_folds_only_the_games_it_may_read(tmp_path, capsys):
    """Five folds, each fitted on four fifths of the games and scored on the
    fifth, so every row is scored by a fit that never read its game. The games
    it folds are the ones the run may read, and the sealed group is not among
    them: the corpus it is handed does not hold those rows."""
    vector = weights({0: 20})
    rows, labels = sample(vector, count=40)
    terms, corpus = fixture_run(tmp_path, vector, rows, labels)
    assert (
        tune.main(
            [
                "cv",
                "--terms",
                str(terms),
                "--corpus",
                str(corpus),
                "--penalties",
                "1e-6",
                "1e-4",
            ]
        )
        == 0
    )
    printed = capsys.readouterr().out
    for index in range(tune.FOLDS):
        assert f"fold {index} games " in printed
    assert "shipped cv mse" in printed
    assert "penalty 1e-06 cv mse" in printed
    assert "design " in printed
    # the line says which group it is best over, so it cannot be pasted into
    # `fit --penalties` as the ridge the fit would have chosen
    assert "best penalty over the folds" in printed
    assert "fit chooses on the selection group instead" in printed
    # thirty-two of the forty games are in the two groups the run may read, and
    # the folds hold those and no more
    folded = sum(
        int(line.split(" games ")[1].split(" ")[0])
        for line in printed.splitlines()
        if line.startswith("fold ")
    )
    assert folded == 32


def test_the_calibration_group_is_not_read_by_a_fit(tmp_path, capsys):
    """What "not read until the weights are final" means, rather than what it
    promises.

    The same fit is run twice, once over a corpus holding the calibration games
    and once over one those rows were cut out of, and it writes the same vector
    and prints the same numbers. The calibration games here are labelled the
    opposite way round to every other game, so a run that read one row of them
    could not come out the same.
    """
    vector = weights({0: 20})
    rows, labels = sample(vector)
    labels = {
        name: (
            1.0 - result if groups.group_of(game) == "calibration" else result,
            count,
            game,
        )
        for name, (result, count, game) in labels.items()
    }
    printed, written = [], []
    for index, drop in enumerate(((), ("calibration",))):
        terms, corpus = fixture_run(
            tmp_path, vector, rows, labels, name=f"corpus{index}.epd", drop=drop
        )
        out = tmp_path / f"fit{index}.json"
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
                    "1e-4",
                ]
            )
            == 0
        )
        written.append(out.read_text(encoding="utf-8"))
        printed.append(
            [
                line
                for line in capsys.readouterr().out.splitlines()
                # but for the two lines saying how big the sealed group is,
                # which is the one thing a run may say about it
                if "calibration" not in line
            ]
        )
    assert written[0] == written[1]
    assert printed[0] == printed[1]
