"""Tests for the gauntlet rating estimate.

The fixed points here are chess statistics rather than implementation detail:
the logistic model says what score a rating difference earns, so the fit can be
checked against numbers computed by hand.
"""

import subprocess
import sys
from pathlib import Path

import pytest
import rating_estimate

SCRIPT = Path(rating_estimate.__file__)


def game(white, black, result):
    """One pgn game record, shaped the way fastchess writes them."""
    return (
        f'[Event "gauntlet"]\n'
        f'[White "{white}"]\n'
        f'[Black "{black}"]\n'
        f'[Result "{result}"]\n'
        f"\n1. e4 e5 {result}\n\n"
    )


class TestExpectedAndImplied:
    def test_equal_ratings_expect_an_even_score(self):
        assert rating_estimate.expected(1600, 1600) == 0.5

    def test_the_stronger_side_expects_more(self):
        assert rating_estimate.expected(1700, 1600) > 0.5
        assert rating_estimate.expected(1500, 1600) < 0.5

    def test_an_even_score_implies_the_opponent_rating(self):
        assert rating_estimate.implied(1600, 5, 10) == pytest.approx(1600)

    def test_three_quarters_implies_the_textbook_gap(self):
        # 75% is the logistic model's worked example: 400 * log10(3) ≈ 191
        assert rating_estimate.implied(1600, 7.5, 10) == pytest.approx(1790.85, abs=0.1)

    def test_a_sweep_is_capped_rather_than_infinite(self):
        assert rating_estimate.implied(1600, 10, 10) == 1600 + 1200
        assert rating_estimate.implied(1600, 0, 10) == 1600 - 1200


class TestReadLadder:
    def test_names_and_ratings_are_parsed(self):
        assert rating_estimate.read_ladder("a:1500,b:1600") == {"a": 1500, "b": 1600}

    def test_whitespace_and_empty_entries_are_tolerated(self):
        assert rating_estimate.read_ladder(" a:1500 , ,b:1600,") == {
            "a": 1500,
            "b": 1600,
        }

    def test_a_name_containing_a_colon_splits_on_the_last_one(self):
        assert rating_estimate.read_ladder("stash:v10:1620") == {"stash:v10": 1620}

    def test_malformed_entries_stop_the_run(self):
        for spec in ["", "1600", "name:", "name:notanumber"]:
            with pytest.raises(SystemExit):
                rating_estimate.read_ladder(spec)


class TestFit:
    def test_an_even_score_fits_the_opponent_rating(self):
        estimate, note = rating_estimate.fit([("a", 1600, 10, 0, 10)])
        assert estimate.rating == pytest.approx(1600, abs=0.5)
        assert note == ""

    def test_three_quarters_fits_the_textbook_gap(self):
        estimate, _ = rating_estimate.fit([("a", 1600, 15, 0, 5)])
        assert estimate.rating == pytest.approx(1790.85, abs=0.5)
        assert 0 < estimate.margin < 400

    def test_a_sweep_is_reported_as_a_bound_not_a_number(self):
        estimate, _ = rating_estimate.fit([("a", 1600, 10, 0, 0), ("b", 1700, 4, 0, 0)])
        assert str(estimate) == "above 1700 on the ccrl blitz scale (14 games)"

    def test_losing_every_game_bounds_from_below(self):
        estimate, _ = rating_estimate.fit([("a", 1600, 0, 0, 10), ("b", 1500, 0, 0, 4)])
        assert str(estimate) == "below 1500 on the ccrl blitz scale (14 games)"

    def test_all_draws_still_carry_a_margin(self):
        # every game alike leaves no observed spread, the fallback to the
        # modelled spread is what keeps the margin from reading as zero doubt
        estimate, _ = rating_estimate.fit([("a", 1600, 0, 20, 0)])
        assert estimate.rating == pytest.approx(1600, abs=0.5)
        assert estimate.margin > 0

    def test_draws_shrink_the_margin_decisive_games_do_not(self):
        # both scored 50%, but a drawish 50% wobbles less than a decisive one,
        # which is the point of using the observed variance over the modelled
        drawish, _ = rating_estimate.fit([("a", 1600, 5, 10, 5)])
        decisive, _ = rating_estimate.fit([("a", 1600, 10, 0, 10)])
        assert drawish.margin < decisive.margin

    def test_opponents_that_disagree_are_called_out(self):
        _, note = rating_estimate.fit([("a", 1600, 10, 0, 0), ("b", 1600, 0, 0, 10)])
        assert "disagree" in note

    def test_consistent_opponents_raise_no_note(self):
        _, note = rating_estimate.fit([("a", 1600, 6, 0, 4), ("b", 1700, 4, 0, 6)])
        assert note == ""

    def test_the_chi_square_cutoff_matches_the_table(self):
        # 3.841 at one degree of freedom; the approximation is allowed a
        # couple of percent, which is what its docstring promises
        assert rating_estimate.chi_square_95(1) == pytest.approx(3.841, rel=0.03)


