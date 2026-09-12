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

import hashlib
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
    """The labels a corpus file gives, as `parse_corpus` hands them over. A
    fixture's games have no partners, so each is its own pair."""
    return {
        name: tune.Label(result, count, game, game)
        for name, (result, count, game) in mapping.items()
    }


def weights(entries=None):
    """A weight vector in the engine's layout, tables at nothing unless the
    caller names an entry."""
    vector = [0] * tune.SLOTS
    vector[tune.MATERIAL_SLOT : tune.MOBILITY_SLOT] = MATERIAL
    for slot, value in (entries or {}).items():
        vector[slot] = value
    return vector


def row(
    identifier, coefficients, vector, phase=24, fen="4k3/8/8/8/8/8/8/4K3 w - - 0 1"
):
    """One row in the shape `arche terms` prints it, with the evaluation it
    states worked out from the weights the same way the engine does. The fen
    is six fields, which is what the row's reader counts back from."""
    evaluation = tune.reconstruct(coefficients, vector)
    terms = " ".join(f"{slot}:{coefficient}" for slot, coefficient in coefficients)
    return f"{identifier} {evaluation} {phase} {len(coefficients)} {terms} {fen}"


def extraction(rows, vector):
    header = (
        f"terms positions {len(rows)} in_check 0 unsettled 0 drawn 0 kept {len(rows)}"
    )
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


def test_a_row_reads_from_the_right_and_an_id_can_hold_spaces():
    """An id can hold a space, so the fields are found from the right rather
    than the left. Nine of the bench's eighteen positions are named that way
    and the bench is the suite `arche terms` reads by default, so reading the
    id as the first word left the rest of a name to be read as the evaluation.

    The three rows are the shapes the reading has to survive: a name with a
    space in it, a line that named no id and is called by its own fen, and a
    row with no coefficients, where the walk back from the fen has nothing to
    walk over."""
    vector = weights({5: 12, 400: -7})
    fen = "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1"
    coefficients = [(5, 24), (400, -3), (tune.MATERIAL_SLOT + 4, -1)]
    lines = [
        row("ruy lopez", coefficients, vector, 18, fen),
        row(fen, coefficients, vector, 18, fen),
        row("start", [], vector, 24, fen),
    ]
    _, rows = tune.parse_terms(extraction(lines, vector))
    assert [parsed.id for parsed in rows] == ["ruy lopez", fen, "start"]
    assert [parsed.fen for parsed in rows] == [fen, fen, fen]
    assert rows[0].phase == 18
    assert rows[0].coefficients == coefficients
    assert rows[2].coefficients == []


def test_a_weights_line_of_the_wrong_length_is_refused():
    """The layout is a contract between two files, and a vector of another
    length means one of them has changed and the other has not."""
    with pytest.raises(ValueError, match="weights line"):
        tune.parse_terms(["weights 3 1 2 3"])
    with pytest.raises(ValueError, match="no weights line"):
        tune.parse_terms(["terms positions 0 in_check 0 unsettled 0 drawn 0 kept 0"])


def test_a_header_without_the_drawn_count_is_refused():
    """The header is a version check. An extraction printed by an engine with
    no drawn material rule would parse, rebuild row for row and fit an
    evaluation that engine no longer runs, because the rows it should have
    turned away are in it and score zero from no weight vector at all.

    This file cannot find those rows itself. Doing so would be a second copy
    of the rule living where the seam forbids one, and it would go on being
    right only for as long as nobody edited either copy. So the count is what
    is read, and a header without it refuses the whole run."""
    vector = weights()
    lines = extraction([row("a", [(tune.MATERIAL_SLOT, 1)], vector)], vector)
    _, rows = tune.parse_terms(lines)
    assert len(rows) == 1
    for header in [
        "terms positions 1 in_check 0 unsettled 0 kept 1",
        "terms epd corpus.epd positions 1 in_check 0 unsettled 0 kept 1",
    ]:
        with pytest.raises(ValueError, match="extraction is to redo"):
            tune.parse_terms([header, *lines[1:]])


