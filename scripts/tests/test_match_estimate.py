# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2022-2026 Andrew Wright

"""Tests for the pooled match estimate.

The input is a pgn per shard, so the fixtures are shaped the way the pinned
fastchess writes one: a Round tag on every game, the two games of a round
playing the same opening with the colours reversed, and the ending in the
Termination tag with the reason at the end of the last comment. What this
guards against is a pooled estimate that pairs games from different shards,
that reads a colour it assumed rather than one the tags gave, or that states
an interval the games do not support.
"""

import math
import subprocess
import sys
from pathlib import Path

import match_estimate

SCRIPT = Path(match_estimate.__file__)

CANDIDATE = "new"
BASELINE = "old"

# the comment fastchess puts on the last move, with the reason its tail
PLAYED = (
    "{+0.15/5 0.010s, tl=2.054s, latency=0.000s, n=83485, sd=20, nps=8348500,"
    ' hashfull=0, pv="e1g1 e8g8 c3e2"'
)


def game(
    round_id=1, result="1-0", swap=False, termination="normal", reason="White mates"
):
    """One game, shaped the way fastchess writes them. The candidate has white
    unless the sides are swapped, which is what the second game of a round
    does."""
    white, black = (BASELINE, CANDIDATE) if swap else (CANDIDATE, BASELINE)
    return (
        f'[Event "Fastchess Tournament"]\n'
        f'[Site "?"]\n'
        f'[Round "{round_id}"]\n'
        f'[White "{white}"]\n'
        f'[Black "{black}"]\n'
        f'[Result "{result}"]\n'
        f'[Termination "{termination}"]\n'
        f"\n1. e4 {{book}} e5 {{book}} 2. Nf3 {PLAYED}, {reason}}} {result}\n\n"
    )


def pair(round_id, first="1-0", second="0-1"):
    """A round as `-repeat` plays it: one opening, then the same one with the
    colours reversed. The results are as the pgn states them, so a candidate
    that won both of its games is 1-0 followed by 0-1."""
    return game(round_id, first) + game(round_id, second, swap=True)


def drawn(round_id):
    return pair(round_id, "1/2-1/2", "1/2-1/2")


def rounds(drawn_pairs, decided_pairs, won=True):
    """A match of drawn pairs and decided ones, which is a score off a half
    with a spread to it."""
    played = [drawn(index) for index in range(drawn_pairs)]
    first, second = ("1-0", "0-1") if won else ("0-1", "1-0")
    played += [
        pair(drawn_pairs + index, first, second) for index in range(decided_pairs)
    ]
    return "".join(played)


def shard(tmp_path, name, text):
    """A shard as download-artifact leaves it: one directory per artifact,
    named after the artifact, with the shard's games inside."""
    directory = tmp_path / name
    directory.mkdir(parents=True, exist_ok=True)
    pgn = directory / "games.pgn"
    pgn.write_text(text)
    return pgn


def pooled(tmp_path, texts):
    """The shards, read and pooled the way the command line does."""
    paths = [
        shard(tmp_path, f"strength-1-1-shard-{index}", text)
        for index, text in enumerate(texts)
    ]
    shards, whole = match_estimate.read_shards(paths, CANDIDATE)
    games = [score for one in shards for score in one.games]
    pairs = [score for one in shards for score in one.pairs]
    estimate = match_estimate.Estimate(sum(games), len(games), pairs)
    return shards, estimate, whole


class TestPairing:
    def test_a_round_holds_the_two_games_of_one_opening(self):
        read, _ = match_estimate.read_games(pair(1) + pair(2), CANDIDATE)
        assert sorted(read) == ["1", "2"]
        scores, unpaired = match_estimate.pair_up(read)
        assert scores == [2.0, 2.0]
        assert unpaired == 0

    def test_two_shards_are_not_pooled_into_one_round(self, tmp_path):
        # every shard numbers its rounds from one, so pairing on the round
        # alone would put a game with one from another slice of the book
        shards, estimate, _ = pooled(tmp_path, [pair(1), pair(1)])
        assert [len(one.pairs) for one in shards] == [1, 1]
        assert estimate.pairs == 2
        assert estimate.games == 4

    def test_the_colour_is_read_from_the_tags_not_assumed(self):
        # the candidate has black in the second game of every round, so a
        # reader that assumed white would score its losses as wins
        read, _ = match_estimate.read_games(pair(1, "0-1", "1-0"), CANDIDATE)
        assert read["1"] == [0.0, 0.0]

    def test_an_odd_game_counts_in_the_score_and_not_in_the_pairs(self):
        # a shard the clock stopped in the middle of a round leaves one game
        read, _ = match_estimate.read_games(pair(1) + game(2, "1-0"), CANDIDATE)
        scores, unpaired = match_estimate.pair_up(read)
        assert scores == [2.0]
        assert unpaired == 1
        estimate = match_estimate.Estimate(3.0, 3, scores)
        assert estimate.games == 3
        assert estimate.pairs == 1

    def test_a_game_with_no_result_is_left_out_and_counted(self):
        read, unfinished = match_estimate.read_games(pair(1) + game(2, "*"), CANDIDATE)
        assert unfinished == 1
        assert sum(len(games) for games in read.values()) == 2

    def test_a_game_the_candidate_did_not_play_is_not_its_game(self):
        other = game(1).replace(f'[White "{CANDIDATE}"]', '[White "someone"]')
        read, _ = match_estimate.read_games(other, CANDIDATE)
        assert read == {}


