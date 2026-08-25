"""Tests for the speed comparison.

Two binaries stand in for the tree as it stands and the commit it is measured
against. They print a bench whose rate is whatever the test wants, and log
every call, so what is under test is the arithmetic, the shape of the trailer
and that the rounds really alternate.
"""

import subprocess
import sys
from pathlib import Path

import pytest
import speed

SCRIPT = Path(__file__).resolve().parent.parent / "speed.sh"


def fake_engine(directory, name, nps_by_call, nodes=100):
    """An engine that prints a bench whose rate is the next of nps_by_call on
    every run, and appends its name to a log beside it."""
    log = directory / "calls.log"
    rates = directory / f"{name}.rates"
    rates.write_text("\n".join(str(n) for n in nps_by_call) + "\n")
    body = (
        "import pathlib, sys\n"
        f"log = pathlib.Path({str(log)!r})\n"
        f"rates = pathlib.Path({str(rates)!r})\n"
        "left = rates.read_text().split()\n"
        "rates.write_text('\\n'.join(left[1:]) + '\\n')\n"
        f"log.open('a').write({name!r} + '\\n')\n"
        "print('bench depth 1 hash 16MB positions 1')\n"
        f"print('{nodes} nodes ' + left[0] + ' nps')\n"
    )
    script = directory / f"{name}.py"
    script.write_text(body)
    if sys.platform == "win32":
        runner = directory / f"{name}.cmd"
        runner.write_text(f'@"{sys.executable}" "{script}" %*\r\n')
    else:
        runner = directory / name
        runner.write_text(f"#!{sys.executable}\n{body}")
        runner.chmod(0o755)
    return runner


def test_the_last_line_is_read_for_nodes_and_rate():
    text = "bench depth 7 hash 16MB positions 18\n...\n42847751 nodes 12473872 nps\n"
    assert speed.last_line(text) == (42847751, 12473872)


def test_the_change_is_between_medians_and_the_spread_is_the_wider_side():
    base = [100, 102, 98, 101, 99]
    candidate = [103, 105, 101, 104, 102]
    assert speed.trailer(base, candidate, "a1b2c3d") == (
        "Speed: +3.0% (bench nps, 5 interleaved rounds vs a1b2c3d, spread 4.0%)"
    )


def test_a_slowdown_carries_its_sign():
    assert speed.trailer([200, 200, 200], [195, 195, 195], "a1b2c3d") == (
        "Speed: -2.5% (bench nps, 3 interleaved rounds vs a1b2c3d, spread 0.0%)"
    )


def test_the_rounds_alternate_which_side_runs_first(tmp_path):
    base = fake_engine(tmp_path, "base", [100] * 3)
    candidate = fake_engine(tmp_path, "candidate", [110] * 3)
    measured = speed.measure(str(base), str(candidate), rounds=3, depth=1)
    assert measured.base_nps == [100, 100, 100]
    assert measured.candidate_nps == [110, 110, 110]
    assert (measured.base_nodes, measured.candidate_nodes) == (100, 100)
    calls = (tmp_path / "calls.log").read_text().split()
    assert calls == ["base", "candidate", "candidate", "base", "base", "candidate"]


def test_the_sides_are_told_apart_even_when_they_are_one_binary(tmp_path):
    # an engine measured against itself is the first thing anyone tries the
    # tooling on, and its node count belongs to both sides, not to one
    engine = fake_engine(tmp_path, "engine", [100] * 4, nodes=100)
    measured = speed.measure(str(engine), str(engine), rounds=2, depth=1)
    assert (measured.base_nodes, measured.candidate_nodes) == (100, 100)


def test_the_time_to_depth_is_the_count_divided_by_the_rate():
    measured = speed.Measured(
        base_nps=[100, 100, 100],
        candidate_nps=[200, 200, 200],
        base_nodes=1000,
        candidate_nodes=1000,
    )
    assert speed.time_to_depth(measured) == (10.0, 5.0)


def test_the_time_is_taken_a_round_at_a_time_not_from_the_median_rate():
    # an even number of rounds makes the median the mean of the middle two,
    # and that does not survive being divided into: twelve hundred nodes at a
    # hundred a second and at two hundred is twelve seconds and six, a median
    # of nine, where the median rate of a hundred and fifty would say eight
    measured = speed.Measured(
        base_nps=[100, 200],
        candidate_nps=[100, 200],
        base_nodes=1200,
        candidate_nodes=1200,
    )
    assert speed.time_to_depth(measured) == (9.0, 9.0)


