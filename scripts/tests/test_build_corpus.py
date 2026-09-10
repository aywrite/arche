# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2022-2026 Andrew Wright

"""Tests for the tuning corpus builder.

What is under test is the reading rather than the writing: which plies are the
book's, which games are not play, which way round a result is read, and how a
position two games reached is labelled. The epd it prints is what the engine
parses, so the shape of a line is pinned as well.

The game key has tests of its own, because two properties rest on it. The
tuner's three groups are assigned from it, so it has to be the movetext's and
nothing else or a re-extraction would move games between groups. And a position
two games reached belongs to the group of the lower of their keys and is
labelled by that group's games alone, which is what keeps a repeated position
out of two groups at once and keeps a sealed game's result out of a label the
fit reads. Two games can also key alike, and the counters say how many do,
because a repeated game looks like nothing at all in the other numbers.
"""

import hashlib
import io

import build_corpus
import chess.pgn
import groups
import pytest

# Two move orders reaching the same position, which is how a test asks for one
# position in two games. The keys are the movetexts', so which group each game
# falls in is fixed, and the tests below assert what they need of it rather
# than assuming it.
DIRECT = "1. e4 e5 2. Nf3 Nc6 3. Bb5"
TRANSPOSED = "1. Nf3 Nc6 2. e4 e5 3. Bb5"


def pgn(
    moves="1. e4 e5 2. Nf3 Nc6 3. Bb5 a6 4. Ba4 Nf6 5. O-O Be7",
    result="1-0",
    termination="normal",
):
    """One game in the shape a fastchess archive writes."""
    return (
        f'[Event "match"]\n[Result "{result}"]\n[Termination "{termination}"]\n\n'
        f"{moves} {result}\n\n"
    )


def games(text):
    handle = io.StringIO(text)
    while True:
        game = chess.pgn.read_game(handle)
        if game is None:
            break
        yield game


def build(text, book_plies=build_corpus.BOOK_PLIES):
    return build_corpus.corpus(games(text), book_plies)


def test_the_book_is_dropped_off_the_front_of_a_game():
    """The corpus starts where the book stops, so its opening variety is the
    book's rather than the engine's."""
    entries, counts = build(pgn())
    assert counts["plies"] == 10
    # ten plies, all of them inside the first sixteen
    assert counts["post_book"] == 0
    assert entries == {}
    # the same game read with a shorter book keeps what is past it
    entries, counts = build(pgn(), book_plies=6)
    assert counts["post_book"] == 4
    assert len(entries) == 4


def test_a_ply_the_book_played_is_dropped_wherever_it_stands():
    """A run whose book runs past the sixteenth ply says so in a comment, and
    the comment is read as well as the count."""
    moves = "1. e4 e5 2. Nf3 {book} Nc6 3. Bb5 a6"
    entries, counts = build(moves_pgn(moves), book_plies=2)
    assert counts["post_book"] == 3
    assert len(entries) == 3


def moves_pgn(moves, result="1-0", termination="normal"):
    return pgn(moves=moves, result=result, termination=termination)


def test_a_result_is_read_from_the_side_to_move():
    """The label is what the side to move went on to do, so the same game
    labels white's positions and black's oppositely."""
    entries, _ = build(pgn(result="1-0"), book_plies=0)
    labelled = [(entry.id, entry.result) for entry in entries.values()]
    # the first position is white's, the second black's, and so on
    assert [result for _, result in labelled[:4]] == [1.0, 0.0, 1.0, 0.0]
    entries, _ = build(pgn(result="0-1"), book_plies=0)
    assert [entry.result for entry in list(entries.values())[:4]] == [
        0.0,
        1.0,
        0.0,
        1.0,
    ]
    entries, _ = build(pgn(result="1/2-1/2"), book_plies=0)
    assert {entry.result for entry in entries.values()} == {0.5}


