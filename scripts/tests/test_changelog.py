"""Tests for the changelog pre-release hook.

git-cliff is replaced by a shim that records how it was called and prepends a
marker section, so what is under test is everything the script itself decides:
which sections survive, what range the changelog is generated over, and that
running the hook twice leaves the same bytes as running it once.
"""

import os
import stat
import subprocess
import textwrap
from pathlib import Path

import pytest

SCRIPT = Path(__file__).resolve().parent.parent / "changelog.sh"

CHANGELOG = textwrap.dedent(
    """\
    # Changelog

    ## [0.2.0-rc.1] - 2026-01-15

    - candidate preview

    ## [0.1.0] - 2025-12-01

    - the first release
    """
)


@pytest.fixture
def repo(tmp_path):
    """A git repo one commit past its v0.1.0 release, with a git-cliff shim
    on the path that logs its arguments and prepends a marker section."""
    repo = tmp_path / "repo"
    repo.mkdir()

    def git(*args):
        subprocess.run(
            ["git", "-c", "user.name=t", "-c", "user.email=t@t", *args],
            cwd=repo,
            check=True,
            capture_output=True,
        )

    git("init", "-q")
    (repo / "README").write_text("hi\n")
    git("add", ".")
    git("commit", "-qm", "init")
    git("tag", "v0.1.0")
    (repo / "README").write_text("hi again\n")
    git("commit", "-aqm", "more")
    (repo / "CHANGELOG.md").write_text(CHANGELOG)

    shim_dir = tmp_path / "bin"
    shim_dir.mkdir()
    shim = shim_dir / "git-cliff"
    shim.write_text(
        textwrap.dedent(
            """\
            #!/usr/bin/env python3
            import os
            import sys
            from pathlib import Path

            args = sys.argv[1:]
            Path(os.environ["CLIFF_LOG"]).write_text(" ".join(args) + "\\n")
            tag = args[args.index("--tag") + 1]
            target = Path(args[args.index("--prepend") + 1])
            old = target.read_text() if target.exists() else ""
            section = f"## [{tag}] - 2026-02-02\\n\\n- something new\\n\\n"
            # the real git-cliff knows the changelog's header and prepends
            # sections below it, so the shim inserts before the first section
            # rather than at the top of the file
            at = old.find("## [")
            at = len(old) if at == -1 else at
            target.write_text(old[:at] + section + old[at:])
            """
        )
    )
    shim.chmod(shim.stat().st_mode | stat.S_IEXEC)
    return repo


def run(repo, version):
    env = dict(
        os.environ,
        PATH=f"{repo.parent / 'bin'}{os.pathsep}{os.environ['PATH']}",
        CLIFF_LOG=str(repo.parent / "cliff_args"),
    )
    return subprocess.run(
        [str(SCRIPT), version],
        cwd=repo,
        env=env,
        check=False,
        capture_output=True,
        text=True,
    )


def test_candidates_are_superseded_and_full_releases_kept(repo):
    result = run(repo, "0.2.0")
    assert result.returncode == 0, result.stderr
    changelog = (repo / "CHANGELOG.md").read_text()
    assert "0.2.0-rc.1" not in changelog, "the candidate section should be gone"
    assert "## [0.1.0] - 2025-12-01" in changelog, (
        "a full release must survive: the dashes in its date must not read"
        " as a pre-release marker"
    )
    assert changelog.count("## [0.2.0]") == 1


def test_the_range_starts_at_the_last_full_release(repo):
    # an rc tag newer than the release must not shorten the range, or the
    # release after a run of candidates would only list what changed since
    # the last candidate
    subprocess.run(
        ["git", "tag", "v0.2.0-rc.1"], cwd=repo, check=True, capture_output=True
    )
    result = run(repo, "0.2.0")
    assert result.returncode == 0, result.stderr
    args = (repo.parent / "cliff_args").read_text()
    assert "v0.1.0..HEAD" in args


def test_running_twice_leaves_the_same_bytes_as_running_once(repo):
    # the hook has to be safe to run twice over, so the section it wrote last
    # time is dropped and rewritten rather than duplicated
    run(repo, "0.2.0")
    once = (repo / "CHANGELOG.md").read_bytes()
    result = run(repo, "0.2.0")
    assert result.returncode == 0, result.stderr
    assert (repo / "CHANGELOG.md").read_bytes() == once


def test_no_release_yet_covers_everything(repo):
    # with no full release tagged there is no range to cut, the whole history
    # is the changelog
    subprocess.run(
        ["git", "tag", "-d", "v0.1.0"], cwd=repo, check=True, capture_output=True
    )
    result = run(repo, "0.1.0")
    assert result.returncode == 0, result.stderr
    assert "no previous release" in result.stdout
    args = (repo.parent / "cliff_args").read_text()
    assert ".." not in args


def test_a_missing_changelog_is_created_rather_than_an_error(repo):
    (repo / "CHANGELOG.md").unlink()
    result = run(repo, "0.2.0")
    assert result.returncode == 0, result.stderr
    assert "## [0.2.0]" in (repo / "CHANGELOG.md").read_text()
