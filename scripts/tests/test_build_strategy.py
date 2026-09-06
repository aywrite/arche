# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2022-2026 Andrew Wright

"""Tests for the strategic suite converter.

The source grades ten moves a position and names them twice, once in san and
once in coordinate notation, so the conversion keeps a column rather than
parsing a move. What is under test is therefore the checking: the two lists
that have to line up, the id the theme is read out of, the one theme spelt two
ways, and that a line nobody is sure of stops the run rather than reaching a
file that gates a build.
"""

import hashlib

import build_strategy
import pytest

FEN = "1kr5/3n4/q3p2p/p2n2p1/PppB1P2/5BP1/1P2Q2P/3R2K1 w - -"
OTHER = "1n5k/3q3p/pp1p2pB/5r2/1PP1Qp2/P6P/6P1/2R3K1 w - -"


def source(
    fen=FEN,
    identifier='id "STS(v1.0) Undermine.001";',
    c8='c8 "100 46 23";',
    c9='c9 "f4f5 d4f2 f3g4";',
    extra='bm f5; c0 "f5=100, Bf2=46, Bg4=23"; c7 "f5 Bf2 Bg4"; Ae "Stockish 15";',
):
    """One line in the shape the source writes, with every part of it something
    a test can replace."""
    return f"{fen} {extra} {identifier} {c8} {c9}\n"


def convert(text, expected=None):
    return build_strategy.convert(text, expected or {"Undermine": 1})


def read(line):
    """The operations of a converted line, as the engine's reader takes them."""
    return build_strategy.operations_of(line.split(None, 4)[4])


def test_a_position_keeps_its_fen_its_best_move_and_its_scores():
    assert convert(source()) == [
        f'{FEN} bm f4f5; id "Undermine.001"; points "f4f5=100 d4f2=46 f3g4=23";'
    ]


def test_the_best_move_is_the_highest_scoring_one_and_not_the_first():
    line = convert(source(c8='c8 "23 100 46";'))[0]
    assert read(line)["bm"] == "d4f2"


def test_every_move_at_the_top_score_is_named():
    # the source has sixty eight of these, three of them with all ten moves
    # level, and a converter that picked one of them would be picking it at
    # random out of moves the source calls equal
    line = convert(source(c8='c8 "100 100 23";'))[0]
    assert read(line)["bm"] == "f4f5 d4f2"


def test_the_analysis_the_source_repeats_on_every_line_is_dropped():
    # c0 and c7 are the same moves in san, c8 and c9 are now the points
    # operation, and Ae names the analysis the header names
    assert set(read(convert(source())[0])) == {"bm", "id", "points"}


def test_the_version_tag_comes_off_the_id():
    for tag in ["v1.0", "v2.2", "v15.0"]:
        line = convert(source(identifier=f'id "STS({tag}) Undermine.001";'))[0]
        assert read(line)["id"] == "Undermine.001"


def test_a_tag_that_names_two_themes_raises():
    # the tag is dropped because it says nothing the theme does not, which is
    # only true while each tag goes with one theme
    text = source() + source(identifier='id "STS(v1.0) Square Vacancy.001";')
    with pytest.raises(ValueError, match="names both"):
        convert(text, {"Undermine": 1, "Square Vacancy": 1})


def test_the_odd_spelling_of_one_theme_is_folded_into_the_other_ninety_nine():
    spelt = 'id "STS(v3.0) Knight Outposts/Centralization/Repositioning.027";'
    line = convert(
        source(identifier=spelt),
        {"Knight Outposts/Repositioning/Centralization": 1},
    )[0]
    assert read(line)["id"] == "Knight Outposts/Repositioning/Centralization.027"


def test_an_abbreviated_theme_is_left_as_the_source_writes_it():
    # expanding AKPC or AT would be a guess at what they stand for
    for theme in ["AKPC", "AT"]:
        line = convert(source(identifier=f'id "STS(v8.0) {theme}.001";'), {theme: 1})[0]
        assert read(line)["id"] == f"{theme}.001"


def test_lists_that_do_not_line_up_raise():
    with pytest.raises(ValueError, match="cannot be paired"):
        convert(source(c8='c8 "100 46";'))
    with pytest.raises(ValueError, match="cannot be paired"):
        convert(source(c9='c9 "f4f5 d4f2";'))


def test_a_missing_list_raises():
    for missing in ["c8", "c9"]:
        with pytest.raises(ValueError, match=f"no {missing} operation"):
            convert(source(**{missing: ""}))
    with pytest.raises(ValueError, match="no c8 operation"):
        convert(source(c8='c8 "";'))


def test_a_score_that_is_not_one_of_the_hundred_raises():
    # the suite is scored out of a hundred everywhere it is described, and a
    # score above that reaches the engine's reader as a number it cannot hold
    for scores in ["100 46 0", "100 46 -1", "100 46 x", "100 46 2.5", "101 46 23"]:
        with pytest.raises(ValueError, match="is not a score"):
            convert(source(c8=f'c8 "{scores}";'))


def test_a_move_that_is_not_a_coordinate_move_raises():
    for move in ["Bf2", "f4f9", "f4", "f4f5k"]:
        with pytest.raises(ValueError, match="is not a coordinate move"):
            convert(source(c9=f'c9 "f4f5 d4f2 {move}";'))