LADDER = {"stash-v11": 1690.0, "stash-v12": 1886.0}


class TestReadPairings:
    def read(self, tmp_path, text):
        pgn = tmp_path / "gauntlet.pgn"
        pgn.write_text(text)
        return rating_estimate.read_pairings(pgn, "arche", dict(LADDER))

    def test_results_are_tallied_from_both_colours(self, tmp_path, capsys):
        pairings = self.read(
            tmp_path,
            game("arche", "stash-v11", "1-0")
            + game("stash-v11", "arche", "1-0")
            + game("arche", "stash-v11", "1/2-1/2")
            + game("stash-v12", "arche", "0-1"),
        )
        assert pairings == [
            ("stash-v11", 1690.0, 1, 1, 1),
            ("stash-v12", 1886.0, 1, 0, 0),
        ]
        assert capsys.readouterr().err == ""

    def test_games_between_other_engines_are_ignored(self, tmp_path, capsys):
        pairings = self.read(
            tmp_path,
            game("stash-v11", "stash-v12", "1-0") + game("arche", "stash-v11", "1-0"),
        )
        assert pairings == [("stash-v11", 1690.0, 1, 0, 0)]

    def test_a_rung_with_no_games_is_dropped_and_said(self, tmp_path, capsys):
        pairings = self.read(tmp_path, game("arche", "stash-v11", "1-0"))
        assert [name for name, *_ in pairings] == ["stash-v11"]
        assert "stash-v12 played no games" in capsys.readouterr().err

    def test_an_unfinished_game_is_counted_out_loud_not_fitted(self, tmp_path, capsys):
        pairings = self.read(
            tmp_path,
            game("arche", "stash-v11", "*") + game("arche", "stash-v11", "1-0"),
        )
        assert pairings == [("stash-v11", 1690.0, 1, 0, 0)]
        assert "1 game with no result" in capsys.readouterr().err

    def test_a_truncated_game_cannot_borrow_the_next_result(self, tmp_path, capsys):
        # a game cut off before its result tag once paired its players with
        # the following game's result, which dropped a rung from the fit
        truncated = (
            '[Event "gauntlet"]\n[White "arche"]\n[Black "stash-v11"]\n\n1. e4\n\n'
        )
        pairings = self.read(tmp_path, truncated + game("arche", "stash-v12", "1-0"))
        assert pairings == [("stash-v12", 1886.0, 1, 0, 0)]
        assert "1 game with no result" in capsys.readouterr().err


class TestCommandLine:
    def run(self, tmp_path, text, *args):
        pgn = tmp_path / "gauntlet.pgn"
        pgn.write_text(text)
        return subprocess.run(
            [sys.executable, str(SCRIPT), str(pgn), "arche", "stash-v11:1690", *args],
            check=False,
            capture_output=True,
            text=True,
        )

    GAMES = (
        game("arche", "stash-v11", "1-0")
        + game("stash-v11", "arche", "0-1")
        + game("arche", "stash-v11", "1/2-1/2")
        + game("stash-v11", "arche", "1-0")
    )

    def test_the_table_and_the_estimate_are_printed(self, tmp_path):
        result = self.run(tmp_path, self.GAMES)
        assert result.returncode == 0
        assert "| stash-v11 | 1690 | 2-1-1 | 62.5% |" in result.stdout
        assert "on the ccrl blitz scale (4 games)" in result.stdout

    def test_line_mode_prints_the_estimate_alone(self, tmp_path):
        result = self.run(tmp_path, self.GAMES, "--line")
        assert result.returncode == 0
        assert result.stdout.count("\n") == 1
        assert "on the ccrl blitz scale (4 games)" in result.stdout

    def test_no_games_for_the_engine_is_an_error(self, tmp_path):
        result = self.run(tmp_path, game("a", "b", "1-0"))
        assert result.returncode != 0
        assert "no games for arche" in result.stderr
