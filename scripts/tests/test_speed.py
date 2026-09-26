# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2022-2026 Andrew Wright

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
    every run, and appends its name to a log beside it. The first rate is
    listed twice, once for the warmup, so for an engine on one side
    nps_by_call is the measured runs. An engine on both sides warms up
    twice and reads one of them."""
    log = directory / "calls.log"
    rates = directory / f"{name}.rates"
    rates.write_text("\n".join(str(n) for n in nps_by_call[:1] + nps_by_call) + "\n")
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


def test_the_interval_steps_in_as_far_as_the_signed_rank_tables_say():
    # the published two sided 5% critical values are 0 at six rounds, 5 at
    # nine and 89 at twenty five, and the bound is the next Walsh average
    # after them. Five rounds have no interval at all: even the most extreme
    # sign pattern turns up one time in thirty two
    assert speed.signed_rank_depth(5) == 0
    assert speed.signed_rank_depth(6) == 1
    assert speed.signed_rank_depth(9) == 6
    assert speed.signed_rank_depth(25) == 90


def test_the_trailer_carries_the_paired_change_and_its_interval():
    base = [100] * 6
    assert speed.trailer(base, [103] * 6, "a1b2c3d") == (
        "Speed: +3.0% (bench nps, 95% interval +3.0% to +3.0%, "
        "6 interleaved rounds vs a1b2c3d)"
    )
    assert speed.trailer(base, [97] * 6, "a1b2c3d") == (
        "Speed: -3.0% (bench nps, 95% interval -3.0% to -3.0%, "
        "6 interleaved rounds vs a1b2c3d)"
    )


def test_each_round_is_read_against_its_own_pair():
    # the runner halves in speed partway through and both sides with it. The
    # medians of each side land wherever the halving puts them, while every
    # round's pair still says five percent
    base = [200, 200, 200, 100, 100, 100, 100]
    candidate = [210, 210, 210, 105, 105, 105, 105]
    estimate = speed.paired(base, candidate)
    assert estimate.change == pytest.approx(5.0)
    assert (estimate.low, estimate.high) == pytest.approx((5.0, 5.0))


def test_one_loaded_round_moves_the_estimate_little_and_widens_the_interval():
    base = [100] * 9
    candidate = [102, 101, 103, 102, 101, 103, 102, 102, 70]
    estimate = speed.paired(base, candidate)
    assert 1.5 < estimate.change < 2.5
    # its averages with the other eight carry the lower bound out towards
    # it, so the interval declines a claim the other eight rounds would make
    assert estimate.low < -10
    assert speed.verdict(estimate, 1.0).startswith("not resolved")


def test_a_change_is_claimed_only_when_the_interval_is_past_the_threshold():
    def says(low, high):
        return speed.verdict(speed.Estimate((low + high) / 2, low, high), 2.0)

    assert says(2.5, 4.0).startswith("faster")
    assert says(-4.0, -2.5).startswith("slower")
    # clear of zero, but not of the threshold
    assert says(0.5, 1.8).startswith("no change beyond ±2.0%")
    assert says(-1.0, 1.0).startswith("no change beyond ±2.0%")
    # past zero and past the threshold on one side, clear of neither
    assert says(0.6, 2.9).startswith("not resolved")
    assert says(-3.0, 3.0).startswith("not resolved")


def test_the_rounds_alternate_which_side_runs_first(tmp_path):
    base = fake_engine(tmp_path, "base", [100] * 3)
    candidate = fake_engine(tmp_path, "candidate", [110] * 3)
    measured = speed.measure(str(base), str(candidate), rounds=3, depth=1)
    assert measured.base_nps == [100, 100, 100]
    assert measured.candidate_nps == [110, 110, 110]
    assert (measured.base_nodes, measured.candidate_nodes) == (100, 100)
    calls = (tmp_path / "calls.log").read_text().split()
    # the first two are the warmup
    assert calls[2:] == ["base", "candidate", "candidate", "base", "base", "candidate"]


def test_each_side_warms_up_once_and_the_run_is_thrown_away(tmp_path):
    # a slow first run on each side, which no round sees
    base = fake_engine(tmp_path, "base", [100] * 6)
    candidate = fake_engine(tmp_path, "candidate", [101] * 6)
    (tmp_path / "base.rates").write_text("50\n" + "100\n" * 6)
    (tmp_path / "candidate.rates").write_text("50\n" + "101\n" * 6)
    measured = speed.measure(str(base), str(candidate), rounds=6, depth=1)
    assert measured.base_nps == [100] * 6
    assert measured.candidate_nps == [101] * 6
    assert measured.replaced == []
    calls = (tmp_path / "calls.log").read_text().split()
    assert calls[:2] == ["base", "candidate"]
    assert len(calls) == 14


def test_the_cpus_are_read_as_a_list():
    assert speed.cpus("4") == {4}
    assert speed.cpus("2,3") == {2, 3}
    assert speed.cpus("4-6,9") == {4, 5, 6, 9}
    for text in ("two", "", "4-"):
        with pytest.raises(speed.argparse.ArgumentTypeError):
            speed.cpus(text)


def test_a_cpu_the_system_refuses_is_a_usage_error(monkeypatch, capsys):
    def refuse(pid, cpus):
        raise OSError(22, "Invalid argument")

    monkeypatch.setattr(speed.os, "sched_setaffinity", refuse, raising=False)
    with pytest.raises(SystemExit) as left:
        speed.main(["base", "candidate", "--rounds", "6", "--cpu", "999"])
    assert left.value.code == 2
    assert "--cpu [999]" in capsys.readouterr().err


def test_the_process_is_pinned_before_any_bench_runs(tmp_path, monkeypatch):
    pinned = []
    monkeypatch.setattr(
        speed.os,
        "sched_setaffinity",
        lambda pid, cpus: pinned.append((pid, cpus)),
        raising=False,
    )

    def bench(binary, depth):
        # nothing runs before the pin
        assert pinned == [(0, {2, 3})]
        return 100, 100

    monkeypatch.setattr(speed, "bench", bench)
    assert speed.main(["base", "candidate", "--rounds", "6", "--cpu", "2,3"]) == 0


def test_a_loaded_round_is_run_again_at_the_end(tmp_path):
    # the fourth round's base run is a tenth slow, which takes its pair 5%
    # below the others. A seventh round takes its place and goes candidate
    # first, as the fourth did, so the kept rounds stay three and three
    base = fake_engine(tmp_path, "base", [100, 100, 100, 90, 100, 100, 100])
    candidate = fake_engine(tmp_path, "candidate", [101] * 7)
    measured = speed.measure(str(base), str(candidate), rounds=6, depth=1)
    assert measured.rounds == [1, 2, 3, 5, 6, 7]
    assert measured.base_nps == [100] * 6
    assert measured.candidate_nps == [101] * 6
    assert measured.replaced == [(4, 90, 101)]
    calls = (tmp_path / "calls.log").read_text().split()
    assert calls[-2:] == ["candidate", "base"]


def test_a_round_is_chosen_by_its_pair_and_not_by_one_side(tmp_path):
    # the candidate's fourth run is 4% below its own median, past the cut on
    # its own. Choosing by one side would trim that side's low tail, which is
    # the ratio's tail; the pair is 2% below, inside the cut
    base = fake_engine(tmp_path, "base", [100] * 6)
    candidate = fake_engine(tmp_path, "candidate", [101, 101, 101, 97, 101, 101])
    measured = speed.measure(str(base), str(candidate), rounds=6, depth=1)
    assert measured.replaced == []


def test_the_faster_half_is_the_mean_of_the_faster_rounds():
    assert speed.faster_half([90, 100, 80, 110]) == 105
    # the middle round is kept when the count is odd
    assert speed.faster_half([90, 100, 80, 110, 70]) == 100
    # a loaded run moves it not at all, and one fast run by a share of it
    assert speed.faster_half([100, 100, 100, 100, 50]) == 100
    assert speed.faster_half([100, 100, 100, 100, 130]) == 110


def test_no_more_than_a_fifth_of_the_rounds_are_run_again(tmp_path):
    # two loaded rounds and room to replace one: the one furthest below goes
    base = fake_engine(tmp_path, "base", [90, 100, 100, 100, 100, 88, 100])
    candidate = fake_engine(tmp_path, "candidate", [101] * 7)
    measured = speed.measure(str(base), str(candidate), rounds=6, depth=1)
    assert measured.replaced == [(6, 88, 101)]
    assert measured.base_nps == [90, 100, 100, 100, 100, 100]
    assert len((tmp_path / "calls.log").read_text().split()) == 16


def test_a_real_change_is_not_a_loaded_round(tmp_path):
    # the candidate is a tenth slower every round, which moves its median
    # with it, so no run of it stands out
    base = fake_engine(tmp_path, "base", [100] * 6)
    candidate = fake_engine(tmp_path, "candidate", [90] * 6)
    measured = speed.measure(str(base), str(candidate), rounds=6, depth=1)
    assert measured.replaced == []


def test_a_cut_of_zero_runs_nothing_again(tmp_path):
    base = fake_engine(tmp_path, "base", [100, 100, 100, 50, 100, 100])
    candidate = fake_engine(tmp_path, "candidate", [101] * 6)
    measured = speed.measure(str(base), str(candidate), rounds=6, depth=1, cut=0)
    assert measured.replaced == []
    assert measured.base_nps == [100, 100, 100, 50, 100, 100]


def test_the_report_lists_the_rounds_run_again(tmp_path, capsys):
    base = fake_engine(tmp_path, "base", [100, 100, 100, 90, 100, 100, 100])
    candidate = fake_engine(tmp_path, "candidate", [101] * 7)
    assert speed.main([str(base), str(candidate), "--rounds", "6"]) == 0
    out = capsys.readouterr().out
    assert "run again, each pair more than 3% below the median pair:" in out
    assert "    4           90            101  +12.2%" in out
    assert "    7          100            101   +1.0%" in out


def test_the_sides_are_told_apart_even_when_they_are_one_binary(tmp_path):
    # an engine measured against itself is the first thing anyone tries
    engine = fake_engine(tmp_path, "engine", [100] * 6, nodes=100)
    measured = speed.measure(str(engine), str(engine), rounds=2, depth=1)
    assert (measured.base_nodes, measured.candidate_nodes) == (100, 100)


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
    # the tree loses a tenth of itself at the same cost a node, so the rate
    # says nothing happened while the search finishes a tenth sooner
    base = fake_engine(tmp_path, "base", [100] * 6, nodes=100)
    candidate = fake_engine(tmp_path, "candidate", [100] * 6, nodes=90)
    assert (
        speed.main(
            [str(base), str(candidate), "--rounds", "6", "--base-ref", "abc1234"]
        )
        == 0
    )
    out = capsys.readouterr().out
    assert "            nodes    time  median nps  faster half" in out
    assert "base          100  1.00 s         100          100" in out
    assert "candidate      90  0.90 s         100          100" in out
    assert "change     -10.0%  -10.0%       +0.0%        +0.0%" in out
    assert speed.COUNTS_DIFFER in out
    # the rates are over different trees, so no verdict is read off them
    assert "no change beyond" not in out
    # and the trailer goes on saying the one thing it has always said
    assert out.strip().endswith(
        "Speed: +0.0% (bench nps, 95% interval +0.0% to +0.0%, "
        "6 interleaved rounds vs abc1234)"
    )


def test_the_report_leaves_the_breakdown_out_when_the_counts_match(tmp_path, capsys):
    # with the tree held still the time is the inverse of the rate
    base = fake_engine(tmp_path, "base", [100] * 6, nodes=100)
    candidate = fake_engine(tmp_path, "candidate", [110] * 6, nodes=100)
    assert speed.main([str(base), str(candidate), "--rounds", "6"]) == 0
    out = capsys.readouterr().out
    assert "candidate    100  0.91 s         110          110" in out
    assert "change                        +10.0%       +10.0%" in out
    assert "paired change +10.0%, 95% interval +10.0% to +10.0%" in out
    assert speed.COUNTS_DIFFER not in out
    assert "faster: the whole interval is above +2.0%" in out


def test_the_faster_halves_are_compared_beside_the_medians(tmp_path, capsys):
    # the loaded rounds drag the medians apart while the faster halves still
    # says nothing changed
    base = fake_engine(tmp_path, "base", [100, 80] * 3, nodes=100)
    candidate = fake_engine(tmp_path, "candidate", [90, 100] * 3, nodes=100)
    # kept loaded, since the loaded rounds are the point
    argv = [str(base), str(candidate), "--rounds", "6", "--loaded", "0"]
    assert speed.main(argv) == 0
    out = capsys.readouterr().out
    assert "change                         +5.6%        +0.0%" in out


def test_the_verdict_stands_above_the_trailer(tmp_path, capsys):
    base = fake_engine(tmp_path, "base", [100] * 6, nodes=100)
    candidate = fake_engine(tmp_path, "candidate", [101] * 6, nodes=100)
    assert speed.main([str(base), str(candidate), "--rounds", "6"]) == 0
    out = capsys.readouterr().out
    assert "no change beyond ±2.0%" in out
    # the trailer stays the last line, which is what speed.sh pipes on
    assert out.strip().splitlines()[-1].startswith("Speed: +1.0% ")


def test_the_threshold_can_be_set(tmp_path, capsys):
    base = fake_engine(tmp_path, "base", [100] * 6, nodes=100)
    candidate = fake_engine(tmp_path, "candidate", [101] * 6, nodes=100)
    argv = [str(base), str(candidate), "--rounds", "6", "--threshold", "0.5"]
    assert speed.main(argv) == 0
    assert "faster: the whole interval is above +0.5%" in capsys.readouterr().out


def test_fewer_than_six_rounds_is_refused(tmp_path, capsys):
    base = fake_engine(tmp_path, "base", [100] * 5)
    candidate = fake_engine(tmp_path, "candidate", [100] * 5)
    with pytest.raises(SystemExit) as left:
        speed.main([str(base), str(candidate), "--rounds", "5"])
    assert left.value.code == 2
    assert "six rounds" in capsys.readouterr().err


def test_an_engine_that_prints_no_bench_is_named_rather_than_a_traceback(tmp_path):
    quiet = tmp_path / ("quiet.cmd" if sys.platform == "win32" else "quiet")
    quiet.write_text("@echo off\r\n" if sys.platform == "win32" else "#!/bin/sh\n")
    quiet.chmod(0o755)
    with pytest.raises(SystemExit) as left:
        speed.measure(str(quiet), str(quiet), rounds=2, depth=1)
    assert "no bench" in str(left.value)


def test_the_trailer_passes_the_hook():
    import check_trailers

    # an interval from below zero to above it, so both signs are printed
    line = speed.trailer(
        [100, 101, 99, 100, 102, 98], [104, 99, 105, 101, 97, 103], "a1b2c3d"
    )
    assert " interval -" in line and " to +" in line
    message = f"perf(search): Sort less\n\nBench: 1\n{line}\n"
    assert check_trailers.problems(message) == []


@pytest.mark.skipif(
    sys.platform == "win32", reason="runs a shell script, which windows cannot"
)
def test_the_wrapper_builds_the_base_commit_and_measures_against_it(tmp_path):
    # a repository of two commits and a cargo that "builds" by writing a fake
    # engine wherever it is told to
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
    # the base is head: the trailer is produced before the commit exists
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
            "ROUNDS": "6",
            "LAYOUTS": "off",
        },
        check=False,
        capture_output=True,
        text=True,
    )
    assert result.returncode == 0, result.stderr
    assert result.stdout.strip().endswith(
        "Speed: +0.0% (bench nps, 95% interval +0.0% to +0.0%, "
        f"6 interleaved rounds vs {base})"
    )
    # the base binary is kept for the next measurement
    assert (repo / "target" / "speed" / base / "arche").exists()


@pytest.mark.skipif(
    sys.platform == "win32", reason="runs a shell script, which windows cannot"
)
def test_the_wrapper_measures_over_layouts_by_default(tmp_path):
    # a cargo that "builds" a fake engine and prints a link command, and a
    # linker that copies the engine to wherever -o says and keeps the rest of
    # its arguments beside it
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
    git("add", ".")
    git("commit", "-qm", "first")
    base = git("rev-parse", "--short", "HEAD")

    shims = tmp_path / "shims"
    shims.mkdir()
    engine = shims / "engine"
    engine.write_text("#!/usr/bin/env bash\necho 100 nodes 1000 nps\n")
    engine.chmod(0o755)
    linker = shims / "fakecc"
    linker.write_text(
        "#!/usr/bin/env bash\n"
        "while [ $# -gt 0 ]; do\n"
        '  if [ "$1" = -o ]; then out=$2; shift; else rest="$rest $1"; fi\n'
        "  shift\n"
        "done\n"
        f'cp "{engine}" "$out"\n'
        'echo "$rest" > "$out.args"\n'
    )
    linker.chmod(0o755)
    cargo = shims / "cargo"
    cargo.write_text(
        "#!/usr/bin/env bash\n"
        "dir=${CARGO_TARGET_DIR:-target}\n"
        'mkdir -p "$dir/release/deps"\n'
        f'cp "{engine}" "$dir/release/arche"\n'
        'case "$*" in *link-args*)\n'
        f'  echo "LC_ALL=\\"C\\" \\"{linker}\\" \\"-fuse-ld=lld\\" \\"-o\\" \\"$dir/release/deps/arche-0\\""\n'
        "esac\n"
    )
    cargo.chmod(0o755)

    result = subprocess.run(
        [str(SCRIPT)],
        cwd=repo,
        env={
            "PATH": f"{shims}:{Path(sys.executable).parent}:/usr/bin:/bin",
            "ROUNDS": "6",
        },
        check=False,
        capture_output=True,
        text=True,
    )
    assert result.returncode == 0, result.stderr
    assert result.stdout.strip().endswith(
        "Speed: +0.0% (bench nps, 95% interval +0.0% to +0.0%, "
        f"6 interleaved rounds over shuffled layouts vs {base})"
    )
    assert "diagnostic, on the default layout alone +0.0%" in result.stdout
    # one layout a round, each linked with its seed and
    # its offset, and the base's kept for the next measurement
    kept = repo / "target" / "speed" / f"{base}-shuffle"
    assert (kept / "mode").read_text().strip() == "shuffle"
    assert (kept / "6").exists() and not (kept / "7").exists()
    args = (kept / "3.args").read_text()
    assert "-Wl,--shuffle-sections=*=3" in args
    assert "-Wl,-T," in args


def layouts_dir(directory, name, rates, mode="shuffle", nodes=100):
    """A directory as layouts.sh leaves it, of fake engines that each print
    the rates given for them in turn."""
    side = directory / name
    side.mkdir()
    (side / "mode").write_text(mode + "\n")
    for i, rate in enumerate(rates):
        fake_engine(side, str(i + 1), [rate], nodes=nodes)
    fake_engine(side, "default", [rates[0]] * 20, nodes=nodes)
    return side


@pytest.mark.skipif(sys.platform == "win32", reason="a layout has no .cmd name")
def test_round_n_runs_layout_n_on_both_sides(tmp_path, capsys):
    base = layouts_dir(tmp_path, "base", [100] * 7)
    candidate = layouts_dir(tmp_path, "candidate", [100, 102, 104, 106, 108, 110, 0])
    argv = [str(base), str(candidate), "--rounds", "6", "--base-ref", "abc1234"]
    assert speed.main(argv) == 0
    out = capsys.readouterr().out
    # the candidate's layouts in order, one a round, the seventh never needed
    for n, rate in enumerate([100, 102, 104, 106, 108, 110], 1):
        assert f"{n:>5} {100:>12} {rate:>14}" in out
    assert "6 interleaved rounds over shuffled layouts vs abc1234)" in out
    # over layouts the threshold is the smaller one
    assert "±1.0%" in out


@pytest.mark.skipif(sys.platform == "win32", reason="a layout has no .cmd name")
def test_layouts_of_different_kinds_are_not_paired(tmp_path, capsys):
    base = layouts_dir(tmp_path, "base", [100] * 7, mode="shuffle")
    candidate = layouts_dir(tmp_path, "candidate", [100] * 7, mode="pad")
    with pytest.raises(SystemExit) as left:
        speed.main([str(base), str(candidate), "--rounds", "6"])
    assert left.value.code == 2
    assert "shuffle" in capsys.readouterr().err


@pytest.mark.skipif(sys.platform == "win32", reason="a layout has no .cmd name")
def test_too_few_layouts_are_named(tmp_path):
    # six rounds want six layouts, since over layouts no round is run again
    # unless asked, and asking for it wants one more to run it on
    base = layouts_dir(tmp_path, "base", [100] * 5)
    candidate = layouts_dir(tmp_path, "candidate", [100] * 5)
    with pytest.raises(SystemExit) as left:
        speed.main([str(base), str(candidate), "--rounds", "6"])
    assert "6 layouts needed" in str(left.value)
    with pytest.raises(SystemExit) as left:
        speed.main([str(base), str(candidate), "--rounds", "6", "--loaded", "3"])
    assert "7 layouts needed" in str(left.value)


@pytest.mark.skipif(
    sys.platform == "win32", reason="runs a shell script, which windows cannot"
)
def test_layouts_refuse_a_directory_they_did_not_make(tmp_path):
    # a slip of the argument must not empty a checkout
    precious = tmp_path / "checkout"
    precious.mkdir()
    (precious / "keep").write_text("mine")
    script = Path(__file__).resolve().parent.parent / "layouts.sh"
    result = subprocess.run(
        [str(script), "HEAD", str(precious), "4", "shuffle"],
        check=False,
        capture_output=True,
        text=True,
    )
    assert result.returncode == 2
    assert "not made by layouts.sh" in result.stderr
    assert (precious / "keep").read_text() == "mine"
    result = subprocess.run(
        [str(script), "HEAD", str(tmp_path / "a#b"), "4", "shuffle"],
        check=False,
        capture_output=True,
        text=True,
    )
    assert result.returncode == 2
    assert "cannot carry" in result.stderr


def test_a_layout_that_counts_other_nodes_stops_the_measurement(tmp_path):
    base = [str(fake_engine(tmp_path, f"b{i}", [100], nodes=100)) for i in range(6)]
    candidate = [
        str(fake_engine(tmp_path, f"c{i}", [100], nodes=100 if i < 3 else 90))
        for i in range(6)
    ]
    with pytest.raises(SystemExit) as left:
        speed.measure(base, candidate, rounds=6, depth=1, cut=0)
    assert "not the same search" in str(left.value)


def test_a_binary_and_a_directory_are_not_paired(tmp_path, capsys):
    engine = fake_engine(tmp_path, "engine", [100])
    with pytest.raises(SystemExit) as left:
        speed.main([str(engine), str(tmp_path), "--rounds", "6"])
    assert left.value.code == 2
    assert "both sides" in capsys.readouterr().err
