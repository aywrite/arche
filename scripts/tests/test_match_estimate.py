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
import pytest

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


# The pentanomial counts and the log likelihood ratio the pinned fastchess
# printed for a real match: 60 games of the engine against itself at 1+0.01
# under `-sprt elo0=0 elo1=10 alpha=0.05 beta=0.05 model=logistic` with
# `-report penta=true`, which ended `Ptnml(0-2): [2, 3, 14, 3, 8]` and
# `LLR: 0.44 (14.8%) (-2.94, 2.94) [0.00, 10.00]`.
MATCH = [2, 3, 14, 3, 8]
MATCH_LLR = 0.44

# the two games of a pair, by what the candidate scored over them. It has
# white in the first and black in the second, so the results below are as the
# pgn states them rather than as the candidate reads them
PAIRS = {
    0.0: ("0-1", "1-0"),
    0.5: ("0-1", "1/2-1/2"),
    1.0: ("1/2-1/2", "1/2-1/2"),
    1.5: ("1-0", "1/2-1/2"),
    2.0: ("1-0", "0-1"),
}


def batch(counts):
    """A shard whose pairs scored what `counts` says, in PENTANOMIAL order."""
    played = []
    for score, count in zip(match_estimate.PENTANOMIAL, counts):
        for _ in range(count):
            played.append(pair(len(played) + 1, *PAIRS[score]))
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

    def test_every_pair_shared_falls_back_to_the_spread_the_model_expects(
        self, tmp_path
    ):
        # each round was won one way and lost the other, so every pair scored
        # one out of two and the variance over the pairs is nought. That is
        # no measurement of the spread rather than a measurement of none, so
        # the interval falls back to the one unrelated games would have
        halves = pair(1, "1-0", "1-0") + pair(2, "0-1", "0-1")
        _, estimate, _ = pooled(tmp_path, [halves])
        assert estimate.score == 0.5
        assert round(estimate.margin) == 340
        assert estimate.los == 0.5

    def test_the_figure_and_the_interval_read_the_same_pairs(self):
        # a drawn pair and one won game the pairing left over. Reading the
        # figure off every game and the spread off the pairs alone made that
        # +120 elo with no interval either side of it and superiority certain
        estimate = match_estimate.Estimate(2.0, 3, [1.0])
        assert estimate.score == 2 / 3
        assert estimate.paired == 0.5
        assert estimate.elo == 0.0
        assert estimate.margin > 0
        assert estimate.los == 0.5

    def test_one_pair_on_its_own_states_the_spread_it_cannot_measure(self):
        estimate = match_estimate.Estimate(1.5, 2, [1.5])
        assert round(estimate.elo) == 191
        assert round(estimate.margin) == 556
        assert 0.5 < estimate.los < 1.0

    def test_a_modelled_spread_says_it_is_one(self, tmp_path):
        # a measured ±340 and a modelled one are not the same claim, so the
        # paragraph the interval goes in says which it is
        halves = pair(1, "1-0", "1-0") + pair(2, "0-1", "0-1")
        shards, estimate, text = pooled(tmp_path, [halves])
        assert estimate.modelled
        assert "rather than from one these games showed" in match_estimate.report(
            shards, estimate, text
        )
        _, measured, _ = pooled(tmp_path / "measured", [drawn(1) + pair(2)])
        assert not measured.modelled

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
        assert result.stdout.startswith("+0 ±241 Elo (8 games)")
        assert result.stderr == ""

    def test_the_line_is_the_one_the_release_notes_carry(self, tmp_path):
        result = self.run(tmp_path, [drawn(1) + drawn(2)], "--line")
        assert result.stdout == "+0 ±340 Elo (4 games)\n"

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

    def test_the_line_carries_the_sprt_reading_after_the_estimate(self, tmp_path):
        result = self.run(
            tmp_path, [drawn(1) + pair(2)], "--line", "--elo0", "0", "--elo1", "10"
        )
        assert result.stdout == (
            "+191 ±321 Elo (4 games), SPRT [0, 10] inconclusive,"
            " LLR 0.06 (-2.94, 2.94)\n"
        )

    def test_the_sprt_trailer_names_the_verdict_and_passes_the_hook(self, tmp_path):
        import check_trailers

        line = self.run(
            tmp_path,
            [drawn(1) + pair(2)],
            "--trailer",
            "--elo0",
            "0",
            "--elo1",
            "10",
            "--prior-pairs",
            "0,0,100,0,5",
        ).stdout
        # the 105 pairs carried in and the 2 this batch played. The figure,
        # the interval and the count are all the test's: the batch on its own
        # reads +191 ±321 over 4 games, which is no claim about 214 of them
        assert line == (
            "Elo: +20 ±15 (sprt [0, 10] passed, 214 games, 30+0.3, vs old)\n"
        )
        message = (
            f"fix(search): Stop the reduction eating the last ply\n\nBench: 1\n{line}"
        )
        assert check_trailers.problems(message) == [], line

    def test_a_first_batch_is_the_whole_test_it_has(self, tmp_path):
        line = self.run(
            tmp_path, [drawn(1) + pair(2)], "--trailer", "--elo0", "0", "--elo1", "10"
        ).stdout
        assert line == (
            "Elo: +191 ±321 (sprt [0, 10] inconclusive, 4 games, 30+0.3, vs old)\n"
        )

    def test_the_trailer_counts_the_paired_games_of_a_cut_off_shard(self, tmp_path):
        # three games, so one round is a pair and the other is half of one.
        # The odd game is out of the counts the ratio is read from, so it is
        # out of the games the trailer states
        line = self.run(
            tmp_path,
            [drawn(1) + game(2, "1-0")],
            "--trailer",
            "--elo0",
            "0",
            "--elo1",
            "10",
        ).stdout
        assert line.endswith("(sprt [0, 10] inconclusive, 2 games, 30+0.3, vs old)\n")

    def test_one_hypothesis_without_the_other_is_not_a_test(self, tmp_path):
        result = self.run(tmp_path, [drawn(1)], "--elo0", "0")
        assert result.returncode != 0
        assert "both or neither" in result.stderr

    def test_a_settled_test_keeps_its_verdict_with_no_estimate_to_state(self, tmp_path):
        import check_trailers

        # every pair of the test went the same way, so the model has no elo
        # for the score. The verdict is what the batches were run for and
        # survives without one
        line = self.run(
            tmp_path,
            [pair(1) + pair(2)],
            "--trailer",
            "--elo0",
            "0",
            "--elo1",
            "10",
            "--prior-pairs",
            "0,0,0,0,110",
        ).stdout
        assert line == (
            "Elo: not measured (sprt [0, 10] passed, 224 games, 30+0.3, vs old)\n"
        )
        message = f"fix(search): Finish depth one\n\nBench: 1\n{line}"
        assert check_trailers.problems(message) == [], line

    def test_a_hypothesis_the_model_has_no_score_for_is_refused(self, tmp_path):
        # not a number bisects forever, and past a thousand elo the expected
        # score rounds to nought or one and there is no interval left to run in
        for elo0, elo1 in (("nan", "10"), ("0", "6400"), ("0", "inf")):
            result = self.run(tmp_path, [drawn(1)], "--elo0", elo0, "--elo1", elo1)
            assert result.returncode != 0
            assert "is an elo difference" in result.stderr

    def test_pairs_carried_in_that_are_not_five_counts_are_refused(self, tmp_path):
        # the counts are one per pair score, so a ratio typed into the box a
        # ratio used to go in is caught rather than read as a count
        for spec in ("1.06", "1,2,3", "1,2,3,4,5,6", "1,-2,3,4,5", "1e9,0,0,0,0"):
            result = self.run(
                tmp_path,
                [drawn(1)],
                "--elo0",
                "0",
                "--elo1",
                "10",
                "--prior-pairs",
                spec,
            )
            assert result.returncode != 0
            assert "--prior-pairs is a count for each pair score" in result.stderr

    def test_pairs_carried_in_with_no_test_to_carry_them_are_refused(self, tmp_path):
        result = self.run(tmp_path, [drawn(1)], "--prior-pairs", "1,2,3,4,5")
        assert result.returncode != 0
        assert "wants --elo0 and --elo1" in result.stderr

    def test_hypotheses_the_wrong_way_round_are_refused(self, tmp_path):
        # the ratio of a test whose ends meet is nought whatever the games
        # did, so every batch of it would ask for another one
        result = self.run(tmp_path, [drawn(1)], "--elo0", "10", "--elo1", "0")
        assert result.returncode != 0
        assert "--elo0 10 is not below --elo1 0" in result.stderr

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


