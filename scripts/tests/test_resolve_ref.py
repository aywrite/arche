# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2022-2026 Andrew Wright

"""Tests for the workflow ref resolver.

The fixture is a local upstream standing in for github, so every form a
workflow input can take is resolved offline. The trigger the script runs
under is an argument rather than inherited, since these tests run inside a
workflow themselves.
"""

import os
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

    # everything after the clone only exists upstream
    (upstream / "README").write_text("two\n")
    git(upstream, "commit", "-aqm", "second")
    git(upstream, "branch", "-q", "feature")
    git(upstream, "update-ref", "refs/pull/7/head", "HEAD")
    return clone


def resolve(clone, ref, event=None):
    """Run the script with the named trigger, or with none, which is what a
    machine that is not a runner looks like."""
    environment = dict(os.environ)
    environment.pop("GITHUB_EVENT_NAME", None)
    if event is not None:
        environment["GITHUB_EVENT_NAME"] = event
    return subprocess.run(
        [str(SCRIPT), ref],
        cwd=clone,
        check=False,
        capture_output=True,
        text=True,
        env=environment,
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


# The script runs only under the triggers where the ref was chosen by somebody
# who can already push. The trigger is the one part of that a step can read:
# a job's permissions and secrets are not visible from a script.


def test_a_dispatch_is_how_the_match_workflows_run(clone):
    assert resolve(clone, "v1.0.0", "workflow_dispatch").returncode == 0


def test_a_push_is_how_the_release_workflow_runs(clone):
    assert resolve(clone, "v1.0.0", "push").returncode == 0


def test_no_trigger_at_all_is_a_machine_that_is_not_a_runner(clone):
    assert resolve(clone, "v1.0.0").returncode == 0


def test_pull_request_target_is_refused(clone):
    result = resolve(clone, "7", "pull_request_target")
    assert result.returncode != 0
    assert "pull_request_target" in result.stderr
    assert result.stdout == ""


def test_workflow_run_is_refused(clone):
    result = resolve(clone, "7", "workflow_run")
    assert result.returncode != 0
    assert "workflow_run" in result.stderr


def test_a_comment_event_is_refused(clone):
    result = resolve(clone, "7", "issue_comment")
    assert result.returncode != 0
    assert "issue_comment" in result.stderr


def test_the_refusal_covers_a_named_ref_and_not_only_a_number(clone):
    assert resolve(clone, "v1.0.0", "pull_request_target").returncode != 0
