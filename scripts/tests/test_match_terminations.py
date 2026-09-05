# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2022-2026 Andrew Wright

"""Tests for the count of how the games ended.

The fixtures are the shapes the pinned fastchess writes, copied from real
games: the tag it files an ending under and the wording it ends the last
comment with. What this guards against is a later release moving one of them
and the count reading normal games where there were crashes.
"""

import subprocess
import sys
from pathlib import Path

import match_terminations

SCRIPT = Path(match_terminations.__file__)

# The comment fastchess puts on the last move, with everything the workflow
# asks it to track. The reason is the tail of it.
PLAYED = (
    "{+0.15/5 0.010s, tl=2.054s, latency=0.000s, n=83485, sd=20, nps=8348500,"
    ' hashfull=0, pv="e1g1 e8g8 c3e2 e7f6 a2a4"'
)

# the reasons, as fastchess words them, with the colour left to fill in
MATES = "{} mates"
FORFEIT = "{} loses on time (102ms overrun)"
CRASH = "{} disconnects"
STALL = "{}'s connection stalls"
ILLEGAL = "{} makes an illegal move"
ADJUDICATED = "{} wins by adjudication"


def game(termination="normal", reason=MATES, colour="White", result="1-0", swap=False):
    """One pgn game record, shaped the way fastchess writes them. new plays
    white unless the sides are swapped."""
    white, black = ("old", "new") if swap else ("new", "old")
    return (
        f'[Event "Fastchess Tournament"]\n'
        f'[White "{white}"]\n'
        f'[Black "{black}"]\n'
        f'[Result "{result}"]\n'
        f'[PlyCount "42"]\n'
        f'[Termination "{termination}"]\n'
        f"\n1. e4 {{book}} e5 {{book}} 2. Nf3 {PLAYED},"
        f" {reason.format(colour)}}} {result}\n\n"
    )


count = match_terminations.count


class TestKind:
    def test_a_termination_is_filed_under_its_own_word(self):
        assert match_terminations.kind("normal", "White mates") == "normal"
        assert match_terminations.kind("time forfeit", "") == "time forfeit"

    def test_abandoned_is_split_by_what_the_reason_says(self):
        # one tag for a crash and for an engine that went quiet, which are not
        # the same fault and are not looked into the same way
        assert match_terminations.kind("abandoned", "Black disconnects") == "disconnect"
        stalled = match_terminations.kind("abandoned", "White's connection stalls")
        assert stalled == "stall"

    def test_a_game_with_no_termination_tag_is_still_counted(self):
        assert match_terminations.kind("", "") == "unrecorded"


class TestCount:
    def test_normal_games_are_counted_and_blamed_on_nobody(self):
        totals, blamed = count(game() + game(reason="Draw by 3-fold repetition"))
        assert totals["normal"] == 2
        assert blamed == {}

    def test_a_time_forfeit_names_the_engine_that_ran_out(self):
        totals, blamed = count(game("time forfeit", FORFEIT, "Black"))
        assert totals["time forfeit"] == 1
        assert blamed["time forfeit"] == {"old": 1}

    def test_the_colour_is_read_through_the_tags_not_assumed(self):
        # the same engine forfeits once from each side, and both go on its name
        totals, blamed = count(
            game("time forfeit", FORFEIT, "White", "0-1")
            + game("time forfeit", FORFEIT, "Black", swap=True)
        )
        assert totals["time forfeit"] == 2
        assert blamed["time forfeit"] == {"new": 2}

    def test_a_crash_and_a_stall_are_told_apart(self):
        totals, blamed = count(
            game("abandoned", CRASH, "Black") + game("abandoned", STALL, "White", "0-1")
        )
        assert totals["disconnect"] == 1
        assert totals["stall"] == 1
        assert blamed["disconnect"] == {"old": 1}
        assert blamed["stall"] == {"new": 1}

    def test_an_illegal_move_is_counted_against_its_player(self):
        _, blamed = count(game("illegal move", ILLEGAL, "Black"))
        assert blamed["illegal move"] == {"old": 1}

    def test_an_adjudication_is_counted_but_not_blamed(self):
        # the reason names the winner, which is not somebody the ending fell on
        totals, blamed = count(
            game("adjudication", ADJUDICATED)
            + game("adjudication", "Draw by adjudication", result="1/2-1/2")
        )
        assert totals["adjudication"] == 2
        assert blamed == {}

    def test_a_game_the_match_was_stopped_in_is_unterminated(self):
        totals, _ = count(game("unterminated", "Game interrupted", result="*"))
        assert totals["unterminated"] == 1

    def test_a_record_with_no_players_is_not_a_game(self):
        totals, _ = count('[Event "Fastchess Tournament"]\n[Site "?"]\n\n*\n\n')
        assert sum(totals.values()) == 0

    def test_a_truncated_game_cannot_borrow_the_next_reason(self):
        # a game cut off before its tags once took the following game's, which
        # would report a crash against whichever engine played next
        truncated = '[Event "Fastchess Tournament"]\n[White "new"]\n\n1. e4\n\n'
        totals, blamed = count(truncated + game("abandoned", CRASH, "Black"))
        assert sum(totals.values()) == 1
        assert blamed["disconnect"] == {"old": 1}


