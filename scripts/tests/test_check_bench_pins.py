# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2022-2026 Andrew Wright

"""Tests for the check that a commit's stated bench matches its own pins.

A repository whose every commit carries a `bench.rs` pinning per position
counts and a message stating a bench: what is under test is that the two are
compared for each commit, that the sum is read from this test's list and not
the one after it, that a commit stating no bench is passed over with a word,
and that one mismatch fails the lot.
"""

import subprocess
import sys
from pathlib import Path
from types import SimpleNamespace

import pytest

SCRIPT = Path(__file__).resolve().parent.parent / "check_bench_pins.py"

BENCH_RS = "arche-core/src/bench.rs"


def source(counts, reference=(9_999_999,)):
    """A bench.rs pinning these counts, and a reference list after it.

    The second list is what a naive walk to the next end would swallow, so
    every case here carries one.
    """

    def listing(values):
        return "\n".join(
            f'                ("p{i}", {n}),' for i, n in enumerate(values)
        )

    return f"""// a stand in for the real one
    #[test]
    fn node_counts_have_not_moved() {{
        assert_eq!(
            counted,
            vec![
{listing(counts)}
            ]
        );
    }}

    #[test]
    fn reference_node_counts_have_not_moved() {{
        assert_eq!(
            counted,
            vec![
{listing(reference)}
            ]
        );
    }}
"""


@pytest.fixture
def repo(tmp_path):
    path = tmp_path / "repo"
    (path / "arche-core" / "src").mkdir(parents=True)

    def git(*args):
        return subprocess.run(
            ["git", "-c", "user.name=t", "-c", "user.email=t@t", *args],
            cwd=path,
            check=True,
            capture_output=True,
            text=True,
        ).stdout.strip()

    def commit_tree(message):
        """Commit whatever the test has just written."""
        git("add", "-A")
        git("commit", "-qm", message)
        return git("rev-parse", "HEAD")

    def commit(counts, message):
        (path / BENCH_RS).write_text(source(counts))
        return commit_tree(message)

    git("init", "-q")
    return SimpleNamespace(path=path, commit=commit, commit_tree=commit_tree)


def check(repo, base, head):
    return subprocess.run(
        [sys.executable, str(SCRIPT), base, head],
        cwd=repo.path,
        capture_output=True,
        text=True,
        check=False,
    )


def test_a_stated_bench_matching_its_pins_passes(repo):
    base = repo.commit([1], "root")
    head = repo.commit([10, 20, 30], "perf(search): x\n\nBench: 60")
    done = check(repo, base, head)
    assert done.returncode == 0, done.stdout
    assert "bench 60 matches its pins" in done.stdout


def test_a_stated_bench_that_is_not_the_sum_fails(repo):
    base = repo.commit([1], "root")
    repo.commit([10, 20, 30], "perf(search): x\n\nBench: 61")
    done = check(repo, base, "HEAD")
    assert done.returncode == 1
    assert "bench stated 61, pins count 60" in done.stdout


def test_the_reference_list_after_it_is_not_counted(repo):
    # the bug this check was written with: walking to the next thing that
    # looks like an end swallows the second list and inflates every sum
    base = repo.commit([1], "root")
    repo.commit([10, 20], "perf(search): x\n\nBench: 30")
    done = check(repo, base, "HEAD")
    assert done.returncode == 0, done.stdout


def test_a_commit_stating_no_bench_is_passed_over(repo):
    base = repo.commit([1], "root")
    repo.commit([10, 20], "docs(docs): say something")
    done = check(repo, base, "HEAD")
    assert done.returncode == 0
    assert "no bench stated" in done.stdout


def test_one_mismatch_fails_the_lot_and_every_commit_is_reported(repo):
    base = repo.commit([1], "root")
    repo.commit([10], "perf(search): a\n\nBench: 10")
    repo.commit([10, 5], "perf(search): b\n\nBench: 999")
    repo.commit([10, 5, 5], "perf(search): c\n\nBench: 20")
    done = check(repo, base, "HEAD")
    assert done.returncode == 1
    assert "perf(search): a" in done.stdout
    assert "bench stated 999" in done.stdout
    assert "perf(search): c" in done.stdout


def test_the_last_bench_trailer_is_the_one_compared(repo):
    # openbench reads the last, and so does git; a message that restates it
    # has to be held to the one that counts
    base = repo.commit([1], "root")
    repo.commit([10, 20], "perf(search): x\n\nBench: 999\nBench: 30")
    done = check(repo, base, "HEAD")
    assert done.returncode == 0, done.stdout


def test_pins_that_have_moved_are_an_error_rather_than_a_pass(repo):
    # a rename that quietly stopped the check working would be worse than a
    # failure, since nothing would say so
    base = repo.commit([1], "root")
    (repo.path / BENCH_RS).write_text("fn something_else_entirely() {}\n")
    repo.commit_tree("refactor(bench): rename\n\nBench: 60")
    done = check(repo, base, "HEAD")
    assert done.returncode != 0
    assert "node_counts_have_not_moved" in done.stderr


def test_the_pins_are_found_wherever_the_crate_puts_them(repo):
    # the crate has been renamed once already; the check reads the pins out
    # of each commit's own tree rather than a path fixed when it was written
    base = repo.commit([7], "root")
    moved = repo.path / "renamed-crate" / "src" / "bench.rs"
    moved.parent.mkdir(parents=True)
    (repo.path / BENCH_RS).rename(moved)
    repo.commit_tree("build(workspace): rename\n\nBench: 7")
    done = check(repo, base, "HEAD")
    assert done.returncode == 0, done.stdout + done.stderr
    assert "matches its pins" in done.stdout


def test_two_files_of_pins_are_an_error(repo):
    # one source of truth: two copies of the pinned test would let them
    # disagree, and the check refuses to pick one
    base = repo.commit([5], "root")
    twin = repo.path / "arche-core" / "src" / "bench_twin.rs"
    twin.write_text(source([5]))
    repo.commit_tree("test(bench): twin\n\nBench: 5")
    done = check(repo, base, "HEAD")
    assert done.returncode != 0
    assert "one place" in done.stderr