@pytest.mark.parametrize(
    ("count", "message"),
    [
        (518, "given an endgame table"),
        (774, "before mobility"),
        (782, "before the king's shelter"),
        (790, "before the pawn storm"),
        (796, "before the pawn structure"),
    ],
)
def test_a_vector_of_an_earlier_layout_is_refused(tmp_path, count, message):
    """518, 774, 782, 790 and 796 are the wrong lengths that would otherwise
    read as right ones: every slot any of them names exists in the layout that
    replaced it, so their numbers would land on the wrong weights rather than
    failing to parse. Both doors a vector comes through say what changed."""
    assert tune.SHARED_TABLE_SLOTS == 518
    assert tune.NO_MOBILITY_SLOTS == 774
    assert tune.NO_SHELTER_SLOTS == 782
    assert tune.NO_STORM_SLOTS == 790
    assert tune.NO_PAWN_SLOTS == 796
    assert tune.SLOTS == 812
    old = [0] * count
    with pytest.raises(ValueError, match=message):
        tune.parse_terms(
            ["weights {} {}".format(len(old), " ".join(str(w) for w in old))]
        )
    written = tmp_path / "fitted.json"
    written.write_text(json.dumps(old), encoding="utf-8")
    with pytest.raises(ValueError, match=message):
        tune.read_weights(written)


def test_a_row_whose_id_opens_with_the_header_word_is_kept():
    """The header is skipped on the two shapes the engine writes it in, and
    not on its first word. An id is whatever the epd put in the quotes, so a
    name can open with the same word, and a row dropped for looking like a
    header would leave the corpus a position short with nothing said about
    it."""
    vector = weights()
    lines = extraction(
        [row("terms of the endgame", [(tune.MATERIAL_SLOT, 1)], vector)], vector
    )
    _, rows = tune.parse_terms(lines)
    assert [parsed.id for parsed in rows] == ["terms of the endgame"]
    # and both shapes of the header are still skipped rather than read as rows
    header = "terms epd corpus.epd positions 1 in_check 0 unsettled 0 drawn 0 kept 1"
    _, rows = tune.parse_terms([header, *lines])
    assert [parsed.id for parsed in rows] == ["terms of the endgame"]


def test_a_corpus_line_is_read_the_way_the_engine_reads_epd():
    """Four fields and then operations, so the id is not looked for among the
    words of the position."""
    line = (
        "r1bqk2r/p3bppp/2n1pn2/2pp4/Pp2P3/3P1NP1/1PPN1PBP/R1BQ1RK1 w kq - "
        f'id "g00001p016"; game "{key(1, TRAIN)}"; pair "{key(3, SELECTION)}"; '
        'result "0.2500"; count "2";'
    )
    label = tune.parse_corpus([line])["g00001p016"]
    assert (label.result, label.count, label.game, label.pair) == (
        0.25,
        2,
        key(1, TRAIN),
        key(3, SELECTION),
    )
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


def test_a_corpus_that_names_no_pair_is_refused():
    """The pair is what the split reads, and a corpus from before pairs were
    keyed would be split by the game, which holds half an opening out."""
    line = (
        '4k3/8/8/8/8/8/8/4K3 w - - id "g00001p020"; '
        f'game "{key(1, TRAIN)}"; result "1.0000"; count "1";'
    )
    with pytest.raises(ValueError, match="names no pair"):
        tune.parse_corpus([line])