class TestSequential:
    """The sprt reading, pinned against the numbers fastchess itself prints.

    The two cases below the first are fastchess's own, from
    `app/tests/sprt_test.cpp` at the pinned tag. Its `Stats` holds the counts
    as (LL, LD, WL, DD, WD, WW) and its test merges WL with DD into the middle
    bin, which is the bin a shared pair and a doubly drawn one share here. It
    runs them at alpha and beta of 0.05, the same as ours, though the bounds
    do not enter the ratio."""

    def test_a_real_match_reads_as_the_ratio_fastchess_printed(self):
        llr = match_estimate.log_likelihood_ratio(MATCH, 0, 10)
        assert abs(llr - MATCH_LLR) < 0.01

    def test_the_pairs_of_that_match_read_back_out_of_a_pgn(self, tmp_path):
        # the same counts through the pairing, so a reader that put a pair in
        # the wrong bin would fail here rather than in the arithmetic
        shards, _, _ = pooled(tmp_path, [batch(MATCH)])
        pairs = [score for one in shards for score in one.pairs]
        sprt = match_estimate.Sprt(pairs, 0, 10)
        assert sprt.counts == MATCH
        assert abs(sprt.llr - MATCH_LLR) < 0.01

    def test_the_first_of_fastchess_own_logistic_cases(self):
        llr = match_estimate.log_likelihood_ratio(
            [223, 9863, 21279, 10037, 246], 0.5, 2.5
        )
        assert abs(llr - -3.07) < 0.01

    def test_the_second_of_fastchess_own_logistic_cases(self):
        llr = match_estimate.log_likelihood_ratio([871, 26175, 55983, 26678, 821], 0, 2)
        assert abs(llr - -4.98) < 0.01

    def test_the_ratio_flips_when_the_match_and_the_question_both_do(self):
        # swapping the wins for the losses is the same match from the other
        # side, and negating and swapping the hypotheses is the same question
        # asked of that side, so the evidence has to read the other way round
        counts = [3, 11, 42, 17, 7]
        assert math.isclose(
            match_estimate.log_likelihood_ratio(counts, 0, 10),
            -match_estimate.log_likelihood_ratio(list(reversed(counts)), -10, 0),
        )

    def test_a_score_no_pair_reached_is_not_divided_by(self):
        # a short batch can easily have none of a score, and a count of nought
        # has no logarithm, so it is nudged off nought as fastchess does
        assert match_estimate.log_likelihood_ratio([0, 0, 4, 0, 0], 0, 10) < 0
        assert match_estimate.log_likelihood_ratio([0, 0, 0, 0, 4], 0, 10) > 0
        assert match_estimate.Sprt([], 0, 10).llr == 0.0

    def test_a_batch_carries_the_pairs_it_played_to_the_next_one(self):
        before = [2, 3, 14, 3, 8]
        carried = match_estimate.Sprt([2.0, 1.0], 0, 10, before)
        assert carried.batch == [0, 0, 1, 0, 1]
        assert carried.counts == [2, 3, 15, 3, 9]
        assert carried.carried == "2,3,15,3,9"
        assert math.isclose(
            carried.llr, match_estimate.log_likelihood_ratio(carried.counts, 0, 10)
        )

    def test_the_ratio_is_worked_out_over_the_test_and_not_added_up(self):
        # the fit is to the pairs the ratio is read against, so each batch
        # fitting its own distribution and the ratios then being added is a
        # different statistic from the one the pairs together give. Two
        # batches that disagree with each other show how far apart: a fifth
        # of the pairs shared and the rest split evenly between a sweep each
        # way reads as -0.03 pooled and -0.59 added
        first, second = [10, 0, 0, 0, 10], [0, 0, 20, 0, 0]
        together = [one + other for one, other in zip(first, second)]
        added = sum(
            match_estimate.log_likelihood_ratio(counts, 0, 10)
            for counts in (first, second)
        )
        sprt = match_estimate.Sprt([], 0, 10, together)
        assert math.isclose(
            sprt.llr, match_estimate.log_likelihood_ratio(together, 0, 10)
        )
        assert abs(sprt.llr - added) > 0.5

    def test_a_prior_that_is_not_one_count_per_score_is_refused(self):
        # every caller reaching Sprt has five counts, and a shorter list would
        # otherwise be zipped down to its own length and silently drop a bin
        with pytest.raises(ValueError):
            match_estimate.Sprt([2.0], 0, 10, [1, 2, 3])

    def test_more_pairs_than_a_match_plays_are_refused(self):
        # the fit bisects between two bounds set by the counts, and counts
        # this far apart put the root on a bound and the division by nought
        with pytest.raises(ValueError):
            match_estimate.read_prior("100000000,0,0,0,0")
        assert match_estimate.read_prior("99999999,0,0,0,0")[0] == 99999999

    def test_the_bounds_are_where_the_verdict_turns_over(self):
        # the pairs of the whole test are what the ratio is read from, so the
        # verdict turns over on the pair that carries the ratio past a bound
        def verdict(counts):
            return match_estimate.Sprt([], 0, 10, counts).verdict

        assert verdict([0, 0, 100, 0, 5]) == "inconclusive"
        assert verdict([0, 0, 100, 0, 6]) == "passed"
        assert verdict([0, 0, 100, 0, 0]) == "inconclusive"
        assert verdict([1, 0, 100, 0, 0]) == "failed"

    def test_the_report_states_the_ratio_the_pairs_of_the_test_give(self, tmp_path):
        shards, estimate, text = pooled(tmp_path, [drawn(1) + pair(2)])
        pairs = [score for one in shards for score in one.pairs]
        sprt = match_estimate.Sprt(pairs, 0, 10, [1, 0, 1, 0, 0])
        printed = match_estimate.report(shards, estimate, text, sprt)
        assert "SPRT [0, 10] inconclusive." in printed
        assert (
            "over the 4 pairs of the test (2 from this batch and 2 from the"
            " batches before it) is -0.00 against bounds of (-2.94, 2.94)" in printed
        )
        assert "Launch another batch with prior_pairs set to 1,0,2,0,1." in printed

    def test_a_test_that_settled_says_what_it_settled(self, tmp_path):
        shards, estimate, text = pooled(tmp_path, [drawn(1) + pair(2)])
        pairs = [score for one in shards for score in one.pairs]
        printed = match_estimate.report(
            shards, estimate, text, match_estimate.Sprt(pairs, 0, 10, [0, 0, 100, 0, 5])
        )
        assert "SPRT [0, 10] passed." in printed
        assert "stronger by about 10 elo or more" in printed
