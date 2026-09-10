# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2022-2026 Andrew Wright

"""Tests for the games harvest.

What is under test is the reading and the deciding, not the downloading. `gh` is
replaced by a fake that records what it was asked for, so these pin the shape of
the listing the script parses, which artifacts it takes, and the two properties
that make it safe to run after every arm.

The first is that running it twice downloads nothing the second time. The
archive is append only and the marker is what says an artifact is down, so a
second run over a current archive has to be free or nobody will run it often.

The second is that it takes the strength runs and nothing else. Calibrate
uploads its rungs' games, and those are arche against another engine where a
strength game is arche against arche, so taking them would give the corpus a
second source and cost it the caveat it states. The listing also still holds
`gauntlet-<run>` artifacts, which were the rungs concatenated until `dcf87b7`
stopped uploading them. A future reader will be tempted by both, which is why
the exclusion is a test rather than a comment.
"""

import json
import subprocess
from pathlib import Path

import harvest_games
import pytest


def listing(*items):
    """The artifact listing as `gh api --jq` prints it: one json object a line."""
    return "".join(json.dumps(item) + "\n" for item in items)


def artifact(name, run=1, expired=False, expires="2026-12-01T00:00:00Z"):
    return {"name": name, "expired": expired, "run": run, "expires": expires}


class FakeGh:
    """Stands in for `gh`, and writes what a download would have written."""

    def __init__(self, rows, fails=()):
        self.rows = rows
        self.fails = set(fails)
        self.downloaded = []
        self.asked = []
        self.empty = set()

    def __call__(self, *arguments):
        self.asked.append(arguments)
        if arguments[0] == "api":
            return listing(*self.rows)
        assert arguments[:2] == ("run", "download"), arguments
        name = arguments[arguments.index("-n") + 1]
        into = arguments[arguments.index("--dir") + 1]
        if name in self.fails:
            raise subprocess.CalledProcessError(1, "gh", stderr="artifact not found")
        self.downloaded.append(name)
        Path(into).mkdir(parents=True, exist_ok=True)
        if name in self.empty:
            # a shard that died before it played: the manifest arrives, the
            # games do not, and the upload stays green
            (Path(into) / "result.txt").write_text("", encoding="utf-8")
            return
        (Path(into) / "games.pgn").write_text('[Event "m"]\n', encoding="utf-8")


@pytest.fixture
def fake(monkeypatch):
    def install(rows, fails=()):
        double = FakeGh(rows, fails)
        monkeypatch.setattr(harvest_games, "gh", double)
        return double

    return install


def test_it_takes_the_strength_artifacts(fake, tmp_path):
    double = fake(
        [
            artifact("strength-1-1-shard-0", run=1),
            artifact("strength-1-1-shard-1", run=1),
            artifact("benchmark-output-9"),
            artifact("coverage"),
        ]
    )
    taken, skipped, expired, failed = harvest_games.harvest(
        "o/r", tmp_path, harvest_games.artifacts("o/r")
    )
    assert taken == ["strength-1-1-shard-0", "strength-1-1-shard-1"]
    assert (skipped, expired, failed) == ([], [], [])
    assert double.downloaded == taken


def test_it_takes_neither_the_calibrate_rungs_nor_the_gauntlet_residue(fake, tmp_path):
    """A calibrate game is against another engine, which is a second source. The
    gauntlet names are residue from before `dcf87b7` and go the same way."""
    double = fake(
        [
            artifact("calibrate-7-1-stockfish-sf16", run=7),
            artifact("gauntlet-7", run=7),
            artifact("strength-8-1-shard-0", run=8),
        ]
    )
    taken, _, _, _ = harvest_games.harvest(
        "o/r", tmp_path, harvest_games.artifacts("o/r")
    )
    assert taken == ["strength-8-1-shard-0"]
    assert double.downloaded == ["strength-8-1-shard-0"]


def test_a_second_run_downloads_nothing(fake, tmp_path):
    rows = [artifact("strength-1-1-shard-0"), artifact("strength-1-1-shard-1")]
    first = fake(rows)
    harvest_games.harvest("o/r", tmp_path, harvest_games.artifacts("o/r"))
    assert len(first.downloaded) == 2
    second = fake(rows)
    taken, skipped, _, _ = harvest_games.harvest(
        "o/r", tmp_path, harvest_games.artifacts("o/r")
    )
    assert taken == []
    assert skipped == ["strength-1-1-shard-0", "strength-1-1-shard-1"]
    assert second.downloaded == []


