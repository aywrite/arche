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


# the tail of a real run under -sprt: the LLR line is only printed then, and
# the verdict line only once the log likelihood ratio crosses a bound
SPRT_H0 = """\
--------------------------------------------------
Results of new vs old (1+0.01 - 5 plies, NULL, NULL):
Elo: -inf +/- -nan, nElo: -inf +/- -nan
LOS: 0.00 %, DrawRatio: 0.00 %, PairsRatio: 0.00
Games: 142, Wins: 0, Losses: 142, Draws: 0, Points: 0.0 (0.00 %)
Ptnml(0-2): [71, 0, 0, 0, 0], WL/DD Ratio: -nan
LLR: -2.95 (-100.1%) (-2.94, 2.94) [0.00, 10.00]
--------------------------------------------------
SPRT ([0.00, 10.00]) completed - H0 was accepted
"""

# the same run stopped by a cap instead: fastchess reports the games played
# and exits without a verdict line
SPRT_CAPPED = """\
--------------------------------------------------
Results of new vs old (1+0.01, NULL, NULL):
Elo: 58.45 +/- 81.32, nElo: 65.66 +/- 87.91
LOS: 92.84 %, DrawRatio: 53.33 %, PairsRatio: 2.50
Games: 60, Wins: 34, Losses: 24, Draws: 2, Points: 35.0 (58.33 %)
Ptnml(0-2): [4, 0, 16, 2, 8], WL/DD Ratio: inf
LLR: 0.31 (10.6%) (-2.94, 2.94) [0.00, 10.00]
--------------------------------------------------
Tournament was interrupted. To resume the tournament, run: ./fastchess -config file=config.json
"""


def test_an_accepted_h1_reads_as_stronger(tmp_path):
    accepted = SPRT_CAPPED.replace(
        "Tournament was interrupted. To resume the tournament, run: ./fastchess -config file=config.json",
        "SPRT ([0.00, 10.00]) completed - H1 was accepted",
    )
    result = run(tmp_path, accepted)
    assert result.stdout == (
        "+58 ±81 Elo (60 games), SPRT [0.00, 10.00] accepted H1: stronger\n"
    )


def test_an_accepted_h0_reads_as_not_stronger(tmp_path):
    # a sweep as well as a verdict: every game pair went the same way, so
    # fastchess prints the elo as inf and only the score can be reported
    result = run(tmp_path, SPRT_H0)
    assert result.stdout == (
        "0.00% score (142 games), SPRT [0.00, 10.00] accepted H0: not stronger\n"
    )


def test_a_capped_sprt_match_is_inconclusive(tmp_path):
    result = run(tmp_path, SPRT_CAPPED)
    assert result.stdout == (
        "+58 ±81 Elo (60 games), SPRT [0.00, 10.00] inconclusive\n"
    )


def trailer(tmp_path, text, tc="10+0.1", baseline="v0.3.10"):
    result_file = tmp_path / "result.txt"
    result_file.write_text(text)
    return subprocess.run(
        [
            sys.executable,
            str(SCRIPT),
            str(result_file),
            "--trailer",
            "--tc",
            tc,
            "--baseline",
            baseline,
        ],
        check=False,
        capture_output=True,
        text=True,
    )


def test_a_fixed_match_gives_the_elo_trailer(tmp_path):
    result = trailer(tmp_path, RESULT)
    assert result.stdout == "Elo: +7 ±45 (50 games, 10+0.1, vs v0.3.10)\n"


def test_an_sprt_match_names_its_bounds_and_verdict_in_the_trailer(tmp_path):
    # the verdict is the point of an sprt: an estimate of +58 with the
    # test failed and one with it passed are not the same claim, and the
    # history has to be able to tell them apart without the log
    interrupted = (
        "Tournament was interrupted. To resume the tournament, "
        "run: ./fastchess -config file=config.json"
    )
    for ending, verdict in [
        (interrupted, "inconclusive"),
        ("SPRT ([0.00, 10.00]) completed - H1 was accepted", "passed"),
        ("SPRT ([0.00, 10.00]) completed - H0 was accepted", "failed"),
    ]:
        text = SPRT_CAPPED.replace(interrupted, ending)
        result = trailer(tmp_path, text, tc="1+0.01", baseline="master")
        assert result.stdout == (
            f"Elo: +58 ±81 (sprt [0, 10] {verdict}, 60 games, 1+0.01, vs master)\n"
        ), verdict


def test_a_sweep_has_no_elo_to_state(tmp_path):
    # fastchess prints inf, and a trailer claiming a number it never gave
    # would be a lie the hook could not catch
    result = trailer(tmp_path, SPRT_H0)
    assert result.stdout == "Elo: not measured\n"


def test_the_trailer_passes_the_hook(tmp_path):
    import check_trailers

    # the bounds are workflow inputs and need not be whole
    decimals = SPRT_CAPPED.replace("[0.00, 10.00]", "[-1.50, 2.50]")
    for text in [RESULT, SPRT_CAPPED, SPRT_H0, decimals]:
        line = trailer(tmp_path, text).stdout
        message = f"perf(search): Sort less\n\nBench: 1\nSpeed: +1.0% (bench nps, 5 interleaved rounds vs a1b2c3d, spread 1.0%)\n{line}"
        assert check_trailers.problems(message) == [], line


def test_a_fixed_match_gets_no_sprt_annotation(tmp_path):
    # the LLR line is the marker of an sprt run, a plain match must not grow
    # a verdict
    result = run(tmp_path, RESULT)
    assert "SPRT" not in result.stdout
