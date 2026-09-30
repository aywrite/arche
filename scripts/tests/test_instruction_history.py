# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2022-2026 Andrew Wright

"""Tests for the instruction history's ordering and report."""

import instruction_history
from instruction_history import Commit, Row

RUSTC = "rustc 1.99.0 (abcdef012 2026-09-01)"


def row(sha, instructions, nodes, md5=None):
    return Row(sha * 40, instructions, nodes, md5 or sha, RUSTC)


def test_rows_follow_the_line_and_one_off_it_is_dropped():
    rows = [row("c", 1, 1), row("a", 1, 1), row("x", 1, 1), row("b", 1, 1)]
    line = ["a" * 40, "b" * 40, "c" * 40]
    assert [r.sha[0] for r in instruction_history.along(rows, line)] == [
        "a",
        "b",
        "c",
    ]


def test_a_step_that_keeps_the_nodes_is_the_code_and_counts_to_the_drift():
    rows = [
        row("a", 1_000_000, 100),
        row("b", 1_010_000, 100),
        # the tree moved: the step is per node and stays out of the drift
        row("c", 2_020_000, 200),
        row("d", 2_000_000, 200),
    ]
    lines = instruction_history.report(rows, {})
    table = [line for line in lines if line.startswith("| ")][1:]
    assert "| +1.00% |" in table[1]
    assert "| +0.00% a node |" in table[2]
    assert "| -0.99% |" in table[3]
    assert "Over the 2 steps" in lines[-1]
    # 1.01 * (2.00 / 2.02) is one exactly
    assert "moved +0.00% in all" in lines[-1]


def test_the_same_binary_is_said_rather_than_read():
    rows = [row("a", 1_000_000, 100, md5="m"), row("b", 1_000_300, 100, md5="m")]
    lines = instruction_history.report(rows, {})
    assert "| same binary |" in lines[-3]
    assert "Over the 0 steps" in lines[-1]


def test_the_trailer_and_the_subject_are_shown():
    rows = [row("a", 1_000_000, 100), row("b", 990_000, 100)]
    described = {"b" * 40: Commit("perf(search): Do less", "+0.7%")}
    line = instruction_history.report(rows, described)[-3]
    assert line.endswith("| -1.00% | +0.7% | perf(search): Do less |")


def test_two_compilers_are_named_as_a_caveat():
    rows = [row("a", 1, 1), Row("b" * 40, 1, 1, "b", "rustc 2.0.0")]
    assert (
        "did not all build with one compiler" in instruction_history.report(rows, {})[2]
    )


def test_a_commit_without_a_trailer_keeps_its_subject():
    text = (
        "a" * 40
        + "\x1ffix(uci): Mend it\x1f\x1e\n"
        + "b" * 40
        + "\x1fperf(search): Do less\x1f+0.7% (bench nps)\x1e\n"
    )
    assert instruction_history.described(text) == {
        "a" * 40: Commit("fix(uci): Mend it", ""),
        "b" * 40: Commit("perf(search): Do less", "+0.7%"),
    }


def test_a_step_across_an_uncounted_commit_is_a_gap_and_not_read():
    rows = [row("a", 1_000_000, 100), row("c", 900_000, 100)]
    meant = ["a" * 40, "b" * 40, "c" * 40]
    lines = instruction_history.report(rows, {}, meant)
    assert "| after a gap |" in lines[-5]
    assert "Over the 0 steps" in lines[-3]
    assert lines[-1] == (
        "Not counted, so the steps across them are not read: bbbbbbbb."
    )


def test_a_commit_that_fails_is_skipped_and_the_count_fails(
    tmp_path, monkeypatch, capsys
):
    def measure(commit, valgrind, depth):
        if commit == "bad":
            raise SystemExit("no bench")
        return [commit * 40, "10", "1", "m", RUSTC]

    monkeypatch.setattr(instruction_history, "measure", measure)
    out = tmp_path / "counts.csv"
    argv = ["count", "a", "bad", "c", "--out", str(out)]
    assert instruction_history.main(argv) == 1
    assert [r.sha[0] for r in instruction_history.read([out])] == ["a", "c"]
    assert "bad: not counted" in capsys.readouterr().err
    # a second call appends without a second header
    assert instruction_history.main(["count", "d", "--out", str(out)]) == 0
    assert len(instruction_history.read([out])) == 3
