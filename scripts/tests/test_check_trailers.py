# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2022-2026 Andrew Wright

"""Tests for the commit message trailer check.

A commit that changes the engine has to say what the bench counts after it,
a commit that claims speed has to say how much it measured, and an elo claim
has to be in the one shape the changelog and the release notes read. The
check runs as a commit-msg hook, so what is under test is which messages it
lets through and what it says about the ones it does not.
"""

import check_trailers

ENGINE = "fix(search): Finish depth one before the clock can stop the search"
BUILD = "feat(ci): Require a bench on engine commits"


def problems(message):
    return check_trailers.problems(message)


def test_an_engine_commit_needs_a_bench():
    found = problems(f"{ENGINE}\n\nA body.\n")
    assert found == ["an engine commit needs a Bench: trailer"]


def test_a_bench_trailer_satisfies_an_engine_commit():
    assert problems(f"{ENGINE}\n\nA body.\n\nBench: 42847751\n") == []


def test_a_build_commit_needs_nothing():
    assert problems(f"{BUILD}\n\nA body.\n") == []


def test_every_engine_scope_and_type_is_covered():
    for scope in ["board", "eval", "magic", "search", "zobrist"]:
        for kind in ["feat", "fix", "perf", "refactor"]:
            message = f"{kind}({scope}): Do a thing\n"
            assert problems(message), f"{kind}({scope}) was let through"
    for kind in ["docs", "test", "chore", "style"]:
        assert problems(f"{kind}(search): Do a thing\n") == []


def test_a_bench_must_be_exactly_digits():
    # the ci check reads the line with git's trailer parser and compares the
    # value as printed, so the hook refuses what that would not match:
    # commas, and whitespace on either side
    for value in ["42,847,751", "5 ", " 5", "5\t"]:
        assert problems(f"{ENGINE}\n\nBench: {value}\n") == [
            f"Bench: must be a plain number, got {value}"
        ], repr(value)


def test_the_bench_trailer_must_be_the_last_bench_number_in_the_message():
    # openbench reads the last `bench <number>` anywhere in the message, so
    # a later trailer with one in it would be read in place of the bench
    message = f"{ENGINE}\n\nBench: 42847751\nNote: the old bench 9 positions are gone\n"
    found = problems(message)
    assert found == [
        (
            "Bench: 42847751 is not the last bench number in the message, "
            "which openbench reads: 9"
        )
    ]


def test_a_node_count_in_the_body_is_read_the_same_way():
    message = f"{ENGINE}\n\nThe search visits 4571056 nodes.\n\nBench: 42847751\n"
    assert problems(message) == []
    message = f"{ENGINE}\n\nBench: 42847751\n\nNow nodes 4571056 are visited.\n"
    assert problems(message)


def test_a_perf_commit_needs_a_speed_as_well():
    message = "perf(search): Sort less\n\nBench: 42847751\n"
    assert problems(message) == ["a perf commit needs a Speed: trailer"]


def test_a_perf_commit_on_the_build_needs_neither():
    assert problems("perf(bench): Stop timing the memset\n") == []


def test_the_speed_format_is_fixed():
    good = (
        "perf(search): Sort less\n\nBench: 42847751\n"
        "Speed: +3.1% (bench nps, 5 interleaved rounds vs a1b2c3d, spread 2.4%)\n"
    )
    assert problems(good) == []
    bad = "perf(search): Sort less\n\nBench: 42847751\nSpeed: faster\n"
    assert problems(bad) == [
        "Speed: is not in the shape scripts/speed.sh prints, got faster"
    ]


def test_the_elo_format_is_fixed_when_present():
    for value in [
        "+12 ±8 (sprt [0, 10] passed, 1240 games, 10+0.1, vs v0.3.10)",
        "-2 ±9 (sprt [0, 10] failed, 1240 games, 10+0.1, vs v0.3.10)",
        "+12 ±8 (sprt [-1.5, 2.5] inconclusive, 100 games, 10+0.1, vs master)",
        "-3 ±11 (500 games, 10+0.1, vs master)",
        "not measured",
    ]:
        assert problems(f"{ENGINE}\n\nBench: 1\nElo: {value}\n") == [], value
    for value in [
        "about ten",
        # sprt bounds with no verdict: the estimate alone is not the result
        "+12 ±8 (sprt [0, 10], 1240 games, 10+0.1, vs v0.3.10)",
    ]:
        assert problems(f"{ENGINE}\n\nBench: 1\nElo: {value}\n") == [
            f"Elo: is not in the shape the strength workflow prints, got {value}"
        ], value


def test_merges_fixups_and_the_release_bump_are_exempt():
    for message in [
        "Merge branch 'feature'\n",
        "fixup! fix(search): Finish depth one\n",
        "squash! perf(search): Sort less\n",
        "chore(release): prepare for 0.4.0\n",
    ]:
        assert problems(message) == [], message


def test_only_the_final_paragraph_holds_trailers_as_git_reads_it():
    # a Bench line followed by more prose is not a trailer to git, git-cliff
    # or the ci check, so it must not be one to the hook either
    message = f"{ENGINE}\n\nBench: 5\n\nMore prose after.\n"
    assert problems(message) == ["an engine commit needs a Bench: trailer"]
    # and other trailers in the same final paragraph are fine
    message = f"{ENGINE}\n\nA body.\n\nBench: 5\nCo-Authored-By: Someone <s@x>\n"
    assert problems(message) == []


def test_a_speed_of_one_round_is_not_a_measurement():
    line = "Speed: +3.1% (bench nps, 1 interleaved rounds vs a1b2c3d, spread 0.0%)"
    assert problems(f"perf(search): x\n\nBench: 1\n{line}\n") == [
        f"Speed: is not in the shape scripts/speed.sh prints, got {line[7:]}"
    ]


def test_a_revert_of_an_engine_change_needs_a_bench():
    assert problems("revert(search): Undo the sort\n") == [
        "an engine commit needs a Bench: trailer"
    ]


def test_comment_lines_are_ignored():
    # git hands the hook the message with its commented instructions still in
    message = f"{ENGINE}\n\nBench: 1\n# Please enter the commit message\n# bench 99\n"
    assert problems(message) == []


def test_the_hook_reads_the_file_and_exits_nonzero_on_a_problem(tmp_path, capsys):
    path = tmp_path / "COMMIT_EDITMSG"
    path.write_text(f"{ENGINE}\n\nA body.\n")
    assert check_trailers.main([str(path)]) == 1
    assert "needs a Bench: trailer" in capsys.readouterr().err
    path.write_text(f"{ENGINE}\n\nBench: 1\n")
    assert check_trailers.main([str(path)]) == 0
