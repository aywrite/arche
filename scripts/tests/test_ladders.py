# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2022-2026 Andrew Wright

"""Tests for the table of ccrl lists the calibration gauntlet can play against.

A ladder's ratings belong to the list they were read off, so the workflow takes
the ladder from the list's block rather than from a default of its own. What
is checked is that every block is complete, that a list the table does not
name is refused, that the workflow offers exactly the lists the table holds,
and that nothing in the workflow or the release puts a ladder, a time control
or a game count back in front of the table's.
"""

import re
import subprocess
import sys
from pathlib import Path

import pytest
import yaml

ROOT = Path(__file__).resolve().parent.parent.parent
SCRIPT = ROOT / "scripts" / "ladders.sh"
CALIBRATE = ROOT / ".github" / "workflows" / "calibrate.yml"
RELEASE = ROOT / ".github" / "workflows" / "release.yml"

FIELDS = {
    "ladder",
    "time_control",
    "games",
    "max_match_minutes",
    "rated_at",
    "artifact",
}
# the settings a list decides unless a box is filled in
FROM_THE_LIST = ("ladder", "time_control", "games", "max_match_minutes")

pytestmark = pytest.mark.skipif(
    sys.platform == "win32", reason="runs a shell script, which windows cannot"
)


def ladders(*args):
    return subprocess.run(
        [str(SCRIPT), *args], cwd=ROOT, check=False, capture_output=True, text=True
    )


def lists() -> list[str]:
    listed = ladders("list")
    assert listed.returncode == 0, listed.stderr
    return listed.stdout.split()


def preset(name: str) -> dict[str, str]:
    read = ladders("preset", name)
    assert read.returncode == 0, read.stderr
    return dict(line.split("=", 1) for line in read.stdout.splitlines())


def triggers(path: Path) -> dict:
    # yaml reads a bare on: as a boolean, so the triggers are under True
    return yaml.safe_load(path.read_text(encoding="utf-8"))[True]


def test_the_table_names_both_lists():
    assert lists() == ["40/15", "blitz"]


def test_every_list_gives_every_setting():
    for name in lists():
        settings = preset(name)
        assert set(settings) == FIELDS, f"{name}: {sorted(settings)}"
        for key, value in settings.items():
            assert value, f"{name} has an empty {key}"
        assert int(settings["games"]) > 0
        assert int(settings["max_match_minutes"]) > 0


def test_a_list_the_table_does_not_name_is_refused():
    refused = ladders("preset", "bullet")
    assert refused.returncode != 0
    assert "bullet" in refused.stderr
    assert not refused.stdout


def test_the_commands_it_does_not_have_are_refused():
    for asked in (ladders(), ladders("ladder"), ladders("preset")):
        assert asked.returncode != 0
        assert "usage" in asked.stderr


def test_the_actions_tab_offers_the_lists_the_table_holds():
    offered = triggers(CALIBRATE)["workflow_dispatch"]["inputs"]["list"]["options"]
    assert sorted(offered) == lists()


def test_the_workflow_has_no_setting_of_its_own_for_a_list_to_lose_to():
    # a default here would be played whatever list was chosen, so picking
    # 40/15 on the actions tab would publish the blitz ladder on its scale
    for trigger_name, trigger in triggers(CALIBRATE).items():
        for key in FROM_THE_LIST:
            spec = trigger["inputs"].get(key)
            if spec is not None:
                assert "default" not in spec, f"{trigger_name} defaults {key}"


def test_the_release_plays_the_table_rather_than_settings_of_its_own():
    jobs = yaml.safe_load(RELEASE.read_text(encoding="utf-8"))["jobs"]
    called = [
        job for job in jobs.values() if job.get("uses", "").endswith("calibrate.yml")
    ]
    assert {job["with"].get("list", "blitz") for job in called} == set(lists())
    for job in called:
        for key in FROM_THE_LIST:
            assert key not in job["with"], f"the release passes {key}"


def test_no_list_downloads_another_lists_games():
    # the estimate downloads <prefix>-<run id>-<attempt>-*, and a run id is
    # digits, so a prefix only reaches another list's artifacts if that
    # list's prefix is this one, a dash and a digit
    prefixes = [preset(name)["artifact"] for name in lists()]
    assert len(set(prefixes)) == len(prefixes), prefixes
    for one in prefixes:
        for other in prefixes:
            if one != other:
                assert not re.match(re.escape(one) + r"-\d", other), (one, other)