def test_a_promotion_is_a_coordinate_move():
    line = convert(source(c9='c9 "f4f5 d4f2 b2b1q";'))[0]
    assert read(line)["points"].endswith("b2b1q=23")


def test_a_move_graded_twice_raises():
    with pytest.raises(ValueError, match="graded twice"):
        convert(source(c9='c9 "f4f5 d4f2 f4f5";'))


def test_an_opcode_twice_on_one_line_raises():
    # taking the last of them would convert the wrong ten moves, or rescore
    # the right ten, and say nothing about either
    with pytest.raises(ValueError, match="c9 appears twice"):
        convert(source(c9='c9 "f4f5 d4f2 f3g4"; c9 "a1a2 a2a3 a3a4";'))
    with pytest.raises(ValueError, match="c8 appears twice"):
        convert(source(c8='c8 "100 46 23"; c8 "1 2 3";'))


def test_two_positions_with_one_id_raise():
    # the theme counts would still add up, and the engine's reader would then
    # hold two positions it could not tell apart in a report
    text = source() + source(fen=OTHER)
    with pytest.raises(ValueError, match="Undermine.001 is named twice"):
        convert(text, {"Undermine": 2})


def test_an_id_that_is_not_an_sts_id_raises():
    for identifier in ['id "Undermine.001";', 'id "STS(v1.0) Undermine";', 'id "";']:
        with pytest.raises(ValueError):
            convert(source(identifier=identifier))
    with pytest.raises(ValueError, match="no id operation"):
        convert(source(identifier=""))


def test_a_line_that_is_not_a_position_raises():
    with pytest.raises(ValueError, match="not an epd position"):
        convert("rnbqkbnr w\n")


def test_a_source_that_is_not_the_suite_this_is_raises():
    with pytest.raises(ValueError, match="not the"):
        convert(source(), {"Undermine": 100})
    with pytest.raises(ValueError, match="not the"):
        convert(source(), {"Undermine": 1, "7th Rank": 1})


def test_one_unreadable_line_fails_the_whole_run():
    text = source() + source(fen=OTHER, c8='c8 "100 46";')
    with pytest.raises(ValueError):
        convert(text)


def test_the_themes_are_the_fifteen_hundred_positions_of_the_suite():
    assert len(build_strategy.THEMES) == 15
    assert sum(build_strategy.THEMES.values()) == 1500


def test_the_emitted_line_parses_back_to_the_moves_and_scores_it_was_given():
    scores = [100, 46, 23]
    moves = ["f4f5", "d4f2", "f3g4"]
    operations = read(convert(source())[0])
    graded = [entry.split("=") for entry in operations["points"].split()]
    assert [move for move, _ in graded] == moves
    assert [int(score) for _, score in graded] == scores
    assert operations["bm"] == moves[scores.index(max(scores))]


def test_the_output_is_a_four_field_fen_as_the_other_suites_are():
    line = convert(source())[0]
    assert line.split(" bm ")[0] == FEN
    assert len(FEN.split()) == 4


def written(tmp_path, monkeypatch, text=None):
    """Run the script over a fixture, with the pin and the theme counts moved
    to what the fixture is, and give back what it wrote."""
    text = source() if text is None else text
    source_file = tmp_path / "sts.epd"
    source_file.write_text(text, encoding="utf-8")
    monkeypatch.setattr(
        build_strategy,
        "SOURCE_SHA256",
        hashlib.sha256(text.encode("utf-8")).hexdigest(),
    )
    monkeypatch.setattr(build_strategy, "THEMES", {"Undermine": 1})
    output = tmp_path / "strategy.epd"
    assert build_strategy.main([str(source_file), str(output)]) == 0
    return output


def test_the_header_pins_the_source_and_states_the_rule(tmp_path, monkeypatch):
    text = written(tmp_path, monkeypatch).read_text(encoding="utf-8")
    header, _, last = text.partition("\n" + FEN)
    assert last.strip().startswith("bm f4f5;")
    assert build_strategy.SOURCE_REF in header
    assert build_strategy.SOURCE_SHA256 in header
    assert "scripts/build_strategy.py" in header
    # the licence asks that the notice travel with the work, and this file is
    # the work travelling
    assert "MIT" in header and "Copyright (c) 2019 fsmosca" in header
    assert "Permission is hereby granted" in header
    assert "THE SOFTWARE IS PROVIDED" in header
    assert "Stockfish 15" in header
    assert "add to the end rather than edit" in header
    assert all(line.startswith("#") for line in header.splitlines())


def test_a_source_that_is_not_the_pinned_one_is_refused(tmp_path):
    source_file = tmp_path / "sts.epd"
    source_file.write_text(source(), encoding="utf-8")
    with pytest.raises(ValueError, match="not the pinned"):
        build_strategy.main([str(source_file), str(tmp_path / "strategy.epd")])


def test_the_file_is_written_with_unix_line_endings(tmp_path, monkeypatch):
    # it is committed, and the repository normalises to unix line endings, so
    # a windows run must not produce a diff against a linux one
    assert b"\r\n" not in written(tmp_path, monkeypatch).read_bytes()
