# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2022-2026 Andrew Wright

"""Tests for the tuner and its loss harness.

Most are about the seam: the engine states a position's coefficients and the
weights, and this file has to fold the two together and get the integer the
engine got. The three details of that arithmetic a python reader gets wrong
each have a test naming a case where getting it wrong gives a different
answer. The rest pin the formats the run parses and the claims the harness
makes about its own numbers.
"""

import hashlib
import json
import math

import groups
import numpy as np
import pytest
import tune

# The layout line an `arche terms` run prints today. Written out rather than
# asked of tune.py, which would agree with itself whatever it said.
LAYOUT_LINE = (
    "layout midgame 384 endgame 384 material 6 mobility 4 shelter 7 "
    "pawn_structure 8 king_attack 4"
)
LAYOUT = tune.Layout.of(LAYOUT_LINE)

# A pawn, a knight, a bishop, a rook, a queen and a king, which is what the
# material slots hold today.
MATERIAL = [100, 310, 320, 500, 900, 10000]

# The five slices of a game key, by the group each falls in.
TRAIN, SELECTION, CALIBRATION = 0, 3, 4


def key(index, slice_):
    """A game key in the shape `build_corpus.py` writes: sixty-four hex
    characters, the first byte saying the group and the second the fold."""
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
    vector = [0] * LAYOUT.slots
    vector[LAYOUT.block("material")] = MATERIAL
    for slot, value in (entries or {}).items():
        vector[slot] = value
    return vector


def row(
    identifier, coefficients, vector, phase=24, fen="4k3/8/8/8/8/8/8/4K3 w - - 0 1"
):
    """One row in the shape `arche terms` prints it, with the evaluation it
    states worked out from the weights the way the engine does."""
    evaluation = tune.reconstruct(coefficients, vector, LAYOUT)
    terms = " ".join(f"{slot}:{coefficient}" for slot, coefficient in coefficients)
    return f"{identifier} {evaluation} {phase} {len(coefficients)} {terms} {fen}"


def extraction(rows, vector):
    header = (
        f"terms positions {len(rows)} in_check 0 unsettled 0 drawn 0 kept {len(rows)}"
    )
    line = "weights {} {}".format(
        LAYOUT.slots, " ".join(str(value) for value in vector)
    )
    return [header, LAYOUT_LINE, line, *rows]


def test_the_divide_truncates_toward_zero():
    """Rust's `/` truncates and python's `//` floors, so on a negative
    numerator that does not divide evenly the two are a centipawn apart."""
    assert tune.trunc_div(-980, 24) == -40
    assert -980 // 24 == -41
    assert tune.trunc_div(980, 24) == 40
    # they agree wherever the divide is exact, so a careless numerator would
    # say nothing
    assert tune.trunc_div(-720, 24) == -720 // 24


def test_material_is_added_outside_the_divide():
    """Folding the material into the numerator by scaling it up gives a
    different integer."""
    vector = weights({0: -1})
    # a knight and a pawn's worth of material, and a piece square numerator of
    # minus one, which does not divide by the taper
    coefficients = [(0, 1), (LAYOUT.start["material"], 1)]
    assert tune.reconstruct(coefficients, vector, LAYOUT) == 100 + tune.trunc_div(
        -1, 24
    )
    folded = (100 * 24 - 1) // 24
    assert folded != tune.reconstruct(coefficients, vector, LAYOUT)


def test_a_row_that_does_not_rebuild_stops_the_run():
    """A row that does not rebuild means this file and the engine have parted
    company, so it raises rather than being dropped."""
    vector = weights({0: 30})
    good = row("a", [(0, 24), (LAYOUT.start["material"], 1)], vector)
    _, _, rows = tune.parse_terms(extraction([good], vector))
    assert len(rows) == 1
    words = good.split(" ")
    words[1] = str(int(words[1]) + 1)
    with pytest.raises(ValueError, match="rebuilds to"):
        tune.parse_terms(extraction([" ".join(words)], vector))


def test_a_row_reads_from_the_right_and_an_id_can_hold_spaces():
    """An id can hold a space (nine of the bench's eighteen positions do), so
    the fields are found from the right. The three rows are the shapes the
    reading has to survive: a name with a space, a line called by its own fen,
    and a row with no coefficients."""
    vector = weights({5: 12, 400: -7})
    fen = "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1"
    coefficients = [(5, 24), (400, -3), (LAYOUT.start["material"] + 4, -1)]
    lines = [
        row("ruy lopez", coefficients, vector, 18, fen),
        row(fen, coefficients, vector, 18, fen),
        row("start", [], vector, 24, fen),
    ]
    _, _, rows = tune.parse_terms(extraction(lines, vector))
    assert [parsed.id for parsed in rows] == ["ruy lopez", fen, "start"]
    assert [parsed.fen for parsed in rows] == [fen, fen, fen]
    assert rows[0].phase == 18
    assert rows[0].coefficients == coefficients
    assert rows[2].coefficients == []


