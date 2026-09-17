# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2022-2026 Andrew Wright

"""Tests for the table of engines the calibration gauntlet can play.

Building one needs the network and a compiler, so what is checked is the
table itself: that every engine it names is described completely, that an
engine it does not name is refused rather than attempted, and that the
default ladder names engines the table knows (which would otherwise fail a
run rather than a check).
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
    # asked of the shell rather than of the file, so that a function named in
    # a comment does not pass
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
    # one ladder written twice, once per trigger, or a run from the actions
    # tab and a run from a release are not comparable
    assert len(set(ladders)) == 1, ladders
    for ladder in ladders:
        for rung in ladder.split(","):
            engine, _, rest = rung.partition(":")
            assert engine in known, f"{rung} names an engine with no build"
            assert rest.count(":") == 1, f"{rung} is not engine:tag:rating"
