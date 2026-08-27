# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2022-2026 Andrew Wright

"""Tests for the workflow ref resolver.

The fixture is a local upstream standing in for github, so every form a
workflow input can take is resolved offline: a commit already present, a
branch or tag that has to be fetched, and a bare number meaning a pull
request's head.
"""

import subprocess
import sys
from pathlib import Path

import pytest

SCRIPT = Path(__file__).resolve().parent.parent / "resolve_ref.sh"

# the script is run through its shebang, which only a posix shell can do:
# from a windows clone run these under wsl
pytestmark = pytest.mark.skipif(
    sys.platform == "win32", reason="runs a shell script, which windows cannot"
)


def git(cwd, *args):
    return subprocess.run(
        ["git", "-c", "user.name=t", "-c", "user.email=t@t", *args],
        cwd=cwd,
        check=True,
        capture_output=True,
        text=True,
    ).stdout.strip()


@pytest.fixture
def clone(tmp_path):
    """A clone of a local upstream that has a release tag, a branch created
    after the clone was taken, and a pull request head ref."""
    upstream = tmp_path / "upstream"
    upstream.mkdir()
    git(upstream, "init", "-q")
    (upstream / "README").write_text("one\n")
    git(upstream, "add", ".")
    git(upstream, "commit", "-qm", "first")
    git(upstream, "tag", "v1.0.0")

    clone = tmp_path / "clone"
    git(tmp_path, "clone", "-q", str(upstream), str(clone))

    # everything after the clone only exists upstream, like a push made
    # after a workflow's checkout
    (upstream / "README").write_text("two\n")
    git(upstream, "commit", "-aqm", "second")
    git(upstream, "branch", "-q", "feature")
    git(upstream, "update-ref", "refs/pull/7/head", "HEAD")
    return clone


def resolve(clone, ref):
    return subprocess.run(
        [str(SCRIPT), ref],
        cwd=clone,
        check=False,
        capture_output=True,
        text=True,
    )


def head_of(clone, upstream_ref):
    return git(clone, "ls-remote", "origin", upstream_ref).split()[0]


def test_a_commit_already_here_resolves_without_fetching(clone):
    sha = git(clone, "rev-parse", "HEAD")
    result = resolve(clone, sha)
    assert result.returncode == 0
    assert result.stdout.strip() == sha


def test_a_tag_resolves_to_its_commit(clone):
    result = resolve(clone, "v1.0.0")
    assert result.returncode == 0
    assert result.stdout.strip() == git(clone, "rev-parse", "HEAD")


def test_a_branch_only_upstream_is_fetched_by_name(clone):
    result = resolve(clone, "feature")
    assert result.returncode == 0
    assert result.stdout.strip() == head_of(clone, "refs/heads/feature")


def test_a_number_means_a_pull_request_head(clone):
    result = resolve(clone, "7")
    assert result.returncode == 0
    assert result.stdout.strip() == head_of(clone, "refs/pull/7/head")


def test_a_ref_that_exists_nowhere_fails(clone):
    result = resolve(clone, "no-such-ref")
    assert result.returncode != 0


def test_a_number_with_no_pull_request_fails(clone):
    result = resolve(clone, "999")
    assert result.returncode != 0