def test_a_weights_line_of_the_wrong_length_is_refused():
    """A vector of another length than the layout's is from another engine."""
    with pytest.raises(ValueError, match="weights line"):
        tune.parse_terms([LAYOUT_LINE, "weights 3 1 2 3"])
    with pytest.raises(ValueError, match="no weights line"):
        tune.parse_terms(
            [
                "terms positions 0 in_check 0 unsettled 0 drawn 0 kept 0",
                LAYOUT_LINE,
            ]
        )


def test_a_header_without_the_drawn_count_is_refused():
    """The header is a version check. An extraction from an engine with no
    drawn material rule would parse and rebuild row for row, and a fit over
    it would fit an evaluation the engine no longer runs. This file cannot
    find those rows without a second copy of the rule, so the count is what
    is read."""
    vector = weights()
    lines = extraction([row("a", [(LAYOUT.start["material"], 1)], vector)], vector)
    _, _, rows = tune.parse_terms(lines)
    assert len(rows) == 1
    for header in [
        "terms positions 1 in_check 0 unsettled 0 kept 1",
        "terms epd corpus.epd positions 1 in_check 0 unsettled 0 kept 1",
    ]:
        with pytest.raises(ValueError, match="extraction is to redo"):
            tune.parse_terms([header, *lines[1:]])


@pytest.mark.parametrize("count", [518, 774, 782, 790, 796, 812])
def test_a_vector_of_an_earlier_layout_is_refused(tmp_path, count):
    """The lengths the vector had before the endgame tables, before mobility,
    before the shelter, before the pawn storm, before the pawn structure and
    before the king attack zone. Every slot any of them names exists in the
    layout that replaced it, so their numbers would land on the wrong weights
    rather than failing to parse. Both doors a vector comes through refuse
    them."""
    assert LAYOUT.slots == 820
    old = [0] * count
    with pytest.raises(ValueError, match=f"of {count}, expected 820"):
        tune.parse_terms(
            [
                LAYOUT_LINE,
                "weights {} {}".format(len(old), " ".join(str(w) for w in old)),
            ]
        )
    written = tmp_path / "fitted.json"
    written.write_text(json.dumps(old), encoding="utf-8")
    with pytest.raises(ValueError, match=f"of {count}, expected 820"):
        tune.read_weights(written, LAYOUT)


def test_a_header_without_a_layout_line_is_refused():
    """An extraction from an engine older than the layout line parses and
    every slot it names exists, so a layout assumed for it would be wrong with
    nothing saying so. The run is refused whether the line is missing or comes
    after the vector it describes."""
    vector = weights()
    lines = extraction([row("a", [(LAYOUT.start["material"], 1)], vector)], vector)
    assert lines[1] == LAYOUT_LINE
    with pytest.raises(ValueError, match="states no layout"):
        tune.parse_terms([lines[0], *lines[2:]])
    with pytest.raises(ValueError, match="states no layout"):
        tune.parse_terms([lines[0], lines[2], lines[1], *lines[3:]])


def test_a_term_the_layout_names_and_nothing_prices_is_refused():
    """A term this file has no bound for cannot be screened against the
    packed halves, so a run that names one is refused rather than fitted with
    a boardful that leaves the new weights out. The same for a term whose
    width has moved, since the bound is per count."""
    with pytest.raises(ValueError, match="no bounds for"):
        tune.Layout.of(LAYOUT_LINE + " king_tropism 4")
    with pytest.raises(ValueError, match="different number of counts"):
        tune.Layout.of(LAYOUT_LINE.replace("shelter 7", "shelter 9"))
    # and the terms it does price are priced count by count
    for name in LAYOUT.terms:
        assert len(tune.BOUNDS[name]) == LAYOUT.widths[name]


def test_a_row_whose_id_opens_with_the_header_word_is_kept():
    """The header is skipped on the two shapes the engine writes it in, not on
    its first word, which an id can open with too."""
    vector = weights()
    lines = extraction(
        [row("terms of the endgame", [(LAYOUT.start["material"], 1)], vector)], vector
    )
    _, _, rows = tune.parse_terms(lines)
    assert [parsed.id for parsed in rows] == ["terms of the endgame"]
    # and both shapes of the header are still skipped rather than read as rows
    header = "terms epd corpus.epd positions 1 in_check 0 unsettled 0 drawn 0 kept 1"
    _, _, rows = tune.parse_terms([header, *lines])
    assert [parsed.id for parsed in rows] == ["terms of the endgame"]


def test_a_row_whose_id_opens_with_the_layout_word_is_kept():
    """The layout line is claimed with the space after the word, so an id that
    opens with the same letters and runs on is a row."""
    vector = weights()
    lines = extraction(
        [row("layouts of the endgame", [(LAYOUT.start["material"], 1)], vector)], vector
    )
    _, _, rows = tune.parse_terms(lines)
    assert [parsed.id for parsed in rows] == ["layouts of the endgame"]