class TestEstimate:
    def test_a_match_of_draws_is_no_difference_at_all(self, tmp_path):
        _, estimate, _ = pooled(tmp_path, [drawn(1) + drawn(2), drawn(1)])
        assert estimate.score == 0.5
        assert estimate.elo == 0.0

    def test_a_seventy_five_percent_score_is_a_hundred_and_ninety_one_elo(self):
        # the logistic model, -400 log10(1/p - 1), which is what the rest of
        # this tooling reads a score with
        estimate = match_estimate.Estimate(3.0, 4, [2.0, 1.0])
        assert round(estimate.elo) == 191

    def test_every_pair_shared_leaves_no_spread_to_measure(self, tmp_path):
        # each round was won one way and lost the other, so every pair scored
        # one out of two and the variance over the pairs is nought
        halves = pair(1, "1-0", "1-0") + pair(2, "0-1", "0-1")
        _, estimate, _ = pooled(tmp_path, [halves])
        assert estimate.score == 0.5
        assert estimate.margin == 0.0
        assert estimate.los == 0.5

    def test_the_interval_narrows_as_the_pairs_pile_up(self, tmp_path):
        # the standard error goes with the square root of the number of pairs,
        # so four times the games at the same score halves the margin
        _, small, _ = pooled(tmp_path / "small", [pair(1) + drawn(2)])
        _, large, _ = pooled(tmp_path / "large", [(pair(1) + drawn(2)) * 4])
        assert small.elo == large.elo
        assert math.isclose(large.margin, small.margin / 2)

    def test_a_sweep_is_bounded_rather_than_infinite(self, tmp_path):
        # the model has no elo for a score of one, and dividing by 1 - p there
        # would be a crash rather than an answer
        _, estimate, _ = pooled(tmp_path, [pair(1) + pair(2)])
        assert estimate.bounded == "above +1200"
        assert estimate.los == 1.0
        assert str(estimate) == "above +1200 Elo (4 games)"

    def test_a_match_swept_the_other_way_is_bounded_below(self, tmp_path):
        _, estimate, _ = pooled(tmp_path, [rounds(0, 2, won=False)])
        assert estimate.bounded == "below -1200"
        assert estimate.los == 0.0

    def test_a_match_with_no_complete_pair_states_no_interval(self, tmp_path):
        _, estimate, _ = pooled(tmp_path, [game(1, "1-0")])
        assert estimate.bounded == "not measured"
        assert estimate.margin is None
        assert str(estimate) == "100.0% score (1 games)"