def test_a_game_that_did_not_end_in_play_is_dropped_whole():
    """A crash or a stall labels every position before it with a result the
    play did not earn, so the game goes rather than the last move."""
    for game in (
        pgn(termination="time forfeit"),
        moves_pgn("1. e4 e5 2. Nf3 {white disconnects} Nc6"),
    ):
        entries, counts = build(game, book_plies=0)
        assert counts["dropped"] == 1
        assert entries == {}


def test_a_game_with_no_result_is_dropped():
    """An unfinished game has nothing to label with."""
    entries, counts = build(pgn(result="*"), book_plies=0)
    assert counts["dropped"] == 1
    assert entries == {}


def test_a_position_two_games_reached_carries_the_mean_of_them():
    """Duplicates are a few per cent of the plies, so this is a small
    correction. It is an exact one, and the count is what a weighted loss
    reads."""
    entries, counts = build(pgn(result="1-0") + pgn(result="0-1"), book_plies=0)
    assert counts["games"] == 2
    # the two games are the same moves, so every position is a duplicate
    assert counts["post_book"] == 20
    assert len(entries) == 10
    assert counts["repeated"] == 10
    for entry in entries.values():
        assert entry.count == 2
        assert entry.result == 0.5


def test_a_game_key_is_its_movetext_and_nothing_else():
    """The key is what the tuner's three groups are assigned from, so it has to
    be a property of the play. Two games of the same moves key alike whatever
    their headers say, and a game one move different does not."""
    played = "1. e4 e5 2. Nf3 Nc6"
    first = next(games(moves_pgn(played, result="1-0")))
    same = next(games(moves_pgn(played, result="0-1")))
    other = next(games(moves_pgn("1. e4 e5 2. Nf3 Nf6")))
    assert build_corpus.game_key(first) == build_corpus.game_key(same)
    assert build_corpus.game_key(first) != build_corpus.game_key(other)
    # and it is the sha256 the split reads sixty-four hex characters of
    assert (
        build_corpus.game_key(first)
        == hashlib.sha256(b"e2e4 e7e5 g1f3 b8c6").hexdigest()
    )


def test_two_games_of_the_same_moves_are_counted():
    """A repeated game and a repeated position look the same in every other
    number, and a game whose movetext another game already had is one game's
    evidence counted twice. Nothing else in the run says so."""
    entries, counts = build(pgn(result="1-0") + pgn(result="0-1"), book_plies=0)
    # the same moves, so the same key, whatever the two games ended in
    assert counts["games"] == 2
    assert counts["same_key"] == 1
    assert len({entry.key for entry in entries.values()}) == 1
    # a game a move different keys apart and is counted apart
    _, counts = build(
        moves_pgn("1. e4 e5 2. Nf3 Nc6") + moves_pgn("1. e4 e5 2. Nf3 Nf6"),
        book_plies=0,
    )
    assert counts["same_key"] == 0


def test_a_position_two_games_reached_belongs_to_the_lower_key():
    """Grouping by the game loses the property that rows sharing a position
    land on one side, because the games that reached it can fall in different
    groups. The lowest key owns the position, which restores it."""
    keys = sorted(
        build_corpus.game_key(game)
        for game in games(moves_pgn(DIRECT) + moves_pgn(TRANSPOSED))
    )
    both_ways = (
        moves_pgn(DIRECT) + moves_pgn(TRANSPOSED),
        moves_pgn(TRANSPOSED) + moves_pgn(DIRECT),
    )
    for text in both_ways:
        entries, _ = build(text, book_plies=4)
        entry = next(iter(entries.values()))
        # whichever order the games arrived in, and whichever of them the
        # position was first seen in
        assert entry.key == keys[0]