class TestBlock:
    def test_every_ending_is_printed_including_the_empty_ones(self):
        printed = match_terminations.block(*count(game()))
        assert printed.splitlines() == [
            "games: 1",
            "normal: 1",
            "adjudication: 0",
            "time forfeit: 0",
            "disconnect: 0",
            "stall: 0",
            "illegal move: 0",
            "unterminated: 0",
        ]

    def test_who_it_fell_on_sits_beside_the_count(self):
        printed = match_terminations.block(
            *count(
                game("time forfeit", FORFEIT, "Black")
                + game("time forfeit", FORFEIT, "Black", swap=True)
            )
        )
        assert "time forfeit: 2 (new 1, old 1)" in printed

    def test_a_word_the_list_does_not_know_is_printed_anyway(self):
        # a later fastchess spelling an ending some other way should be counted
        # and shown rather than quietly folded in with the normal games
        printed = match_terminations.block(*count(game("resigned", "Black resigns")))
        assert "resigned: 1" in printed


class TestRemark:
    def test_a_clean_match_says_nothing(self):
        assert match_terminations.remark(*count(game())) == ""

    def test_adjudications_alone_are_not_a_fault(self):
        adjudicated = count(game("adjudication", ADJUDICATED))
        assert match_terminations.remark(*adjudicated) == ""

    def test_the_faults_are_listed_with_the_engines_they_fell_on(self):
        counted = count(
            game()
            + game("time forfeit", FORFEIT, "Black")
            + game("abandoned", CRASH, "White", "0-1")
        )
        assert match_terminations.remark(*counted) == (
            "2 of 3 games ended by a fault and not by play:"
            " 1 by time forfeit (old 1), 1 by disconnect (new 1)"
        )


class TestCommandLine:
    def run(self, tmp_path, text):
        pgn = tmp_path / "games.pgn"
        pgn.write_text(text)
        return subprocess.run(
            [sys.executable, str(SCRIPT), str(pgn)],
            check=False,
            capture_output=True,
            text=True,
        )

    def test_the_counts_go_to_stdout_and_nothing_else_does(self, tmp_path):
        result = self.run(tmp_path, game())
        assert result.returncode == 0
        assert result.stdout.startswith("games: 1\n")
        assert result.stderr == ""

    def test_a_fault_is_reported_on_stderr_for_the_workflow_to_raise(self, tmp_path):
        result = self.run(tmp_path, game("abandoned", CRASH, "Black"))
        # counted and not an error: the games stay in the result either way
        assert result.returncode == 0
        assert "disconnect: 1 (old 1)" in result.stdout
        assert "1 of 1 games ended by a fault" in result.stderr
