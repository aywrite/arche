# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2022-2026 Andrew Wright

"""Tests for the instruction count comparison.

A fake valgrind stands in for cachegrind. It prints the bench line and the
summary cachegrind would for whichever binary it was handed, reading both
from a file beside that binary's path, so what is under test is the reading
of the two outputs and the report.
"""

import sys

import instructions
import pytest


def fake_valgrind(directory):
    body = (
        "import pathlib, sys\n"
        "binary = pathlib.Path(sys.argv[sys.argv.index('bench') - 1])\n"
        "refs, nodes = binary.with_suffix('.counts').read_text().split()\n"
        "if 'games' in sys.argv:\n"
        "    nodes = str(int(nodes) * 2)\n"
        "print('bench depth 1 hash 16MB positions 1')\n"
        "print(nodes + ' nodes 1000 nps')\n"
        "print('==123== I refs:        ' + refs, file=sys.stderr)\n"
    )
    script = directory / "valgrind.py"
    script.write_text(body)
    if sys.platform == "win32":
        runner = directory / "valgrind.cmd"
        runner.write_text(f'@"{sys.executable}" "{script}" %*\r\n')
    else:
        runner = directory / "valgrind"
        runner.write_text(f"#!{sys.executable}\n{body}")
        runner.chmod(0o755)
    return runner


def engine(directory, name, refs, nodes):
    """A binary path the fake valgrind will answer for. Nothing runs it."""
    (directory / f"{name}.counts").write_text(f"{refs} {nodes}\n")
    return directory / f"{name}.bin"


def test_the_summary_is_read_with_its_separators():
    stderr = "==9== Cachegrind\n==9== \n==9== I refs:        12,332,829,881\n"
    assert instructions.instructions_in(stderr) == 12_332_829_881
    assert instructions.instructions_in("==9== no summary\n") is None


def test_the_last_line_of_the_bench_is_read_for_its_nodes():
    assert instructions.nodes_in("...\n6900228 nodes 91574 nps\n") == 6900228
    assert instructions.nodes_in("uciok\n") is None


def test_a_change_with_the_counts_held_leaves_the_nodes_cell_empty(tmp_path, capsys):
    valgrind = fake_valgrind(tmp_path)
    base = engine(tmp_path, "base", "1,000,000", 1000)
    candidate = engine(tmp_path, "candidate", "997,000", 1000)
    argv = [str(base), str(candidate), "--valgrind", str(valgrind), "--base-ref", "a1"]
    assert instructions.main(argv) == 0
    out = capsys.readouterr().out
    assert "instructions against a1, one cachegrind run a side, bench" in out
    assert "base          1,000,000   1000    1000.0" in out
    assert "candidate       997,000   1000     997.0" in out
    assert "change           -0.30%           -0.30%" in out


def test_differing_counts_split_the_change_into_nodes_and_cost(tmp_path, capsys):
    # a tenth fewer nodes at a tenth more a node leaves the total nearly
    # where it was, and only the two columns together say so
    valgrind = fake_valgrind(tmp_path)
    base = engine(tmp_path, "base", "1,000,000", 1000)
    candidate = engine(tmp_path, "candidate", "990,000", 900)
    argv = [str(base), str(candidate), "--valgrind", str(valgrind)]
    assert instructions.main(argv) == 0
    out = capsys.readouterr().out
    assert "change           -1.00%  -10.00%   +10.00%" in out


def test_the_verdict_holds_the_change_against_an_incidental_edit(tmp_path, capsys):
    valgrind = fake_valgrind(tmp_path)
    base = engine(tmp_path, "base", "1,000,000", 1000)
    within = engine(tmp_path, "within", "994,000", 1000)
    beyond = engine(tmp_path, "beyond", "990,000", 1000)
    assert instructions.main([str(base), str(within), "--valgrind", str(valgrind)]) == 0
    assert "within ±0.7%" in capsys.readouterr().out
    assert instructions.main([str(base), str(beyond), "--valgrind", str(valgrind)]) == 0
    assert "fewer instructions, beyond the ±0.7%" in capsys.readouterr().out


def test_the_band_is_inclusive_and_a_rise_is_named():
    base = instructions.Counted(1_000_000, 1000)
    edge = instructions.verdict(base, instructions.Counted(1_007_000, 1000))
    assert edge.startswith("within")
    rise = instructions.verdict(base, instructions.Counted(1_008_000, 1000))
    assert rise.startswith("more instructions")


def test_the_verdict_stands_aside_when_the_trees_differ():
    said = instructions.verdict(
        instructions.Counted(1_000_000, 1000), instructions.Counted(900_000, 900)
    )
    assert said.startswith("the trees differ")


def test_a_run_with_no_summary_is_named_rather_than_a_traceback(tmp_path):
    quiet = tmp_path / ("quiet.cmd" if sys.platform == "win32" else "quiet")
    quiet.write_text("@echo off\r\n" if sys.platform == "win32" else "#!/bin/sh\n")
    quiet.chmod(0o755)
    with pytest.raises(SystemExit) as left:
        instructions.count(str(quiet), "engine", None)
    assert "no instruction count" in str(left.value)


def test_games_asks_each_binary_for_the_games_suite(tmp_path, capsys):
    # the fake doubles the nodes when it is handed `games`, so the counts
    # show which suite each side was asked for
    valgrind = fake_valgrind(tmp_path)
    base = engine(tmp_path, "base", "1,000,000", 1000)
    candidate = engine(tmp_path, "candidate", "990,000", 1000)
    argv = [str(base), str(candidate), "--games", "--valgrind", str(valgrind)]
    assert instructions.main(argv) == 0
    out = capsys.readouterr().out
    assert "one cachegrind run a side, bench games" in out
    assert "base          1,000,000   2000     500.0" in out


def test_a_depth_and_the_games_suite_are_refused_together(tmp_path):
    with pytest.raises(SystemExit):
        instructions.main(["base", "candidate", "--games", "--depth", "3"])


def test_a_binary_that_refuses_the_bench_is_named_rather_than_a_traceback(tmp_path):
    refusing = tmp_path / ("refusing.cmd" if sys.platform == "win32" else "refusing")
    if sys.platform == "win32":
        refusing.write_text(
            "@echo off\r\necho unrecognised bench depth: games 1>&2\r\nexit /b 2\r\n"
        )
    else:
        refusing.write_text(
            "#!/bin/sh\necho 'unrecognised bench depth: games' >&2\nexit 2\n"
        )
    refusing.chmod(0o755)
    with pytest.raises(SystemExit) as left:
        instructions.count(str(refusing), "engine", None, games=True)
    assert "exited 2" in str(left.value)
    assert "depth: games" in str(left.value)
