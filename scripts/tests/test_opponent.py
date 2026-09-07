# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2022-2026 Andrew Wright

"""Tests for the table of engines the calibration gauntlet can play.

Building one needs the network and a compiler, which a test has neither of,
so what is checked here is the table itself: that every engine it names is
described completely, that an engine it does not name is refused rather than
attempted, and that the ladder the workflow plays by default names engines
the table knows. The last of those is the one that would otherwise go
unnoticed, since a default that names an engine with no build fails a run
rather than a check.
"""

import subprocess
import sys
from pathlib import Path

import pytest
import yaml

ROOT = Path(__file__).resolve().parent.parent.parent
SCRIPT = ROOT / "scripts" / "opponent.sh"
WORKFLOW = ROOT / ".github" / "workflows" / "calibrate.yml"

pytestmark = pytest.mark.skipif(
    sys.platform == "win32", reason="runs a shell script, which windows cannot"
)


def opponent(*args):
    return subprocess.run(
        [str(SCRIPT), *args], cwd=ROOT, check=False, capture_output=True, text=True
    )


def engines() -> list[str]:
    listed = opponent("list")
    assert listed.returncode == 0, listed.stderr
    return listed.stdout.split()


def default_ladders() -> list[str]:
    workflow = yaml.safe_load(WORKFLOW.read_text(encoding="utf-8"))
    # yaml reads a bare on: as a boolean, so the triggers are under True
    triggers = workflow[True]
    return [trigger["inputs"]["ladder"]["default"] for trigger in triggers.values()]


def test_the_table_names_engines():
    assert "stash" in engines()


def test_every_engine_named_says_where_it_comes_from():
    for engine in engines():
        where = opponent("repository", engine)
        assert where.returncode == 0, where.stderr
        assert where.stdout.startswith("https://"), where.stdout


def test_every_engine_named_has_a_build():
    # the workflow reads the list to decide whether a ladder entry can be
    # played at all, so a name in it with no build behind it would be refused
    # after the clone rather than before the match. Asked of the shell rather
    # than of the file, so that a function named in a comment does not pass
    for engine in engines():
        declared = subprocess.run(
            [
                "bash",
                "-c",
                'source "$1" list > /dev/null; declare -F "${2}_build"',
                "opponent",
                str(SCRIPT),
                engine,
            ],
            check=False,
            capture_output=True,
            text=True,
        )
        assert declared.returncode == 0, f"{engine} has no build: {declared.stderr}"


def test_an_engine_the_table_does_not_know_is_refused(tmp_path):
    binary = tmp_path / "engine"
    for asked in (
        opponent("repository", "rustic"),
        opponent("build", "rustic", "alpha-3", str(binary)),
    ):
        assert asked.returncode != 0
        assert "rustic" in asked.stderr
    # refused rather than attempted: nothing was fetched and nothing was built
    assert not binary.exists()


def test_the_commands_it_does_not_have_are_refused():
    for asked in (opponent(), opponent("clone"), opponent("build", "stash")):
        assert asked.returncode != 0
        assert "usage" in asked.stderr


def test_the_default_ladder_names_engines_the_table_knows():
    known = engines()
    ladders = default_ladders()
    assert ladders, "the workflow has no ladder to check"
    # one ladder written twice, once per trigger. A run from the actions tab
    # and a run from a release play the same opponents or neither figure means
    # what the other does
    assert len(set(ladders)) == 1, ladders
    for ladder in ladders:
        for rung in ladder.split(","):
            engine, _, rest = rung.partition(":")
            assert engine in known, f"{rung} names an engine with no build"
            assert rest.count(":") == 1, f"{rung} is not engine:tag:rating"
