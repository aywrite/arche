"""Tests for building the engine as it was at a commit.

A repository of two commits whose one file says which commit it is, and a
cargo that "builds" by writing a fake engine printing that file: what is
under test is that the binary asked for is the commit's, that the working
tree is never touched to get it, and that the build lands where cargo is
told to put things.
"""

import subprocess
import sys
from pathlib import Path

import pytest

SCRIPT = Path(__file__).resolve().parent.parent / "build_at.sh"

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

    git("init", "-q")
    (repo / "Cargo.toml").write_text('[package]\nname = "arche"\n')
    (repo / "which.txt").write_text("first\n")
    git("add", ".")
    git("commit", "-qm", "first")
    (repo / "which.txt").write_text("second\n")
    git("commit", "-aqm", "second")

    shims = tmp_path / "shims"
    shims.mkdir()
    cargo = shims / "cargo"
    # builds wherever --target-dir says, or target/, a fake engine that
    # prints the file of the tree it was built from
    cargo.write_text(
        "#!/usr/bin/env bash\n"
        "dir=target\n"
        "while [ $# -gt 0 ]; do\n"
        '  if [ "$1" = --target-dir ]; then dir=$2; shift; fi\n'
        "  shift\n"
        "done\n"
        'mkdir -p "$dir/release"\n'
        'printf \'#!/usr/bin/env bash\\ncat %s/which.txt\\n\' "$PWD" > "$dir/release/arche"\n'
        'chmod +x "$dir/release/arche"\n'
    )
    cargo.chmod(0o755)
    return repo, git, shims


def prints(binary):
    """What the fake engine says, which is the tree it was built from."""
    return subprocess.run(
        [str(binary)], check=True, capture_output=True, text=True
    ).stdout


def build(repo, shims, ref, binary, **env):
    return subprocess.run(
        [str(SCRIPT), ref, str(binary)],
        cwd=repo,
        env={"PATH": f"{shims}:/usr/bin:/bin", **env},
        check=False,
        capture_output=True,
        text=True,
    )


def test_the_binary_is_the_commits_and_the_tree_is_untouched(repo, tmp_path):
    repo, git, shims = repo
    branch = git("symbolic-ref", "--short", "HEAD")
    head = git("rev-parse", "HEAD")
    (repo / "which.txt").write_text("dirty\n")

    result = build(repo, shims, "HEAD~1", tmp_path / "old")
    assert result.returncode == 0, result.stderr
    assert prints(tmp_path / "old") == "first\n"

    assert git("rev-parse", "HEAD") == head
    assert git("symbolic-ref", "--short", "HEAD") == branch
    assert (repo / "which.txt").read_text() == "dirty\n"
    assert "checkout" not in git("reflog")


def test_the_build_lands_where_cargo_is_told_to_put_it(repo, tmp_path):
    repo, _git, shims = repo
    elsewhere = tmp_path / "elsewhere"
    result = build(
        repo, shims, "HEAD", tmp_path / "new", CARGO_TARGET_DIR=str(elsewhere)
    )
    assert result.returncode == 0, result.stderr
    assert prints(tmp_path / "new") == "second\n"
    assert (elsewhere / "release" / "arche").exists()
    assert not (repo / "target").exists()


def test_a_ref_that_is_not_a_commit_fails(repo, tmp_path):
    repo, _git, shims = repo
    result = build(repo, shims, "nowhere", tmp_path / "none")
    assert result.returncode != 0
    assert not (tmp_path / "none").exists()
