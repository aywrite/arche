# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2022-2026 Andrew Wright

"""Tests for the per commit bench check.

A repository whose every commit carries a file saying what its engine would
count, a cargo that builds a fake engine reading that file, and the check
walking the commits: what is under test is that a stated bench is compared
with a counted one for each commit, that a commit stating none is passed
over with a word, and that one mismatch fails the lot.
"""

import subprocess
import sys
from pathlib import Path

import pytest

SCRIPT = Path(__file__).resolve().parent.parent / "verify_bench.sh"

pytestmark = pytest.mark.skipif(
    sys.platform == "win32", reason="runs a shell script, which windows cannot"
)


@pytest.fixture
def repo(tmp_path):
    repo = tmp_path / "repo"
    repo.mkdir()

    def git(*args):
        return subprocess.run(
            ["git", "-c", "user.name=t", "-c", "user.email=t@t", *args],
            cwd=repo,
            check=True,
            capture_output=True,
            text=True,
        ).stdout.strip()

    def commit(counted, message):
        (repo / "bench.txt").write_text(f"{counted}\n")
        git("add", ".")
        git("commit", "-qm", message)
        return git("rev-parse", "HEAD")

    git("init", "-q")
    shims = tmp_path / "shims"
    shims.mkdir()
    cargo = shims / "cargo"
    # the fake engine counts whatever the file of the tree it was built from
    # says, and an engine told to crash does, the way a broken commit's
    # would. Built wherever --target-dir says, or target/
    cargo.write_text(
        "#!/usr/bin/env bash\n"
        "dir=target\n"
        "while [ $# -gt 0 ]; do\n"
        '  if [ "$1" = --target-dir ]; then dir=$2; shift; fi\n'
        "  shift\n"
        "done\n"
        'mkdir -p "$dir/release"\n'
        "printf '#!/usr/bin/env bash\\nn=$(cat %s/bench.txt)\\n"
        '[ "$n" = crash ] && exit 1\\necho "$n nodes 1 nps"\\n\' "$PWD"'
        ' > "$dir/release/arche"\n'
        'chmod +x "$dir/release/arche"\n'
    )
    cargo.chmod(0o755)
    return repo, git, commit, shims


def verify(repo, shims, base, head):
    return subprocess.run(
        [str(SCRIPT), base, head],
        cwd=repo,
        env={"PATH": f"{shims}:/usr/bin:/bin"},
        check=False,
        capture_output=True,
        text=True,
    )


def test_every_stated_bench_is_counted_and_a_match_passes(repo):
    repo, _git, commit, shims = repo
    base = commit(1, "docs(docs): Start")
    commit(100, "fix(search): One\n\nBench: 100")
    head = commit(200, "fix(search): Two\n\nBench: 200")
    result = verify(repo, shims, base, head)
    assert result.returncode == 0, result.stdout + result.stderr
    assert "fix(search): One: bench 100 ok" in result.stdout
    assert "fix(search): Two: bench 200 ok" in result.stdout


def test_a_commit_stating_no_bench_is_passed_over(repo):
    repo, _git, commit, shims = repo
    base = commit(1, "docs(docs): Start")
    head = commit(5, "docs(docs): Words only")
    result = verify(repo, shims, base, head)
    assert result.returncode == 0
    assert "docs(docs): Words only: no bench stated" in result.stdout


def test_one_mismatch_fails_the_lot_but_every_commit_is_still_reported(repo):
    repo, _git, commit, shims = repo
    base = commit(1, "docs(docs): Start")
    commit(100, "fix(search): One\n\nBench: 100")
    commit(200, "fix(search): Two\n\nBench: 250")
    head = commit(300, "fix(search): Three\n\nBench: 300")
    result = verify(repo, shims, base, head)
    assert result.returncode == 1
    assert "fix(search): Two: bench stated 250, counted 200" in result.stdout
    assert "fix(search): Three: bench 300 ok" in result.stdout


def test_a_bench_that_crashes_is_reported_and_the_rest_still_counted(repo):
    repo, _git, commit, shims = repo
    base = commit(1, "docs(docs): Start")
    commit("crash", "fix(search): One\n\nBench: 100")
    head = commit(200, "fix(search): Two\n\nBench: 200")
    result = verify(repo, shims, base, head)
    assert result.returncode == 1
    assert "fix(search): One: bench stated 100, counted nothing" in result.stdout
    assert "fix(search): Two: bench 200 ok" in result.stdout


def test_a_bench_line_that_git_does_not_read_as_a_trailer_is_not_one(repo):
    # a Bench line with prose after it is body text to git, and the check
    # reads trailers the way git does
    repo, _git, commit, shims = repo
    base = commit(1, "docs(docs): Start")
    head = commit(100, "fix(search): One\n\nBench: 999\n\nMore prose after.")
    result = verify(repo, shims, base, head)
    assert result.returncode == 0
    assert "fix(search): One: no bench stated" in result.stdout


def test_the_working_tree_is_never_touched(repo):
    # the commits are built from exports, not checkouts: a developer's
    # branch, index and half written files are exactly as they were after
    repo, git, commit, shims = repo
    base = commit(1, "docs(docs): Start")
    commit(100, "fix(search): One\n\nBench: 100")
    head = commit(200, "fix(search): Two\n\nBench: 200")
    branch = git("symbolic-ref", "--short", "HEAD")
    (repo / "bench.txt").write_text("half written\n")
    result = verify(repo, shims, base, head)
    assert result.returncode == 0, result.stderr
    assert git("rev-parse", "HEAD") == head
    assert git("symbolic-ref", "--short", "HEAD") == branch
    assert (repo / "bench.txt").read_text() == "half written\n"
    assert "checkout" not in git("reflog")
