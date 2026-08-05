"""Tests for the one line match summary.

The input is fastchess's result block, so the fixtures are copies of real
output: what this guards against is the parse quietly matching the wrong
number when that format shifts.
"""

import subprocess
import sys
from pathlib import Path

import match_summary

SCRIPT = Path(match_summary.__file__)

# the shape fastchess prints after every round, tail of a real run
RESULT = """\
--------------------------------------------------
Results of new vs old (10+0.1, NULL, NULL, 8moves_v3.pgn):
Elo: 6.95 +/- 45.36, nElo: 14.84 +/- 96.30
LOS: 61.87 %, DrawRatio: 68.00 %, PairsRatio: 1.00
Games: 50, Wins: 15, Losses: 14, Draws: 21, Points: 25.5 (51.00 %)
Ptnml(0-2): [0, 4, 17, 3, 1], WL/DD Ratio: 1.43
--------------------------------------------------
"""


def run(tmp_path, text):
    result_file = tmp_path / "result.txt"
    result_file.write_text(text)
    return subprocess.run(
        [sys.executable, str(SCRIPT), str(result_file)],
        check=False,
        capture_output=True,
        text=True,
    )


def test_the_elo_line_is_read_and_rounded(tmp_path):
    result = run(tmp_path, RESULT)
    assert result.returncode == 0
    assert result.stdout == "+7 ±45 Elo (50 games)\n"


def test_the_elo_figure_is_not_confused_with_nelo(tmp_path):
    # nElo sits on the same line with a much larger margin, the lookbehind is
    # what keeps the parse off it
    assert match_summary.ELO.findall(RESULT) == [("6.95", "45.36")]


def test_the_last_block_wins(tmp_path):
    # fastchess prints an interim block every few games, only the final one
    # describes the whole match
    interim = RESULT.replace("6.95 +/- 45.36", "-40.00 +/- 80.00").replace(
        "Games: 50", "Games: 10"
    )
    result = run(tmp_path, interim + RESULT)
    assert result.stdout == "+7 ±45 Elo (50 games)\n"


def test_a_losing_result_keeps_its_sign(tmp_path):
    result = run(tmp_path, RESULT.replace("6.95", "-12.60"))
    assert result.stdout == "-13 ±45 Elo (50 games)\n"


def test_output_without_a_result_block_fails_rather_than_invents(tmp_path):
    result = run(tmp_path, "fastchess crashed before printing anything\n")
    assert result.returncode != 0