def test_an_interrupted_download_is_taken_again(fake, tmp_path):
    """A directory is not the marker. An artifact whose download died leaves one
    behind, and it has to be fetched again rather than counted as held."""
    (tmp_path / "strength-1-1-shard-0").mkdir()
    double = fake([artifact("strength-1-1-shard-0")])
    taken, skipped, _, _ = harvest_games.harvest(
        "o/r", tmp_path, harvest_games.artifacts("o/r")
    )
    assert taken == ["strength-1-1-shard-0"]
    assert skipped == []
    assert double.downloaded == ["strength-1-1-shard-0"]


def test_an_expired_artifact_is_named_and_not_attempted(fake, tmp_path):
    double = fake(
        [
            artifact("strength-1-1-shard-0", expired=True),
            artifact("strength-2-1-shard-0"),
        ]
    )
    taken, _, expired, failed = harvest_games.harvest(
        "o/r", tmp_path, harvest_games.artifacts("o/r")
    )
    assert expired == ["strength-1-1-shard-0"]
    assert taken == ["strength-2-1-shard-0"]
    assert failed == []
    assert double.downloaded == ["strength-2-1-shard-0"]


def test_one_failed_download_does_not_stop_the_rest(fake, tmp_path):
    double = fake(
        [artifact("strength-1-1-shard-0"), artifact("strength-2-1-shard-0")],
        fails=["strength-1-1-shard-0"],
    )
    taken, _, _, failed = harvest_games.harvest(
        "o/r", tmp_path, harvest_games.artifacts("o/r")
    )
    assert taken == ["strength-2-1-shard-0"]
    assert [name for name, _ in failed] == ["strength-1-1-shard-0"]
    assert double.downloaded == ["strength-2-1-shard-0"]
    # a failure leaves no marker, so the next run tries it again
    assert not (tmp_path / "strength-1-1-shard-0" / harvest_games.MARKER).exists()


def test_the_pgn_list_holds_only_marked_directories(fake, tmp_path):
    fake([artifact("strength-1-1-shard-0")])
    harvest_games.harvest("o/r", tmp_path, harvest_games.artifacts("o/r"))
    stray = tmp_path / "strength-9-1-shard-0"
    stray.mkdir()
    (stray / "games.pgn").write_text('[Event "m"]\n', encoding="utf-8")
    assert harvest_games.pgns(tmp_path) == [
        str(tmp_path / "strength-1-1-shard-0" / "games.pgn")
    ]


def test_the_deadline_is_the_soonest_live_strength_artifact(fake, tmp_path):
    rows = [
        artifact("strength-1-1-shard-0", expires="2026-11-02T00:00:00Z"),
        artifact("strength-2-1-shard-0", expires="2026-12-31T00:00:00Z"),
        # an expired one has no deadline left to report, and a benchmark
        # artifact is not what this script is keeping
        artifact("strength-3-1-shard-0", expired=True, expires="2026-10-01T00:00:00Z"),
        artifact("benchmark-output-9", expires="2026-09-20T00:00:00Z"),
    ]
    fake(rows)
    listing = harvest_games.artifacts("o/r")
    assert harvest_games.soonest(tmp_path, listing) == "2026-11-02T00:00:00Z"


def test_an_artifact_already_held_is_not_a_deadline(fake, tmp_path):
    """The figure is what is still at risk. Left unfiltered it is the oldest
    live artifact's expiry whatever the archive holds, so it would read as a
    deadline on a run where every game is already safe."""
    rows = [artifact("strength-1-1-shard-0", expires="2026-11-02T00:00:00Z")]
    fake(rows)
    listing = harvest_games.artifacts("o/r")
    assert harvest_games.soonest(tmp_path, listing) == "2026-11-02T00:00:00Z"
    harvest_games.harvest("o/r", tmp_path, listing)
    assert harvest_games.soonest(tmp_path, listing) is None