def test_a_row_carries_the_pair_term_once_the_run_says_it_is_on():
    """With a `factors` line the fourth number after the id is the pair term's
    score, which the row adds outside the divide and the corpus adds to every
    score as a constant no weight moves."""
    vector = weights({0: 30})
    coefficients = [(0, 24), (LAYOUT.start["material"], 1)]
    fen = "4k3/8/8/8/8/8/8/4K3 w - - 0 1"
    base = tune.reconstruct(coefficients, vector, LAYOUT)
    terms = " ".join(f"{slot}:{coefficient}" for slot, coefficient in coefficients)
    line = f"factors of the pair {base + 7} 24 {len(coefficients)} 7 {terms} {fen}"
    header, layout, weights_line = extraction([], vector)
    _, _, rows = tune.parse_terms([header, layout, weights_line, "factors 8 64", line])
    assert [(parsed.id, parsed.eval, parsed.machine) for parsed in rows] == [
        ("factors of the pair", base + 7, 7)
    ]
    # the same row in a run without the line has a number too many, and the
    # count it then reads does not match the coefficients it prints
    with pytest.raises(ValueError, match="says 7 coefficients and prints 2"):
        tune.parse_terms([header, layout, weights_line, line])


def test_a_run_without_the_pair_term_reads_as_before():
    """Rows extracted while the term was off carry no fourth number and score
    no pair term."""
    vector = weights({0: 30})
    lines = extraction(
        [row("a", [(0, 24), (LAYOUT.start["material"], 1)], vector)], vector
    )
    _, _, rows = tune.parse_terms(lines)
    assert [parsed.machine for parsed in rows] == [0]


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
    """A corpus from before the game operand would be split into one game per
    position, which is the leak the game split closed."""
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
    _, shipped, parsed = tune.parse_terms(extraction(rows, vector))
    corpus = tune.Corpus(LAYOUT, shipped, parsed, labels)
    assert [r.id for r in corpus.rows] == ["g00002p020"]
    assert corpus.sealed.positions == 1
    assert corpus.sealed.pairs == 1