def test_a_position_is_labelled_by_its_own_group_and_no_other():
    """The appearance in another group is dropped rather than meaned in.

    These two games fall in different groups, so merging them would put the
    result of a game in one group into a label the other group's fit reads. The
    position takes the owning game's result alone and weighs one, and the
    counters say an appearance went.
    """
    text = moves_pgn(DIRECT, result="1-0") + moves_pgn(TRANSPOSED, result="0-1")
    keys = dict(zip((build_corpus.game_key(game) for game in games(text)), (1.0, 0.0)))
    owner = min(keys)
    assert len({groups.group_of(key) for key in keys}) == 2
    entries, counts = build(text, book_plies=4)
    entry = next(iter(entries.values()))
    # white is to move in the shared position, so the label is the owning
    # game's result the way white saw it and not the mean of the two, which
    # would have been a half
    assert entry.result == keys[owner]
    assert entry.count == 1
    assert entry.dropped == 1
    assert counts["repeated"] == 1
    assert counts["straddled"] == 1
    assert counts["dropped_appearances"] == 1


def test_a_position_two_games_of_one_group_reached_merges_as_before():
    """Where both games are in one group nothing is dropped and the label is
    the mean of the two, which is what the rule did everywhere before it was
    made group-local."""
    direct, transposed = f"{DIRECT} Nf6", f"{TRANSPOSED} Nf6"
    keys = [
        build_corpus.game_key(game)
        for game in games(moves_pgn(direct) + moves_pgn(transposed))
    ]
    assert len({groups.group_of(key) for key in keys}) == 1
    entries, counts = build(
        moves_pgn(direct, result="1-0") + moves_pgn(transposed, result="0-1"),
        book_plies=4,
    )
    assert len(entries) == 2
    for entry in entries.values():
        assert entry.count == 2
        assert entry.result == 0.5
        assert entry.dropped == 0
    assert counts["repeated"] == 2
    assert counts["straddled"] == 0
    assert counts["dropped_appearances"] == 0


def test_a_position_is_deduplicated_by_the_epd_the_engine_reads():
    """Four fields and no clocks, so two games reaching the same diagram by
    different move orders are one row."""
    entries, counts = build(moves_pgn(DIRECT) + moves_pgn(TRANSPOSED), book_plies=4)
    assert counts["post_book"] == 2
    assert len(entries) == 1
    assert len(next(iter(entries.values())).appearances) == 2


def test_a_line_is_the_epd_the_engine_parses():
    """The engine reads four fields and then operations, so the line names the
    position, the game it belongs to, the label and how many times it was
    reached."""
    entries, _ = build(pgn(), book_plies=8)
    key = build_corpus.game_key(next(games(pgn())))
    line = build_corpus.render(entries).splitlines()[0]
    position, *operations = line.split("; ")
    fen, name = position.rsplit(" id ", 1)
    assert len(fen.split(" ")) == 4
    assert name.startswith('"g00001p008')
    assert operations == [f'game "{key}"', 'result "1.0000"', 'count "1";']


def test_a_mean_result_is_written_to_four_places():
    """A mean over a handful of games, and a place further would be spelling
    out a repeating decimal."""
    entries, _ = build(
        pgn(result="1-0") + pgn(result="1-0") + pgn(result="0-1"), book_plies=0
    )
    line = build_corpus.render(entries).splitlines()[0]
    assert 'result "0.6667"' in line
    assert 'count "3"' in line


def test_a_run_with_nothing_past_the_book_says_so_rather_than_writing_a_file(tmp_path):
    out = tmp_path / "corpus.epd"
    source = tmp_path / "games.pgn"
    source.write_text(pgn(), encoding="utf-8")
    assert build_corpus.main([str(source), "--out", str(out)]) == 1
    assert not out.exists()


def test_a_directory_stands_for_the_games_inside_it(tmp_path):
    """A strength run's artifact is a directory holding games.pgn, so the run
    can be named rather than the file."""
    run = tmp_path / "9979240008"
    run.mkdir()
    (run / "games.pgn").write_text(pgn(), encoding="utf-8")
    out = tmp_path / "corpus.epd"
    assert build_corpus.main([str(run), "--out", str(out), "--book-plies", "4"]) == 0
    assert len(out.read_text(encoding="utf-8").splitlines()) == 6


@pytest.mark.parametrize("turn,expected", [(True, 0.25), (False, 0.75)])
def test_a_black_position_reads_the_result_the_other_way_up(turn, expected):
    assert build_corpus.result_for(0.25, turn) == expected