def test_the_groups_are_assigned_from_the_pair_and_not_the_game():
    """A game whose own key would train but whose pair's key is sealed is
    sealed, because the pair is the unit the split holds out."""
    vector = weights({0: 7})
    rows = [
        row("g00001p020", [(0, 24)], vector, 24, "4k3/8/8/8/8/8/8/4K3 w - - 0 1"),
        row("g00002p020", [(0, 24)], vector, 24, "4k3/8/8/8/8/8/8/3K4 w - - 0 1"),
    ]
    labels = {
        "g00001p020": tune.Label(1.0, 1, key(1, TRAIN), key(9, CALIBRATION)),
        "g00002p020": tune.Label(0.0, 1, key(2, CALIBRATION), key(8, TRAIN)),
    }
    shipped, parsed = tune.parse_terms(extraction(rows, vector))
    corpus = tune.Corpus(shipped, parsed, labels)
    assert [r.id for r in corpus.rows] == ["g00002p020"]
    assert corpus.sealed.positions == 1
    assert corpus.sealed.pairs == 1


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
        row(
            "a", [(0, 7), (64, -13), (tune.MATERIAL_SLOT, 1)], vector, 7, "a w - - 0 1"
        ),
        row(
            "b",
            [(0, -11), (400, 3), (tune.MATERIAL_SLOT + 3, -2)],
            vector,
            11,
            "b w - - 0 1",
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
        row("a", [(0, 24), (tune.MATERIAL_SLOT, 1)], vector, 24, "a w - - 0 1"),
        row("b", [(0, 24), (tune.MATERIAL_SLOT, 1)], vector, 24, "b w - - 0 1"),
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
        row("a", [(0, 24), (tune.MATERIAL_SLOT, 1)], vector, 24, "a w - - 0 1"),
        row("b", [(tune.MATERIAL_SLOT, 1)], vector, 24, "b w - - 0 1"),
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


def test_the_mobility_weights_are_priced_too():
    """The figure reads as the whole vector, so a mobility weight left out of
    it would be a vector priced at 774 of its 790 slots. The range stops at the
    shelter block, which the test below prices on its own."""
    one_each = weights(
        {slot: 1 for slot in range(tune.MOBILITY_SLOT, tune.SHELTER_SLOT)}
    )
    _, worst = tune.bounds_hold(np.array(one_each))
    assert worst == 2 * int(tune.MAX_COUNT.sum())
    huge = weights(
        {slot: 5000 for slot in range(tune.MOBILITY_SLOT, tune.SHELTER_SLOT)}
    )
    assert not tune.bounds_hold(np.array(huge))[0]


def test_the_shelter_weights_are_priced_too():
    """The same for the block after it. Three of each count a side, both
    colours, at the larger of the two halves, which here is the endgame one at
    ten a count."""
    vector = weights(
        {tune.SHELTER_SLOT + index: 5 for index in range(tune.SHELTER_SLOTS)}
        | {
            tune.SHELTER_SLOT + tune.SHELTER_SLOTS + index: 10
            for index in range(tune.SHELTER_SLOTS)
        }
    )
    inside, worst = tune.bounds_hold(np.array(vector))
    assert inside
    assert worst == 2 * tune.MAX_SHELTER * tune.SHELTER_SLOTS * 10
    huge = weights({slot: 5000 for slot in range(tune.SHELTER_SLOT, tune.PAWN_SLOT)})
    assert not tune.bounds_hold(np.array(huge))[0]


def test_the_pawn_structure_weights_are_priced_too():
    """And the block after that one. Eight of each count a side, both colours,
    at the larger of the two halves, which here is the endgame one at four a
    count."""
    vector = weights(
        {tune.PAWN_SLOT + index: 3 for index in range(tune.PAWN_SLOTS)}
        | {
            tune.PAWN_SLOT + tune.PAWN_SLOTS + index: 4
            for index in range(tune.PAWN_SLOTS)
        }
    )
    inside, worst = tune.bounds_hold(np.array(vector))
    assert inside
    assert worst == 2 * tune.MAX_PAWNS * tune.PAWN_SLOTS * 4
    huge = weights({slot: 5000 for slot in range(tune.PAWN_SLOT, tune.SLOTS)})
    assert not tune.bounds_hold(np.array(huge))[0]


def test_the_material_block_is_the_only_thing_outside_the_divide():
    """`is_material` is what puts a weight outside the taper's divide, and
    mobility and the shelter go inside it the way the tables do. Outside it
    either would answer a centipawn away from the engine wherever a numerator
    is negative and does not divide evenly, and `reconstruct` would stop
    matching `eval` the moment a weight was fitted.

    The last two lines are what the slot arithmetic above cannot say. A
    shelter weight is the fit's now and a coefficient sorted into the material
    half rebuilds to a different number, but six of the eight mobility weights
    still ship at zero and theirs would not. The weights here are the
    fixture's for that reason, so the two answers differ whatever a fit
    holds."""
    material = [slot for slot in range(tune.SLOTS) if tune.is_material(slot)]
    assert material == list(range(tune.MATERIAL_SLOT, tune.MOBILITY_SLOT))
    assert not any(
        tune.is_material(slot) for slot in range(tune.MOBILITY_SLOT, tune.SLOTS)
    )
    vector = weights({tune.SHELTER_SLOT: 30})
    vector[tune.MATERIAL_SLOT] = 100
    # a pawn outside the divide, and a shelter count of a full phase inside it
    coefficients = [(tune.SHELTER_SLOT, 24), (tune.MATERIAL_SLOT, 1)]
    assert tune.reconstruct(coefficients, vector) == 130


def test_a_fit_is_free_to_move_the_leaf_terms_weights():
    """Material is held for a first fit and nothing after it is. Frozen at the
    material block's end instead, which is what it was before mobility, the
    twenty two weights of the two leaf terms would sit at zero through the fit
    and the arm would report a null result with nothing saying why."""
    frozen = tune.frozen_slots(False)
    assert frozen[tune.MATERIAL_SLOT : tune.MOBILITY_SLOT].all()
    assert not frozen[tune.MOBILITY_SLOT :].any()
    assert not frozen[: tune.MATERIAL_SLOT].any()
    assert not tune.frozen_slots(True).any()


def test_a_term_is_fitted_with_every_earlier_term_held():
    """The holds leave one term free, which is what lets a match read the
    change as that term. Holding the tables alone leaves mobility free, so a
    shelter fit that passed only that would have refitted mobility beside the
    shelter and called the pair king safety. The same again one term on: two
    holds leave the shelter free, and a pawn structure fit wants three."""
    tables = tune.frozen_slots(False, True)
    assert not tables[tune.MOBILITY_SLOT : tune.SHELTER_SLOT].any()
    both = tune.frozen_slots(False, True, True)
    assert both[: tune.SHELTER_SLOT].all()
    assert not both[tune.SHELTER_SLOT : tune.PAWN_SLOT].any()
    three = tune.frozen_slots(False, True, True, True)
    assert three[: tune.PAWN_SLOT].all()
    assert not three[tune.PAWN_SLOT :].any()


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
            f'pair "{game}"; result "{result}"; count "{count}";'
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
    assert fitted[tune.MATERIAL_SLOT : tune.MOBILITY_SLOT] == MATERIAL
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


def test_final_opens_the_sealed_group_once_and_logs_it_first(tmp_path, capsys):
    """A frozen integer vector is scored on the sealed group against the
    shipped one, the log names the corpus and the sealed games by checksum
    before any row is read, and the same sealed games are refused a second
    time whatever file they arrive in. A corpus whose sealed games differ
    opens."""
    vector = weights({0: 20, 64: -30})
    rows, labels = sample(vector)
    terms, corpus = fixture_run(tmp_path, vector, rows, labels)
    frozen = tmp_path / "frozen.json"
    frozen.write_text(json.dumps(weights({0: 24, 64: -30})), encoding="utf-8")
    log = tmp_path / "final.log"
    run = ["final", "--terms", str(terms), "--corpus", str(corpus)]
    assert tune.main([*run, "--weights", str(frozen), "--log", str(log)]) == 0
    printed = capsys.readouterr().out
    # six sealed games at four positions each, and nothing else
    assert "calibration positions 24 games 6 pairs 6 appearances 24 opened" in printed
    assert "shipped calibration mse" in printed
    assert "frozen against shipped calibration quantized_mse" in printed
    assert "frozen residual signed p50" in printed
    assert "not a coverage claim" in printed
    lines = log.read_text(encoding="utf-8").splitlines()
    assert len(lines) == 1
    assert lines[0].startswith("opened ")
    assert f"corpus {tune.sha256_of(corpus)}" in lines[0]
    assert f"weights {tune.sha256_of(frozen)}" in lines[0]
    assert "positions 24 games 6" in lines[0]
    sealed_line = lines[0]
    # the same rows under another name are the same corpus
    renamed = tmp_path / "renamed.epd"
    renamed.write_bytes(corpus.read_bytes())
    with pytest.raises(SystemExit, match="opened before"):
        tune.main(
            [
                "final",
                "--terms",
                str(terms),
                "--corpus",
                str(renamed),
                "--weights",
                str(frozen),
                "--log",
                str(log),
            ]
        )
    assert len(log.read_text(encoding="utf-8").splitlines()) == 1
    # and so are the same sealed games in a corpus that grew elsewhere: a
    # training row's count moved, the file's checksum with it, and the
    # sealed games are the ones the log names
    grown = tmp_path / "grown.epd"
    grown.write_text(
        corpus.read_text(encoding="utf-8").replace('count "1"', 'count "2"', 1),
        encoding="utf-8",
    )
    assert tune.sha256_of(grown) != tune.sha256_of(corpus)
    with pytest.raises(SystemExit, match="opened before"):
        tune.main(
            [
                "final",
                "--terms",
                str(terms),
                "--corpus",
                str(grown),
                "--weights",
                str(frozen),
                "--log",
                str(log),
            ]
        )
    assert log.read_text(encoding="utf-8").splitlines() == [sealed_line]
    # a corpus whose sealed games differ opens: one sealed game left out
    sealed_game = next(
        game
        for (_, _, game) in labels.values()
        if groups.group_of(game) == "calibration"
    )
    other = tmp_path / "other.epd"
    other.write_text(
        "".join(
            line
            for line in corpus.read_text(encoding="utf-8").splitlines(keepends=True)
            if sealed_game not in line
        ),
        encoding="utf-8",
    )
    assert (
        tune.main(
            [
                "final",
                "--terms",
                str(terms),
                "--corpus",
                str(other),
                "--weights",
                str(frozen),
                "--log",
                str(log),
            ]
        )
        == 0
    )
    assert len(log.read_text(encoding="utf-8").splitlines()) == 2


def test_final_scores_the_sealed_rows_and_only_those(tmp_path, capsys):
    """The number printed is the loss over the sealed rows, worked out here
    from the same rows by hand, so the command is reading the group and not
    the corpus it was handed."""
    vector = weights({0: 20})
    rows, labels = sample(vector)
    terms, corpus = fixture_run(tmp_path, vector, rows, labels)
    frozen = tmp_path / "frozen.json"
    frozen.write_text(json.dumps(vector), encoding="utf-8")
    log = tmp_path / "final.log"
    assert (
        tune.main(
            [
                "final",
                "--terms",
                str(terms),
                "--corpus",
                str(corpus),
                "--weights",
                str(frozen),
                "--log",
                str(log),
                "--k",
                "1.0",
            ]
        )
        == 0
    )
    printed = capsys.readouterr().out
    # the sealed rows, scored the way the fixture's rows state their own
    # evaluation: the table entry at full phase and a pawn either way
    sealed = [
        (index, result)
        for index, (result, _, game) in enumerate(labels.values())
        if groups.group_of(game) == "calibration"
    ]
    scores = np.array(
        [
            tune.reconstruct(
                [(0, 24), (tune.MATERIAL_SLOT, 1 if result == 1.0 else -1)], vector
            )
            for _, result in sealed
        ],
        dtype=float,
    )
    results = np.array([result for _, result in sealed])
    counts = np.ones(len(sealed))
    expected = tune.mean_squared_error(scores, results, counts, 1.0)
    assert f"shipped calibration mse {expected:.6f}" in printed
    assert f"frozen calibration mse {expected:.6f}" in printed


def test_final_refuses_a_vector_that_is_not_integers(tmp_path):
    """The vector that ships is integers, and a reading of the sealed group
    against anything else is a reading of a vector that will not ship."""
    vector = weights({0: 20})
    rows, labels = sample(vector)
    terms, corpus = fixture_run(tmp_path, vector, rows, labels)
    unrounded = tmp_path / "unrounded.json"
    unrounded.write_text(json.dumps(weights({0: 20.5})), encoding="utf-8")
    log = tmp_path / "final.log"
    with pytest.raises(SystemExit, match="not the integers"):
        tune.main(
            [
                "final",
                "--terms",
                str(terms),
                "--corpus",
                str(corpus),
                "--weights",
                str(unrounded),
                "--log",
                str(log),
            ]
        )
    assert not log.exists()


def paired(labels):
    """The fixture's games keyed two to a pair, consecutive games together, so
    a draw by pair and a draw by game can be told apart."""
    keys = list(dict.fromkeys(game for (_, _, game) in labels.values()))
    partner = {}
    for index in range(0, len(keys) - 1, 2):
        partner[keys[index]] = keys[index]
        partner[keys[index + 1]] = keys[index]
    return {
        name: (result, count, game, partner.get(game, game))
        for name, (result, count, game) in labels.items()
    }


def fixture_run_paired(tmp_path, vector, rows, labels, name="corpus.epd"):
    terms = tmp_path / "rows.txt"
    terms.write_text("\n".join(extraction(rows, vector)) + "\n", encoding="utf-8")
    corpus = tmp_path / name
    corpus.write_text(
        "\n".join(
            f'4k3/8/8/8/8/8/8/4K3 w - - id "{identifier}"; game "{game}"; '
            f'pair "{pair}"; result "{result}"; count "{count}";'
            for identifier, (result, count, game, pair) in labels.items()
        )
        + "\n",
        encoding="utf-8",
    )
    return terms, corpus


def test_the_learning_curve_draws_pairs_and_holds_the_selection_group(tmp_path, capsys):
    """A draw takes whole pairs, the draws below the whole differ from each
    other, every fit is read on the selection group `fit` reads, the whole is
    fitted once, and the same seed draws the same curve."""
    vector = weights({0: 20, 64: -30})
    rows, labels = sample(vector, count=40)
    labels = paired(labels)
    terms, corpus = fixture_run_paired(tmp_path, vector, rows, labels)
    out = tmp_path / "curve.json"
    run = [
        "curve",
        "--terms",
        str(terms),
        "--corpus",
        str(corpus),
        "--shares",
        "0.5",
        "1.0",
        "--draws",
        "3",
        "--penalties",
        "1e-6",
        "--iterations",
        "20",
        "--k",
        "1.0",
        "--out",
        str(out),
    ]
    assert tune.main(run) == 0
    capsys.readouterr()
    written = json.loads(out.read_text(encoding="utf-8"))
    fits = written["fits"]
    # three draws at a half and one at the whole
    assert [fit["share"] for fit in fits] == [0.5, 0.5, 0.5, 1.0]
    # the training games are keyed two to a pair, and a draw takes both
    # games of a pair or neither: eight positions a pair in this fixture
    train_pairs = {
        pair
        for (_, _, game, pair) in labels.values()
        if groups.group_of(pair) == "train"
    }
    whole = fits[-1]
    assert whole["pairs"] == len(train_pairs)
    assert set(whole["chosen"]) == train_pairs
    for fit in fits[:-1]:
        assert fit["pairs"] == len(train_pairs) // 2
        assert set(fit["chosen"]) < train_pairs
        assert fit["games"] == 2 * fit["pairs"]
        assert fit["positions"] == 8 * fit["pairs"]
    # the draws differ from each other
    assert len({tuple(fit["chosen"]) for fit in fits[:-1]}) == 3
    # the whole share is the fit `fit` makes, read on the same selection group
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
                "--iterations",
                "20",
                "--k",
                "1.0",
            ]
        )
        == 0
    )
    fitted = next(
        line
        for line in capsys.readouterr().out.splitlines()
        if line.startswith("fitted selection mse ")
    )
    assert f"fitted selection mse {whole['selection_mse']:.6f}" in fitted
    assert written["summary"][0]["draws"] == 3
    assert written["summary"][1]["draws"] == 1
    # and the same seed draws the same curve
    assert tune.main(run) == 0
    capsys.readouterr()
    assert json.loads(out.read_text(encoding="utf-8")) == written


