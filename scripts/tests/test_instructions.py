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
    assert "instructions against a1, one cachegrind run a side" in out
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


def test_a_run_with_no_summary_is_named_rather_than_a_traceback(tmp_path):
    quiet = tmp_path / ("quiet.cmd" if sys.platform == "win32" else "quiet")
    quiet.write_text("@echo off\r\n" if sys.platform == "win32" else "#!/bin/sh\n")
    quiet.chmod(0o755)
    with pytest.raises(SystemExit) as left:
        instructions.count(str(quiet), "engine", None)
    assert "no instruction count" in str(left.value)