class TestReport:
    def test_a_row_for_every_shard_and_one_for_the_pool(self, tmp_path):
        shards, estimate, text = pooled(
            tmp_path, [drawn(1) + drawn(2), drawn(1) + pair(2)]
        )
        printed = match_estimate.report(shards, estimate, text)
        assert "| strength-1-1-shard-0 | 4 | 50.0% | 0 |" in printed
        assert "| strength-1-1-shard-1 | 4 | 75.0% | 0 |" in printed
        assert "| pooled | 8 | 62.5% | 0 |" in printed

    def test_a_shard_that_lost_games_to_a_fault_shows_in_its_row(self, tmp_path):
        forfeit = game(
            2,
            "0-1",
            termination="time forfeit",
            reason="White loses on time (102ms overrun)",
        )
        shards, estimate, text = pooled(
            tmp_path, [drawn(1) + forfeit, drawn(1) + drawn(2)]
        )
        printed = match_estimate.report(shards, estimate, text)
        assert "| strength-1-1-shard-0 | 3 | 33.3% | 1 |" in printed
        assert "| pooled | 7 | 42.9% | 1 |" in printed

    def test_the_pairs_are_counted_by_what_they_scored(self, tmp_path):
        shards, estimate, text = pooled(
            tmp_path, [drawn(1) + pair(2) + pair(3, "0-1", "1-0")]
        )
        printed = match_estimate.report(shards, estimate, text)
        assert "| pair score | 0 | 0.5 | 1 | 1.5 | 2 |" in printed
        assert "| pairs | 1 | 0 | 1 | 0 | 1 |" in printed

    def test_the_terminations_are_counted_and_not_reimplemented(self, tmp_path):
        forfeit = game(
            2,
            "0-1",
            termination="time forfeit",
            reason="White loses on time (102ms overrun)",
        )
        shards, estimate, text = pooled(tmp_path, [drawn(1) + forfeit])
        printed = match_estimate.report(shards, estimate, text)
        assert "How the games ended:" in printed
        assert "games: 3" in printed
        assert "time forfeit: 1 (new 1)" in printed

    def test_the_headline_is_the_estimate_and_the_paragraph_the_interval(
        self, tmp_path
    ):
        shards, estimate, text = pooled(tmp_path, [drawn(1) + pair(2)])
        printed = match_estimate.report(shards, estimate, text)
        assert printed.splitlines()[0] == "+191 ±321 Elo (4 games)"
        assert "The 95% interval is -130 to +512 elo" in printed
        assert "likelihood of superiority is 92.1%" in printed

    def test_the_games_left_over_are_said_and_the_unfinished_ones_too(self, tmp_path):
        shards, estimate, text = pooled(
            tmp_path, [drawn(1) + game(2, "1-0") + game(3, "*")]
        )
        printed = match_estimate.report(shards, estimate, text)
        assert "1 of the games had no partner" in printed
        assert "1 games had no result and are left out" in printed


class TestCommandLine:
    def run(self, tmp_path, texts, *arguments):
        paths = [
            shard(tmp_path, f"strength-1-1-shard-{index}", text)
            for index, text in enumerate(texts)
        ]
        return subprocess.run(
            [
                sys.executable,
                str(SCRIPT),
                *[str(path) for path in paths],
                "--candidate",
                CANDIDATE,
                "--baseline",
                BASELINE,
                "--tc",
                "30+0.3",
                *arguments,
            ],
            check=False,
            capture_output=True,
            text=True,
        )

    def test_the_report_goes_to_stdout_and_nothing_else_does(self, tmp_path):
        result = self.run(tmp_path, [drawn(1) + drawn(2), drawn(1) + drawn(2)])
        assert result.returncode == 0
        assert result.stdout.startswith("+0 ±0 Elo (8 games)")
        assert result.stderr == ""

    def test_the_line_is_the_one_the_release_notes_carry(self, tmp_path):
        result = self.run(tmp_path, [drawn(1) + drawn(2)], "--line")
        assert result.stdout == "+0 ±0 Elo (4 games)\n"

    def test_the_trailer_passes_the_hook(self, tmp_path):
        import check_trailers

        line = self.run(tmp_path, [drawn(1) + pair(2)], "--trailer").stdout
        assert line.startswith("Elo: +")
        assert line.endswith("(4 games, 30+0.3, vs old)\n")
        message = (
            "perf(search): Sort less\n\nBench: 1\n"
            "Speed: +1.0% (bench nps, 5 interleaved rounds vs a1b2c3d,"
            f" spread 1.0%)\n{line}"
        )
        assert check_trailers.problems(message) == [], line

    def test_a_match_with_no_estimate_states_none_in_the_trailer(self, tmp_path):
        line = self.run(tmp_path, [game(1, "1-0")], "--trailer").stdout
        assert line == "Elo: not measured\n"

    def test_a_fault_is_reported_on_stderr_for_the_workflow_to_raise(self, tmp_path):
        crashed = game(1, "0-1", termination="abandoned", reason="White disconnects")
        result = self.run(tmp_path, [drawn(1) + game(2, "1-0"), crashed])
        # counted and not an error: the games stay in the estimate either way
        assert result.returncode == 0
        assert "ended by a fault" in result.stderr

    def test_a_shard_with_no_games_of_its_own_does_not_stop_the_pool(self, tmp_path):
        result = self.run(tmp_path, [drawn(1), ""])
        assert result.returncode == 0
        assert "| strength-1-1-shard-1 | 0 | 0.0% | 0 |" in result.stdout

    def test_no_games_anywhere_is_an_error(self, tmp_path):
        result = self.run(tmp_path, ["", ""])
        assert result.returncode != 0
        assert "no games for new" in result.stderr