def test_the_report_breaks_the_change_down_when_the_counts_differ(tmp_path, capsys):
    # the case the breakdown is there for: the tree loses a tenth of itself
    # and every node costs what it did, so the rate says nothing happened
    # while the search finishes a tenth sooner
    base = fake_engine(tmp_path, "base", [100] * 2, nodes=100)
    candidate = fake_engine(tmp_path, "candidate", [100] * 2, nodes=90)
    assert (
        speed.main(
            [str(base), str(candidate), "--rounds", "2", "--base-ref", "abc1234"]
        )
        == 0
    )
    out = capsys.readouterr().out
    assert "100 nodes" in out and "90 nodes" in out
    assert "nodes -10.0%, nps +0.0%, time to depth -10.0%" in out
    assert speed.COUNTS_DIFFER in out
    # and the trailer goes on saying the one thing it has always said
    assert out.strip().endswith(
        "Speed: +0.0% (bench nps, 2 interleaved rounds vs abc1234, spread 0.0%)"
    )


def test_the_report_leaves_the_breakdown_out_when_the_counts_match(tmp_path, capsys):
    # with the tree held still the time is the exact inverse of the rate, so
    # printing both would be the same measurement twice
    base = fake_engine(tmp_path, "base", [100] * 2, nodes=100)
    candidate = fake_engine(tmp_path, "candidate", [110] * 2, nodes=100)
    assert speed.main([str(base), str(candidate), "--rounds", "2"]) == 0
    out = capsys.readouterr().out
    assert "100 nodes in" in out
    assert "time to depth" not in out
    assert speed.COUNTS_DIFFER not in out


def test_fewer_than_two_rounds_is_refused(tmp_path, capsys):
    base = fake_engine(tmp_path, "base", [100])
    candidate = fake_engine(tmp_path, "candidate", [100])
    with pytest.raises(SystemExit) as left:
        speed.main([str(base), str(candidate), "--rounds", "1"])
    assert left.value.code == 2
    assert "rounds" in capsys.readouterr().err


def test_an_engine_that_prints_no_bench_is_named_rather_than_a_traceback(tmp_path):
    quiet = tmp_path / ("quiet.cmd" if sys.platform == "win32" else "quiet")
    quiet.write_text("@echo off\r\n" if sys.platform == "win32" else "#!/bin/sh\n")
    quiet.chmod(0o755)
    with pytest.raises(SystemExit) as left:
        speed.measure(str(quiet), str(quiet), rounds=2, depth=1)
    assert "no bench" in str(left.value)


def test_the_trailer_passes_the_hook():
    import check_trailers

    line = speed.trailer([100, 101, 99], [104, 103, 105], "a1b2c3d")
    message = f"perf(search): Sort less\n\nBench: 1\n{line}\n"
    assert check_trailers.problems(message) == []


@pytest.mark.skipif(
    sys.platform == "win32", reason="runs a shell script, which windows cannot"
)
def test_the_wrapper_builds_the_base_commit_and_measures_against_it(tmp_path):
    # a repository of two commits, a cargo that "builds" by writing a fake
    # engine wherever it is told to, and the wrapper measuring the second
    # commit against the first
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
    (repo / "scripts").mkdir()
    (repo / "scripts" / "speed.py").write_text(
        speed.__file__ and Path(speed.__file__).read_text()
    )
    (repo / "Cargo.toml").write_text('[package]\nname = "arche"\n')
    git("add", ".")
    git("commit", "-qm", "first")
    (repo / "Cargo.toml").write_text('[package]\nname = "arche"\nversion = "2"\n')
    git("commit", "-aqm", "second")
    # the tree is measured against the commit it will be made on top of,
    # which is head: the trailer is produced before the commit exists
    base = git("rev-parse", "--short", "HEAD")

    shims = tmp_path / "shims"
    shims.mkdir()
    cargo = shims / "cargo"
    cargo.write_text(
        "#!/usr/bin/env bash\n"
        "# writes a fake engine under the target dir asked for, or target/\n"
        "dir=target\n"
        "while [ $# -gt 0 ]; do\n"
        '  if [ "$1" = --target-dir ]; then dir=$2; shift; fi\n'
        "  shift\n"
        "done\n"
        'mkdir -p "$dir/release"\n'
        "printf '#!/usr/bin/env bash\\necho 100 nodes 1000 nps\\n' > \"$dir/release/arche\"\n"
        'chmod +x "$dir/release/arche"\n'
    )
    cargo.chmod(0o755)

    result = subprocess.run(
        [str(SCRIPT)],
        cwd=repo,
        env={
            "PATH": f"{shims}:{Path(sys.executable).parent}:/usr/bin:/bin",
            "ROUNDS": "2",
        },
        check=False,
        capture_output=True,
        text=True,
    )
    assert result.returncode == 0, result.stderr
    assert result.stdout.strip().endswith(
        f"Speed: +0.0% (bench nps, 2 interleaved rounds vs {base}, spread 0.0%)"
    )
    # the base binary is kept, so measuring against the same commit again
    # costs only the rounds
    assert (repo / "target" / "speed" / base / "arche").exists()