def test_the_learning_curve_refuses_a_share_it_cannot_draw(tmp_path):
    vector = weights({0: 20})
    rows, labels = sample(vector)
    terms, corpus = fixture_run(tmp_path, vector, rows, labels)
    for shares in (["0"], ["1.5"], ["-0.5", "1"]):
        with pytest.raises(SystemExit, match="in \\(0, 1\\]"):
            tune.main(
                ["curve", "--terms", str(terms), "--corpus", str(corpus), "--shares"]
                + shares
            )
    with pytest.raises(SystemExit, match="at least one draw"):
        tune.main(
            ["curve", "--terms", str(terms), "--corpus", str(corpus), "--draws", "0"]
        )
    # shares given twice or out of order are one sorted set
    assert tune.curve_shares([0.5, 0.25, 0.5], 1) == [0.25, 0.5]


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


def sealed_checksum(pairs):
    """What `Sealed.checksum` answers for a set of pairs, which is how `final`
    names the group it opened."""
    return hashlib.sha256("\n".join(sorted(pairs)).encode("utf-8")).hexdigest()


def test_a_named_seal_holds_out_the_games_it_names_and_no_others():
    """The corpus splits on the file rather than on the key when it is given
    one. Read through the checksum `final` logs, so what this asserts is the
    identity of the group and not its size: the pairs named are sealed, no
    other pair is, and the same corpus read without the file seals a different
    set."""
    vector = weights()
    rows, raw = sample(vector)
    _, parsed = tune.parse_terms(extraction(rows, vector))
    labels = labels_of(raw)
    named = {min(label.pair for label in labels.values())}
    corpus = tune.Corpus(vector, parsed, labels, named)
    assert corpus.sealed.checksum() == sealed_checksum(named)
    assert named.isdisjoint(corpus.pairs)
    assert set(corpus.groups) == {"train", "selection"}
    drawn = tune.Corpus(vector, parsed, labels)
    assert drawn.sealed.checksum() != sealed_checksum(named)


def test_a_sealed_pair_the_extraction_never_reached_is_refused():
    """A named pair with no row is a game the seal was drawn from that this
    corpus does not hold, so the group is smaller than the file says it is.
    The reading would then be over games a reader cannot name, which is worse
    than no reading at all."""
    vector = weights()
    rows, raw = sample(vector)
    _, parsed = tune.parse_terms(extraction(rows, vector))
    with pytest.raises(ValueError, match="reach no row"):
        tune.Corpus(vector, parsed, labels_of(raw), {"f" * 64})