def test_the_listing_asks_for_the_fields_it_parses(fake):
    """The script reads four fields off each artifact, and a rename upstream
    should fail here rather than produce an empty harvest."""
    double = fake([artifact("strength-1-1-shard-0")])
    rows = harvest_games.artifacts("o/r")
    recorded = double.asked
    assert rows == [
        {
            "name": "strength-1-1-shard-0",
            "expired": False,
            "run": 1,
            "expires": "2026-12-01T00:00:00Z",
        }
    ]
    query = recorded[0]
    assert "repos/o/r/actions/artifacts?per_page=100" in query
    assert "--paginate" in query
    jq = query[query.index("--jq") + 1]
    for field in ("name", "expired", ".workflow_run.id", "expires_at"):
        assert field in jq


def test_a_failed_download_fails_the_run_even_with_out(fake, tmp_path, monkeypatch):
    """The documented invocation passes --out, and that is the one where a failed
    download has to reach the exit code. Returning the corpus builder's status
    alone reports success over an arm's games left on a run that will expire,
    which is the loss the script exists to prevent."""
    built = []
    monkeypatch.setattr(
        harvest_games.build_corpus, "main", lambda argv: built.append(argv) or 0
    )
    fake(
        [artifact("strength-1-1-shard-0"), artifact("strength-2-1-shard-0")],
        fails=["strength-1-1-shard-0"],
    )
    status = harvest_games.main(
        [
            "--archive",
            str(tmp_path),
            "--out",
            str(tmp_path / "corpus.epd"),
            "--repo",
            "o/r",
        ]
    )
    assert built, "the corpus was still rebuilt from what did arrive"
    assert status == 1


def test_a_clean_run_with_out_returns_the_builders_status(fake, tmp_path, monkeypatch):
    monkeypatch.setattr(harvest_games.build_corpus, "main", lambda argv: 0)
    fake([artifact("strength-1-1-shard-0")])
    assert (
        harvest_games.main(
            [
                "--archive",
                str(tmp_path),
                "--out",
                str(tmp_path / "c.epd"),
                "--repo",
                "o/r",
            ]
        )
        == 0
    )


def test_a_corpus_build_that_fails_fails_the_run(fake, tmp_path, monkeypatch):
    monkeypatch.setattr(harvest_games.build_corpus, "main", lambda argv: 1)
    fake([artifact("strength-1-1-shard-0")])
    assert (
        harvest_games.main(
            [
                "--archive",
                str(tmp_path),
                "--out",
                str(tmp_path / "c.epd"),
                "--repo",
                "o/r",
            ]
        )
        == 1
    )


def test_a_gh_that_is_absent_is_reported_rather_than_raised(
    monkeypatch, tmp_path, capsys
):
    """gh carries the diagnosis and check=True puts it inside the exception, so
    without this the user gets a traceback and not the reason."""

    def missing(*arguments):
        raise FileNotFoundError(2, "No such file or directory", "gh")

    monkeypatch.setattr(harvest_games, "gh", missing)
    assert harvest_games.main(["--archive", str(tmp_path), "--repo", "o/r"]) == 1
    assert "gh is not on PATH" in capsys.readouterr().err


def test_a_gh_that_cannot_authenticate_prints_what_gh_said(
    monkeypatch, tmp_path, capsys
):
    def refused(*arguments):
        raise subprocess.CalledProcessError(
            4,
            "gh",
            stderr="gh: To get started with GitHub CLI, please run: gh auth login",
        )

    monkeypatch.setattr(harvest_games, "gh", refused)
    assert harvest_games.main(["--archive", str(tmp_path), "--repo", "o/r"]) == 1
    assert "gh auth login" in capsys.readouterr().err


def test_an_artifact_held_without_games_is_named(fake, tmp_path, capsys):
    """A shard that died before it played uploads its manifest and no games, and
    `if-no-files-found: warn` keeps the run green, so it arrives looking exactly
    like a shard that played."""
    double = fake([artifact("strength-1-1-shard-0"), artifact("strength-2-1-shard-0")])
    double.empty = {"strength-1-1-shard-0"}
    harvest_games.harvest("o/r", tmp_path, harvest_games.artifacts("o/r"))
    assert harvest_games.gameless(tmp_path) == ["strength-1-1-shard-0"]
    harvest_games.main(["--archive", str(tmp_path), "--repo", "o/r"])
    assert (
        "held but carries no games.pgn: strength-1-1-shard-0" in capsys.readouterr().err
    )
