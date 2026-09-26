# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2022-2026 Andrew Wright

"""Tests for reading the speed of a match's games.

The games are written here in the shape fastchess leaves them: a comment for
every ply, a book move's included, and a searched move's carrying its time and
the nodes and rate of the engine's last info line.
"""

import ingame_speed
import pytest
import speed

CANDIDATE = "c" * 40
BASELINE = "b" * 40


def searched(seconds, nodes, counted=None):
    """A searched move's comment. Its last info line covered `counted` of
    the move's seconds, all of them unless said."""
    rate = round(nodes / (seconds if counted is None else counted))
    return (
        f"{{+0.10/12 {seconds}s, tl=9.1s, latency=0.000s, n={nodes}, sd=20, "
        f"nps={rate}}}"
    )


def game(white, black, white_moves, black_moves, book=2, fen=None, plies=None):
    """A game whose sides make the searched moves given, as (seconds, nodes)
    or (seconds, nodes, counted), after `book` plies of book."""
    comments = ["{book}"] * book
    for w, b in zip(white_moves, black_moves):
        comments += [searched(*w), searched(*b)]
    tags = [
        '[Event "Fastchess Tournament"]',
        f'[White "{white}"]',
        f'[Black "{black}"]',
        f'[PlyCount "{len(comments) if plies is None else plies}"]',
    ]
    if fen:
        tags.append(f'[FEN "{fen}"]')
    body = " ".join(f"m{i} {c}" for i, c in enumerate(comments))
    return "\n".join(tags) + "\n\n" + body + " 1/2-1/2\n"


def movetext(text):
    return text.split("\n\n", 1)[1]


def test_each_side_is_read_from_its_own_plies():
    text = game("w", "b", [(1.0, 100), (1.0, 300)], [(2.0, 1000), (2.0, 1000)])
    both, comments = ingame_speed.sides(movetext(text), False)
    assert comments == 6
    assert [side.nodes for side in both] == [400, 2000]
    assert [side.thought for side in both] == [2.0, 4.0]


def test_a_move_is_read_over_the_span_its_last_info_covers():
    # stopped a third of the way into an iteration: the last line counted
    # 1000 nodes over the first two seconds of three, and the rest of the
    # time searched nodes nobody reported. The rate is 500 a second, where
    # nodes over the whole time would say 333
    text = game(CANDIDATE, BASELINE, [(3.0, 1000, 2.0)], [(2.0, 1000)])
    games, _ = ingame_speed.games_in(text, [CANDIDATE], [BASELINE])
    assert [(g.candidate_nps, g.baseline_nps) for g in games] == [(500, 500)]


def test_a_game_from_a_position_with_black_to_move_starts_with_black():
    # the first comment is then black's, so an odd book keeps the sides right
    text = " ".join(["{book}", searched(1.0, 50), searched(1.0, 70)])
    both, _ = ingame_speed.sides(text, True)
    assert [side.nodes for side in both] == [50, 70]


def test_the_candidate_is_found_on_either_colour():
    text = game(
        CANDIDATE, BASELINE, [(1.0, 1100), (1.0, 1100)], [(1.0, 1000), (1.0, 1000)]
    ) + game(
        BASELINE, CANDIDATE, [(1.0, 1000), (1.0, 1000)], [(1.0, 1100), (1.0, 1100)]
    )
    games, skipped = ingame_speed.games_in(text, [CANDIDATE], [BASELINE])
    assert skipped == 0
    assert [(g.candidate_nps, g.baseline_nps) for g in games] == [(1100, 1000)] * 2


def test_an_engine_is_named_by_its_sha_whole_or_short():
    text = game(CANDIDATE[:9], BASELINE, [(1.0, 10)], [(1.0, 10)])
    games, _ = ingame_speed.games_in(text, [CANDIDATE], [BASELINE])
    assert len(games) == 1
    # and a game between other engines is not counted
    games, _ = ingame_speed.games_in(text, ["d" * 40], [BASELINE])
    assert games == []


def test_a_run_that_named_its_sides_by_ref_is_read_by_those_names():
    # a run given a branch and a tag writes those into the games, not shas
    text = game("claude/some-branch", "v0.4.5", [(1.0, 1100)], [(1.0, 1000)])
    games, _ = ingame_speed.games_in(
        text, ["claude/some-branch", CANDIDATE], ["v0.4.5", BASELINE]
    )
    assert [(g.candidate_nps, g.baseline_nps) for g in games] == [(1100, 1000)]
    # and a name is matched whole unless both are shas
    games, _ = ingame_speed.games_in(text, ["claude/some"], ["v0.4"])
    assert games == []


def test_sides_that_share_a_name_are_refused(tmp_path, capsys):
    with pytest.raises(SystemExit) as left:
        ingame_speed.main([str(tmp_path), "--candidate", "x", "--baseline", "x"])
    assert left.value.code == 2
    assert "share a name" in capsys.readouterr().err


def test_a_game_whose_comments_miss_a_ply_is_left_out():
    # a missing comment would credit every later ply to the other side
    text = game(CANDIDATE, BASELINE, [(1.0, 100)], [(1.0, 100)], plies=5)
    games, skipped = ingame_speed.games_in(text, [CANDIDATE], [BASELINE])
    assert (games, skipped) == ([], 1)


def test_a_side_that_barely_thought_is_left_out():
    text = game(CANDIDATE, BASELINE, [(0.4, 100)], [(2.0, 100)])
    games, skipped = ingame_speed.games_in(text, [CANDIDATE], [BASELINE])
    assert (games, skipped) == ([], 1)


def test_the_report_gives_the_paired_change():
    games = [ingame_speed.Game(1020 + i % 3, 1000) for i in range(30)]
    lines = ingame_speed.report(games, skipped=2)
    assert lines[0] == "### Speed in the games"
    assert "Nodes a second, candidate against baseline: +2.1%" in lines[2]
    assert "over 30 games" in lines[2]
    assert any(line.startswith("2 games are left out") for line in lines)


def test_too_few_games_say_so_rather_than_an_interval():
    games = [ingame_speed.Game(1100, 1000)] * 5
    assert "too few for an interval" in ingame_speed.report(games, 0)[2]


def test_the_directory_is_searched_for_every_shard(tmp_path, capsys):
    for shard in range(3):
        directory = tmp_path / f"shard-{shard}"
        directory.mkdir()
        (directory / "games.pgn").write_text(
            "".join(
                game(CANDIDATE, BASELINE, [(1.0, 1050)], [(1.0, 1000)])
                for _ in range(4)
            )
        )
    argv = [str(tmp_path), "--candidate", CANDIDATE, "--baseline", BASELINE]
    assert ingame_speed.main(argv) == 0
    assert (
        "+5.0% (95% interval +5.0% to +5.0%) over 12 games" in capsys.readouterr().out
    )


def test_past_the_exact_count_the_bound_is_the_normal_one(monkeypatch):
    # within one of the exact count, and never wider than it
    for n in (150, 200):
        exact = speed.signed_rank_depth(n)
        monkeypatch.setattr(speed, "EXACT", 0)
        approximate = speed.signed_rank_depth(n)
        monkeypatch.setattr(speed, "EXACT", 200)
        assert exact - 1 <= approximate <= exact
    # and a match's worth of games does not overflow
    assert speed.signed_rank_depth(5000) > 0
