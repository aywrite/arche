"""Tests for the tactical suite converter.

The engine has no san parser, so a published suite's `bm Rxb2` is turned into
the coordinate move the engine speaks before it is committed. What is under
test is the part of that a hand written converter gets subtly wrong:
disambiguation, the check and mate suffixes, castling and promotion, several
acceptable moves on one line, and refusing rather than skipping a move it
cannot read.
"""

import build_tactics
import pytest

ITALIAN = "r1bqk2r/pppp1ppp/2n2n2/2b1p3/2B1P3/5N2/PPPP1PPP/RNBQK2R w KQkq -"
PROMOTION = "8/4P3/8/8/8/k7/8/K7 w - -"
# two knights can reach a4, so the suite writes the file of the one it means
TWO_KNIGHTS = "1n2rr2/1pk3pp/pNn2p2/2N1p3/8/6P1/PP2PPKP/2RR4 w - -"


def convert(text):
    return build_tactics.convert(text)


def test_a_san_move_becomes_a_coordinate_move():
    source = '8/7p/5k2/5p2/p1p2P2/Pr1pPK2/1P1R3P/8 b - - bm Rxb2; id "WAC.002";'
    assert convert(source) == [
        '8/7p/5k2/5p2/p1p2P2/Pr1pPK2/1P1R3P/8 b - - bm b3b2; id "WAC.002";'
    ]


def test_the_id_survives_and_the_rest_of_the_operations_do_not():
    # the suite carries one 2004 engine's analysis of the position beside it,
    # which describes that engine rather than the position
    fen = "2rr3k/pp3pp1/1nnqbN1p/3pN3/2pP4/2P3Q1/PPB4P/R4RK1 w - -"
    source = (
        f"{fen} acd 4; acn 21146; acs 1; bm Qg6; ce 32764; "
        'id "WAC.001"; pv Qg6 fxg6 Nxg6#;'
    )
    assert convert(source) == [f'{fen} bm g3g6; id "WAC.001";']


def test_several_acceptable_moves_all_convert():
    fen = "r1bqk2r/ppp1nppp/4p3/n5N1/2BPp3/P1P5/2P2PPP/R1BQK2R w KQkq -"
    assert convert(f'{fen} bm Bxa2 Nxf7; id "WAC.022";') == [
        f'{fen} bm c4a2 g5f7; id "WAC.022";'
    ]


def test_castling_converts_to_the_king_move():
    assert convert(f'{ITALIAN} bm O-O; id "white";') == [
        f'{ITALIAN} bm e1g1; id "white";'
    ]
    black = ITALIAN.replace(" w ", " b ")
    assert convert(f'{black} bm O-O; id "black";') == [f'{black} bm e8g8; id "black";']


def test_promotion_carries_the_piece_the_engine_writes():
    assert convert(f'{PROMOTION} bm e8=Q e8=N; id "promotion";') == [
        f'{PROMOTION} bm e7e8q e7e8n; id "promotion";'
    ]


def test_a_disambiguated_move_picks_the_right_piece():
    # both knights reach a4; the one on c5 is meant, not the one on b6
    assert convert(f'{TWO_KNIGHTS} bm Nca4; id "WAC.299";') == [
        f'{TWO_KNIGHTS} bm c5a4; id "WAC.299";'
    ]


def test_the_check_and_mate_suffixes_are_read():
    mate = "6k1/R7/6K1/8/8/8/8/8 w - -"
    assert convert(f'{mate} bm Ra8#; id "mate";') == [f'{mate} bm a7a8; id "mate";']
    check = "6k1/R7/8/8/8/8/8/6K1 w - -"
    assert convert(f'{check} bm Ra8+; id "check";') == [f'{check} bm a7a8; id "check";']


def test_comments_and_blank_lines_are_skipped():
    source = (
        "# a header comment\n"
        "\n"
        f'{ITALIAN} bm O-O; id "one";\n'
        "\n"
        "# and a note in the middle\n"
        f'{TWO_KNIGHTS} bm Nca4; id "two";\n'
    )
    assert convert(source) == [
        f'{ITALIAN} bm e1g1; id "one";',
        f'{TWO_KNIGHTS} bm c5a4; id "two";',
    ]


def test_a_move_that_cannot_be_read_raises_rather_than_being_skipped():
    # a suite that drops what it could not read gates on a number that means
    # something other than what it claims
    for san in ["Rxb2", "Qz9", "O-O-O"]:
        with pytest.raises(ValueError) as raised:
            convert(f'{ITALIAN} bm {san}; id "WAC.777";')
        assert "WAC.777" in str(raised.value)
        assert san in str(raised.value)


def test_one_unreadable_move_fails_the_whole_run():
    source = f'{ITALIAN} bm O-O; id "one";\n{ITALIAN} bm Rxb2; id "two";\n'
    with pytest.raises(ValueError):
        convert(source)


def test_a_position_with_no_bm_or_no_id_raises():
    with pytest.raises(ValueError, match="no bm operation"):
        convert(f'{ITALIAN} id "no move";')
    with pytest.raises(ValueError, match="no id operation"):
        convert(f"{ITALIAN} bm O-O;")
    with pytest.raises(ValueError, match="no bm operation"):
        convert(f'{ITALIAN} bm ; id "empty";')


def test_a_line_that_is_not_a_position_raises():
    with pytest.raises(ValueError, match="not an epd position"):
        convert("rnbqkbnr w\n")


def test_a_fen_that_will_not_parse_raises():
    with pytest.raises(ValueError, match="cannot read fen"):
        convert('9/8/8/8/8/8/8/8 w - - bm a1a2; id "bad";')


def test_a_quoted_operand_may_hold_a_semicolon():
    source = f'{ITALIAN} bm O-O; id "one; and two";'
    assert convert(source) == [f'{ITALIAN} bm e1g1; id "one; and two";']


def test_the_output_is_a_four_field_fen_as_bench_epd_is():
    line = convert(f'{ITALIAN} bm O-O; id "one";')[0]
    assert line.split(" bm ")[0] == ITALIAN
    assert len(ITALIAN.split()) == 4


def test_the_header_pins_the_source_and_states_the_rule(tmp_path):
    source = tmp_path / "wac.epd"
    source.write_text(f'{ITALIAN} bm O-O; id "one";\n', encoding="utf-8")
    output = tmp_path / "tactics.epd"
    assert build_tactics.main([str(source), str(output)]) == 0

    written = output.read_text(encoding="utf-8")
    header, _, last = written.partition("\n" + ITALIAN)
    assert last.strip() == 'bm e1g1; id "one";'
    assert build_tactics.SOURCE_REF in header
    assert "github.com/jwiegley/emacs-chess" in header
    assert "scripts/build_tactics.py" in header
    assert "add to the end rather than edit" in header
    assert all(line.startswith("#") for line in header.splitlines())


def test_the_file_is_written_with_unix_line_endings(tmp_path):
    # it is committed, and the repository normalises to unix line endings, so
    # a windows run must not produce a diff against a linux one
    source = tmp_path / "wac.epd"
    source.write_text(f'{ITALIAN} bm O-O; id "one";\n', encoding="utf-8")
    output = tmp_path / "tactics.epd"
    build_tactics.main([str(source), str(output)])
    assert b"\r\n" not in output.read_bytes()