def test_a_corpus_that_repeats_a_position_across_games_is_refused():
    """The corpus is deduplicated by fen before it is labelled, so no position
    is in two groups; one that was not would be the fen split's leak in a new
    place."""
    vector = weights({0: 7})
    fen = "4k3/8/8/8/8/8/8/4K3 w - - 0 1"
    rows = [
        row("g00001p020", [(0, 24), (LAYOUT.start["material"], 1)], vector, 24, fen),
        row("g00002p031", [(0, 24), (LAYOUT.start["material"], 1)], vector, 24, fen),
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
            rows.append(
                row(name, [(0, 24), (LAYOUT.start["material"], 1)], vector, 24, fen)
            )
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
    _, _, parsed = tune.parse_terms(extraction(rows, vector))
    return tune.Corpus(LAYOUT, vector, parsed, labels_of(labels))


def test_the_integer_score_is_the_evaluation_the_engine_gave():
    """The integer scores, with the truncation put back, are the row's own
    column at the shipped weights."""
    vector = weights({0: -30, 64: 17, 400: 5})
    rows = [
        row(
            "a",
            [(0, 7), (64, -13), (LAYOUT.start["material"], 1)],
            vector,
            7,
            "a w - - 0 1",
        ),
        row(
            "b",
            [(0, -11), (400, 3), (LAYOUT.start["material"] + 3, -2)],
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
    reached it, so a position two games reached pulls the loss towards its
    own result."""
    vector = weights({0: 20})
    rows = [
        row("a", [(0, 24), (LAYOUT.start["material"], 1)], vector, 24, "a w - - 0 1"),
        row("b", [(0, 24), (LAYOUT.start["material"], 1)], vector, 24, "b w - - 0 1"),
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
    # the row the evaluation is right about is the one weighted up
    assert weighted < flat


def test_a_slots_support_is_how_many_rows_it_appears_in():
    """A weight the corpus barely constrains says so before it ships."""
    vector = weights({0: 5})
    rows = [
        row("a", [(0, 24), (LAYOUT.start["material"], 1)], vector, 24, "a w - - 0 1"),
        row("b", [(LAYOUT.start["material"], 1)], vector, 24, "b w - - 0 1"),
    ]
    corpus = corpus_of(
        rows,
        vector,
        {"a": (1.0, 1, key(1, TRAIN)), "b": (0.5, 1, key(2, TRAIN))},
    )
    support = corpus.support()
    assert support[0] == 1
    assert support[LAYOUT.start["material"]] == 2
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
    """Two vectors scored on the same positions are a paired sample, and the
    mean difference comes with its standard error."""
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
    """Two games of a hundred positions each, differing by game and not within
    one, are two draws and not two hundred."""
    counts = np.ones(200)
    games = np.array(["a"] * 100 + ["b"] * 100)
    first = np.zeros(200)
    second = np.concatenate([np.full(100, 0.02), np.full(100, -0.02)])
    mean, clustered, naive, design = tune.paired_difference(
        first, second, counts, games
    )
    assert mean == pytest.approx(0.0)
    # the whole spread is between the games, so the two-game interval is the
    # full half-swing and the per-position one a fourteenth of it
    assert clustered == pytest.approx(0.02, rel=1e-6)
    assert naive == pytest.approx(0.02 / math.sqrt(200), rel=1e-6)
    assert design == pytest.approx(clustered / naive, rel=1e-6)
    # where every row is a game of its own the two agree but for the
    # correction a sample of two hundred carries
    alone = np.array([str(index) for index in range(200)])
    _, clustered, naive, design = tune.paired_difference(first, second, counts, alone)
    assert design == pytest.approx(math.sqrt(200 / 199), rel=1e-9)


def test_the_optimiser_finds_the_bottom_of_a_bowl():
    """The bowl is scaled so the useful step is many times longer than one,
    as on the real loss, where a search that only backtracked would stall."""

    centre = np.arange(LAYOUT.slots, dtype=float)

    def objective(x):
        slack = (x - centre) * 1e-4
        return float(slack @ slack), 2e-8 * (x - centre)

    found, value = tune.lbfgs(objective, np.zeros(LAYOUT.slots))
    assert value < 1e-12
    assert np.max(np.abs(found - centre)) < 1e-3


def test_a_vector_the_engine_could_not_carry_is_refused():
    """A fit that grew the tables past a boardful of `i16` is no candidate
    whatever it scores."""
    inside, worst = tune.bounds_hold(np.array(weights()), LAYOUT)
    assert inside and worst == 0
    huge = np.array(weights({slot: 400 for slot in range(LAYOUT.start["material"])}))
    inside, worst = tune.bounds_hold(huge, LAYOUT)
    assert not inside
    assert worst == 2 * 64 * 400


def test_the_mobility_weights_are_priced_too():
    """The boardful reads as the whole vector, so the mobility weights have to
    be in it. The range stops at the shelter block, priced below."""
    one_each = weights(
        {slot: 1 for slot in range(LAYOUT.start["mobility"], LAYOUT.start["shelter"])}
    )
    _, worst = tune.bounds_hold(np.array(one_each), LAYOUT)
    assert worst == 2 * int(np.array(tune.BOUNDS["mobility"]).sum())
    huge = weights(
        {
            slot: 5000
            for slot in range(LAYOUT.start["mobility"], LAYOUT.start["shelter"])
        }
    )
    assert not tune.bounds_hold(np.array(huge), LAYOUT)[0]


def test_the_shelter_weights_are_priced_too():
    """The same for the block after it. Three of each count a side, both
    colours, at the larger of the two halves, which here is the endgame one at
    ten a count."""
    vector = weights(
        {
            LAYOUT.start["shelter"] + index: 5
            for index in range(LAYOUT.widths["shelter"])
        }
        | {
            LAYOUT.start["shelter"] + LAYOUT.widths["shelter"] + index: 10
            for index in range(LAYOUT.widths["shelter"])
        }
    )
    inside, worst = tune.bounds_hold(np.array(vector), LAYOUT)
    assert inside
    assert worst == 2 * tune.BOUNDS["shelter"][0] * LAYOUT.widths["shelter"] * 10
    huge = weights(
        {
            slot: 5000
            for slot in range(LAYOUT.start["shelter"], LAYOUT.start["pawn_structure"])
        }
    )
    assert not tune.bounds_hold(np.array(huge), LAYOUT)[0]


def test_the_pawn_structure_weights_are_priced_too():
    """And the block after that one. Eight of each count a side, both colours,
    at the larger of the two halves, which here is the endgame one at four a
    count."""
    vector = weights(
        {
            LAYOUT.start["pawn_structure"] + index: 3
            for index in range(LAYOUT.widths["pawn_structure"])
        }
        | {
            LAYOUT.start["pawn_structure"] + LAYOUT.widths["pawn_structure"] + index: 4
            for index in range(LAYOUT.widths["pawn_structure"])
        }
    )
    inside, worst = tune.bounds_hold(np.array(vector), LAYOUT)
    assert inside
    assert (
        worst
        == 2 * tune.BOUNDS["pawn_structure"][0] * LAYOUT.widths["pawn_structure"] * 4
    )
    huge = weights(
        {
            slot: 5000
            for slot in range(
                LAYOUT.start["pawn_structure"], LAYOUT.start["king_attack"]
            )
        }
    )
    assert not tune.bounds_hold(np.array(huge), LAYOUT)[0]


def test_the_king_attack_weights_are_priced_too():
    """And the last block of the four. What one knight, one bishop, one rook
    and one queen can show of the ring, a side, both colours, at the larger of
    the two halves, which here is the endgame one at seven a count."""
    vector = weights(
        {
            LAYOUT.start["king_attack"] + index: 5
            for index in range(LAYOUT.widths["king_attack"])
        }
        | {
            LAYOUT.start["king_attack"] + LAYOUT.widths["king_attack"] + index: 7
            for index in range(LAYOUT.widths["king_attack"])
        }
    )
    inside, worst = tune.bounds_hold(np.array(vector), LAYOUT)
    assert inside
    assert worst == 2 * int(np.array(tune.BOUNDS["king_attack"]).sum()) * 7
    huge = weights(
        {slot: 5000 for slot in range(LAYOUT.start["king_attack"], LAYOUT.slots)}
    )
    assert not tune.bounds_hold(np.array(huge), LAYOUT)[0]


def test_the_material_block_is_the_only_thing_outside_the_divide():
    """Mobility and the shelter go inside the taper's divide the way the
    tables do; outside it either would answer a centipawn away from the
    engine wherever a numerator is negative and does not divide evenly.

    The last two lines use the fixture's weights rather than the shipped
    ones, so a coefficient sorted into the wrong half rebuilds to a different
    number whatever the shipped weights happen to be (a zero weight would
    rebuild to the same number either side).
    """
    material = [slot for slot in range(LAYOUT.slots) if LAYOUT.is_material(slot)]
    assert material == list(range(LAYOUT.start["material"], LAYOUT.start["mobility"]))
    assert not any(
        LAYOUT.is_material(slot)
        for slot in range(LAYOUT.start["mobility"], LAYOUT.slots)
    )
    vector = weights({LAYOUT.start["shelter"]: 30})
    vector[LAYOUT.start["material"]] = 100
    # a pawn outside the divide, and a shelter count of a full phase inside it
    coefficients = [(LAYOUT.start["shelter"], 24), (LAYOUT.start["material"], 1)]
    assert tune.reconstruct(coefficients, vector, LAYOUT) == 130


def test_a_fit_is_free_to_move_the_leaf_terms_weights():
    """Material is held for a first fit and nothing after it is. A freeze that
    ran to the end of the vector, as it did before mobility, would hold every
    leaf term at zero and report a null result."""
    frozen = tune.frozen_slots(LAYOUT, False)
    assert frozen[LAYOUT.start["material"] : LAYOUT.start["mobility"]].all()
    assert not frozen[LAYOUT.start["mobility"] :].any()
    assert not frozen[: LAYOUT.start["material"]].any()
    assert not tune.frozen_slots(LAYOUT, True).any()


def test_a_term_is_fitted_with_every_earlier_term_held():
    """Each hold freezes its own block, so a fit of the newest term names
    every hold below it and leaves that term alone free."""
    tables = tune.frozen_slots(LAYOUT, False, ["tables"])
    assert not tables[LAYOUT.start["mobility"] : LAYOUT.start["shelter"]].any()
    both = tune.frozen_slots(LAYOUT, False, ["tables", "mobility"])
    assert both[: LAYOUT.start["shelter"]].all()
    assert not both[LAYOUT.start["shelter"] : LAYOUT.start["pawn_structure"]].any()
    three = tune.frozen_slots(LAYOUT, False, ["tables", "mobility", "shelter"])
    assert three[: LAYOUT.start["pawn_structure"]].all()
    assert not three[LAYOUT.start["pawn_structure"] : LAYOUT.start["king_attack"]].any()
    four = tune.frozen_slots(
        LAYOUT, False, ["tables", "mobility", "shelter", "pawn_structure"]
    )
    assert four[: LAYOUT.start["king_attack"]].all()
    assert not four[LAYOUT.start["king_attack"] :].any()


def test_a_refit_holds_the_terms_above_it_as_well_as_the_ones_below():
    """A mobility refit holds the tables below it and the shelter, the pawn
    structure and the king attack zone above, so the mobility weights are the
    only thing that moves. Without the pawn structure hold the pawn weights
    move too, which is the confound `1b0862a` found the first time a hold was
    missing."""
    refit = tune.frozen_slots(
        LAYOUT, False, ["tables", "shelter", "pawn_structure", "king_attack"]
    )
    assert refit[: LAYOUT.start["mobility"]].all()
    assert not refit[LAYOUT.start["mobility"] : LAYOUT.start["shelter"]].any()
    assert refit[LAYOUT.start["shelter"] :].all()


def test_the_king_attack_hold_freezes_its_eight_slots_and_no_more():
    """The hold takes the eight slots the term occupies and not one either
    side."""
    held = tune.frozen_slots(LAYOUT, True, ["king_attack"])
    assert held.sum() == 8
    assert held[LAYOUT.block("king_attack")].all()
    assert not held[: LAYOUT.start["king_attack"]].any()


def test_the_tables_hold_freezes_the_two_table_blocks_and_no_more():
    """`--free-material --hold tables` is a run the fit documents, and it
    freezes the 768 table entries and nothing else. Material is not part of the
    tables hold: it is held by leaving `--free-material` off, which every other
    hold test here does, so nothing but this pins the two apart."""
    held = tune.frozen_slots(LAYOUT, True, ["tables"])
    assert held.sum() == LAYOUT.widths["midgame"] + LAYOUT.widths["endgame"]
    assert held[LAYOUT.block("midgame")].all()
    assert held[LAYOUT.block("endgame")].all()
    assert not held[LAYOUT.start["material"] :].any()


def test_every_term_the_layout_names_can_be_held():
    """Every leaf term of `LAYOUT_LINE` is holdable, walked from the line
    rather than listed again here. What that guards is the holds: a term is
    held by the name it is laid out under, so one added to the line costs no
    edit to `frozen_slots` and none here. It does cost a `BOUNDS` entry, and
    until it has one every command refuses the run, which
    `test_a_term_the_layout_names_and_nothing_prices_is_refused` covers."""
    assert LAYOUT.terms
    for name in LAYOUT.terms:
        held = tune.frozen_slots(LAYOUT, True, [name])
        assert held[LAYOUT.block(name)].all()
        assert held.sum() == 2 * LAYOUT.widths[name]


def test_a_hold_off_the_layout_is_refused():
    """A term the run never printed is a typo or a name from another engine,
    and either way the fit it asks for is not the fit it would get. The refusal
    names it and says what it could have named. A command line value, so it is
    refused the way a share outside (0, 1] is rather than raised at."""
    with pytest.raises(SystemExit, match="a hold of knight_outposts") as refused:
        tune.frozen_slots(LAYOUT, False, ["mobility", "knight_outposts"])
    for name in LAYOUT.terms:
        assert name in str(refused.value)
    # the fixed blocks are not names either: the tables are held as `tables`
    # and material by leaving `--free-material` off
    for name in ("midgame", "endgame", "material"):
        with pytest.raises(SystemExit, match=f"a hold of {name}"):
            tune.frozen_slots(LAYOUT, False, [name])


def test_every_recorded_fit_recipe_freezes_what_it_froze_before():
    """The combinations the recorded fits were run under, each against the
    blocks it froze. The old spelling was five flags in a fixed order and is
    gone, so the masks are stated as the blocks rather than asked of it."""
    # what a first fit freezes: material, held unless freed, and the two table
    # blocks that `tables` names
    base = ["material", "midgame", "endgame"]
    recipes = [
        (["tables"], base),
        (["tables", "mobility"], [*base, "mobility"]),
        (["tables", "mobility", "shelter"], [*base, "mobility", "shelter"]),
        (
            ["tables", "mobility", "shelter", "pawn_structure"],
            [*base, "mobility", "shelter", "pawn_structure"],
        ),
        (
            ["tables", "shelter", "pawn_structure", "king_attack"],
            [*base, "shelter", "pawn_structure", "king_attack"],
        ),
    ]
    for held, blocks in recipes:
        expected = np.zeros(LAYOUT.slots, dtype=bool)
        for name in blocks:
            expected[LAYOUT.block(name)] = True
        assert list(tune.frozen_slots(LAYOUT, False, held)) == list(expected), held


def test_quantizing_rounds_to_nearest():
    assert list(tune.quantize([1.4, 1.6, -1.4, -1.6, 2.5])) == [1, 2, -1, -2, 2]


def sample(vector, count=30, plies=4, extra=()):
    """A corpus of whole games spread across the five slices of the key: six
    games a slice, so eighteen train, six choose the ridge and six are sealed.
    The group is the key's first byte and the fold its second, moved
    independently here because a fixture whose folds followed its groups
    would leave two folds empty.

    `extra` names slots to give every row a coefficient in, signed the way the
    material one is so the fit has something to move there. Without it the only
    supported slots are the first table entry and a material value, which
    leaves a claim about any other block unfalsifiable.
    """
    rows, labels = [], {}
    for index in range(count):
        result = 1.0 if index % 2 else 0.0
        sign = 1 if index % 2 else -1
        for ply in range(plies):
            name = f"g{index:05d}p{ply:03d}"
            fen = f"4k3/8/8/8/8/{index}p/{ply}p/4K3 w - - 0 1"
            rows.append(
                row(
                    name,
                    [
                        (0, 24),
                        (LAYOUT.start["material"], sign),
                        *((slot, sign) for slot in extra),
                    ],
                    vector,
                    24,
                    fen,
                )
            )
            labels[name] = (result, 1, key(index, index * len(groups.SLICES) // count))
    return rows, labels


def fixture_run(tmp_path, vector, rows, labels, name="corpus.epd", drop=()):
    """The two files a run reads. `drop` names groups to leave out of the
    corpus file."""
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
    distribution before any loss. The sealed group is named and not scored."""
    vector = weights({0: 20, 64: -30})
    rows, labels = sample(vector)
    terms, corpus = fixture_run(tmp_path, vector, rows, labels)
    assert tune.main(["loss", "--terms", str(terms), "--corpus", str(corpus)]) == 0
    printed = capsys.readouterr().out
    # eighteen games train and six choose the ridge, at four positions a game
    assert "corpus positions 96 train 72 selection 24" in printed
    assert "games 30 train 18 selection 6 calibration 6" in printed
    assert "calibration positions 24 appearances 24 sealed, not read here" in printed
    assert "shipped selection mse" in printed
    assert "support least" in printed


def test_a_candidate_is_scored_against_the_shipped_weights(tmp_path, capsys):
    """A candidate vector is read from json and reported beside the shipped
    one, with the paired difference and its interval."""
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
    # the naive per-position interval beside the honest one, and their ratio
    assert " per position " in printed
    assert " design " in printed


def test_a_fit_holds_the_material_values_unless_it_is_told_not_to(tmp_path, capsys):
    """The delta margin in quiescence reads `eval::material`, so moving it
    changes the tree for a reason unrelated to the evaluation's accuracy."""
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
    assert fitted[LAYOUT.start["material"] : LAYOUT.start["mobility"]] == MATERIAL
    assert len(fitted) == LAYOUT.slots


def test_a_hold_a_run_names_reaches_the_fit(tmp_path, capsys):
    """A term named on the command line is a term the fitted vector left alone.
    The same fit without the hold moves that term, so the held run is held and
    not merely unsupported: the fixture gives the mobility block a coefficient
    on every row for that reason."""
    vector = weights({0: 20})
    mobility = LAYOUT.block("mobility")
    rows, labels = sample(vector, extra=[LAYOUT.start["mobility"]])
    terms, corpus = fixture_run(tmp_path, vector, rows, labels)

    def fitted(name, held):
        out = tmp_path / f"{name}.json"
        run = ["fit", "--terms", str(terms), "--corpus", str(corpus)]
        run += ["--out", str(out), "--penalties", "1e-6"]
        for term in held:
            run += ["--hold", term]
        assert tune.main(run) == 0
        capsys.readouterr()
        return json.loads(out.read_text(encoding="utf-8"))

    free = fitted("free", [])
    held = fitted("held", ["tables", "mobility"])
    assert held[mobility] == vector[mobility]
    assert free[mobility] != vector[mobility]
    for name in ("midgame", "endgame"):
        assert held[LAYOUT.block(name)] == vector[LAYOUT.block(name)]
    assert free[:64] != vector[:64]


def test_a_hold_off_the_layout_is_refused_before_a_row_is_read(tmp_path):
    """The names are checked against the extraction's layout line, which is why
    a hold is not a flag of its own: the parser is built before any run has said
    what its terms are. The check reads the head of the extraction and nothing
    else, so it beats both the row rebuild, here a row that cannot rebuild at
    all, and the corpus, here a file that is not there."""
    vector = weights({0: 20})
    rows, labels = sample(vector)
    terms, _ = fixture_run(tmp_path, vector, rows, labels)
    wrecked = "wrecked 9999 24 1 0:24 4k3/8/8/8/8/8/8/4K3 w - - 0 1"
    terms.write_text(
        terms.read_text(encoding="utf-8") + wrecked + "\n", encoding="utf-8"
    )
    # not a file, so a run that read the corpus first would raise about that
    run = ["fit", "--terms", str(terms), "--corpus", str(tmp_path / "nowhere.epd")]
    with pytest.raises(SystemExit, match="a hold of mobilty"):
        tune.main([*run, "--hold", "tables", "--hold", "mobilty"])
    # the same run with the hold spelled right gets as far as the wrecked row
    with pytest.raises(ValueError, match="wrecked rebuilds to"):
        tune.main([*run, "--hold", "tables", "--hold", "mobility"])


def test_an_extraction_the_holds_cannot_be_read_off_is_left_to_the_parse(tmp_path):
    """Where the head states no layout the names can be read off, the hold check
    does nothing and the extraction is refused for what is wrong with it, in the
    words `parse_terms` uses. A hold is checked against a layout line or not at
    all: a line read past its first fault would offer a width as a term and
    refuse a hold the run has.

    The three shapes: the line missing, the line arriving after the vector it
    describes, and a name with no width, which moves every name after it onto a
    width.
    """
    vector = weights({0: 20})
    rows, labels = sample(vector)
    terms, corpus = fixture_run(tmp_path, vector, rows, labels)
    lines = terms.read_text(encoding="utf-8").splitlines()
    assert lines[1] == LAYOUT_LINE
    dangling = LAYOUT_LINE.replace("mobility 4", "mobility")
    run = ["fit", "--terms", str(terms), "--corpus", str(corpus)]
    for head, refusal in (
        ([lines[0], *lines[2:]], "carries no layout line"),
        ([lines[0], lines[2], lines[1], *lines[3:]], "carries no layout line"),
        ([lines[0], dangling, *lines[2:]], "a name and no width"),
    ):
        terms.write_text("\n".join(head) + "\n", encoding="utf-8")
        # `shelter` is a term the layout means to name, so a check that read the
        # broken line would refuse a hold the run offers
        for hold in ("tables", "shelter", "mobilty"):
            with pytest.raises(ValueError, match=refusal):
                tune.main([*run, "--hold", hold])


def test_a_hold_the_run_can_have_does_not_displace_its_own_refusals(tmp_path):
    """An extraction the parse would refuse is refused for what is wrong with
    it, as long as the holds are ones its layout line offers. Here the header is
    one this file does not know, which is read before the rows and stays the
    refusal.

    A hold the line does not offer is answered first, on every shape whose names
    can be read at all, because that is what checking before the parse means.
    The extraction's own refusal comes on the next run, once the hold is spelled
    right, which the second half of this test is.
    """
    vector = weights({0: 20})
    rows, labels = sample(vector)
    terms, corpus = fixture_run(tmp_path, vector, rows, labels)
    lines = terms.read_text(encoding="utf-8").splitlines()
    lines[0] = lines[0].replace(" drawn 0", "")
    terms.write_text("\n".join(lines) + "\n", encoding="utf-8")
    run = ["fit", "--terms", str(terms), "--corpus", str(corpus)]
    with pytest.raises(SystemExit, match="a hold of mobilty"):
        tune.main([*run, "--hold", "mobilty"])
    with pytest.raises(ValueError, match="extraction is to redo"):
        tune.main([*run, "--hold", "mobility"])


def test_a_ridge_is_chosen_on_the_selection_games(tmp_path, capsys):
    """The grid is fitted on the training games and ranked on the selection
    games. Every penalty is printed with what it bought and its interval, and
    the chosen one is named."""
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
    """Five folds over the games the run may read, which the sealed group is
    not among."""
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
    # said in full, so it is not pasted into `fit --penalties`
    assert "best penalty over the folds" in printed
    assert "fit chooses on the selection group instead" in printed
    # thirty-two of the forty games are in the two groups the run may read
    folded = sum(
        int(line.split(" games ")[1].split(" ")[0])
        for line in printed.splitlines()
        if line.startswith("fold ")
    )
    assert folded == 32


def test_final_opens_the_sealed_group_once_and_logs_it_first(tmp_path, capsys):
    """The log names the corpus and the sealed games by checksum, the same
    sealed games are refused a second time whatever file they arrive in, and
    a corpus whose sealed games differ opens."""
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
    # training row's count moved, and the file's checksum with it
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
    """The loss printed is the loss over the sealed rows, worked out here by
    hand."""
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
    # scored the way the fixture's rows state their own evaluation
    sealed = [
        (index, result)
        for index, (result, _, game) in enumerate(labels.values())
        if groups.group_of(game) == "calibration"
    ]
    scores = np.array(
        [
            tune.reconstruct(
                [(0, 24), (LAYOUT.start["material"], 1 if result == 1.0 else -1)],
                vector,
                LAYOUT,
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
    """A reading against anything but integers is a reading of a vector that
    will not ship."""
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
    """A draw takes whole pairs, the draws differ, every fit is read on the
    selection group `fit` reads, the whole is fitted once, and the same seed
    draws the same curve."""
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
    """The same fit over a corpus holding the calibration games and over one
    with those rows cut out writes the same vector and prints the same
    numbers. The calibration games are labelled the opposite way round to
    every other game, so a run that read one row of them could not come out
    the same.
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
                # but for the lines saying how big the sealed group is
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
    """Read through the checksum `final` logs, so this asserts the identity of
    the group and not its size: the pairs named are sealed, no other pair is,
    and the same corpus read without the file seals a different set."""
    vector = weights()
    rows, raw = sample(vector)
    _, _, parsed = tune.parse_terms(extraction(rows, vector))
    labels = labels_of(raw)
    named = {min(label.pair for label in labels.values())}
    corpus = tune.Corpus(LAYOUT, vector, parsed, labels, named)
    assert corpus.sealed.checksum() == sealed_checksum(named)
    assert named.isdisjoint(corpus.pairs)
    assert set(corpus.groups) == {"train", "selection"}
    drawn = tune.Corpus(LAYOUT, vector, parsed, labels)
    assert drawn.sealed.checksum() != sealed_checksum(named)


def test_a_sealed_pair_no_game_of_the_corpus_holds_is_refused():
    """A pair the corpus has no game for is a seal drawn against another
    archive."""
    vector = weights()
    rows, raw = sample(vector)
    _, _, parsed = tune.parse_terms(extraction(rows, vector))
    with pytest.raises(ValueError, match="in no game of this corpus"):
        tune.Corpus(LAYOUT, vector, parsed, labels_of(raw), {"f" * 64})


def test_a_sealed_pair_with_no_row_is_counted_rather_than_refused():
    """A named pair whose positions were all claimed by a lower key or
    filtered out of the extraction is sealed and contributes nothing, which is
    a number to report and not a fault. The 2026-09-13 mobility corpus had
    four of them in 4,750."""
    vector = weights()
    rows, raw = sample(vector)
    labels = labels_of(raw)
    # one pair's rows dropped from the extraction with its games left in the
    # corpus. A fixture row opens with its id
    lonely = min(label.pair for label in labels.values())
    dropped = {name for name, label in labels.items() if label.pair == lonely}
    kept = [row for row in rows if row.split(" ", 1)[0] not in dropped]
    assert len(kept) < len(rows), "the fixture shares no pair"
    _, _, parsed = tune.parse_terms(extraction(kept, vector))
    corpus = tune.Corpus(LAYOUT, vector, parsed, labels, {lonely})
    assert corpus.sealed_without_rows == 1
    assert corpus.sealed.positions == 0
