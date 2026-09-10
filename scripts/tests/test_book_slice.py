# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2022-2026 Andrew Wright

"""Tests for the slice of the opening book a shard plays.

The shards of a match are pooled afterwards as one sample, so what this pins
is that no two of them are handed the same opening, whatever the seed, and
that a run wanting more openings than the book holds says so rather than
quietly overlapping.
"""

import subprocess
import sys

from conftest import SCRIPTS
from match_tools import book_slice

# the shim at the old path, which is what the workflows run
SCRIPT = SCRIPTS / "book_slice.py"

# what 8moves_v3.pgn holds, counted at run time by the workflow
BOOK = 34700


def openings_of(pairs, shards, seed, book=BOOK):
    """The openings every shard of a run would play, as sets."""
    return [
        set(
            range(
                book_slice.start(book, pairs, shards, shard, seed),
                book_slice.start(book, pairs, shards, shard, seed) + pairs,
            )
        )
        for shard in range(shards)
    ]


def run(*arguments):
    return subprocess.run(
        [sys.executable, str(SCRIPT), *[str(argument) for argument in arguments]],
        check=False,
        capture_output=True,
        text=True,
    )


def test_no_two_shards_are_given_the_same_opening():
    played = openings_of(pairs=100, shards=5, seed=16093711234)
    pooled = set().union(*played)
    assert len(pooled) == 500
    assert sorted(pooled) == sorted(range(min(pooled), min(pooled) + 500))


def test_every_shard_stays_inside_the_book():
    # the offset leaves room for the whole run, so the last shard's last
    # opening is still one the book has
    for seed in [0, 1, 7, 34699, 34700, 16093711234]:
        played = openings_of(pairs=100, shards=5, seed=seed)
        assert min(min(shard) for shard in played) >= 1, seed
        assert max(max(shard) for shard in played) <= BOOK, seed


def test_a_run_id_sized_seed_is_a_remainder_and_not_an_overflow():
    # the seed is the run id when nobody chose one, which is eleven digits
    start = book_slice.start(BOOK, 100, 5, 0, 16093711234)
    assert start == 1 + 16093711234 % (BOOK - 500)
    assert 1 <= start <= BOOK - 500


def test_the_start_is_one_based_the_way_fastchess_counts():
    # fastchess refuses a start under one, and reads start 1 as the first
    # opening in the file
    assert book_slice.start(BOOK, 1, 1, 0, 0) == 1


class TestCommandLine:
    def test_the_index_is_all_that_goes_to_stdout(self):
        result = run(
            "--openings", BOOK, "--pairs", 100, "--shards", 5, "--shard", 2, "--seed", 0
        )
        assert result.returncode == 0
        assert result.stdout == "201\n"
        assert result.stderr == ""

    def test_a_seed_written_with_a_leading_zero_is_read_as_decimal(self):
        # bash arithmetic would take 08 for octal and refuse it
        leading = run(
            "--openings",
            BOOK,
            "--pairs",
            2,
            "--shards",
            2,
            "--shard",
            0,
            "--seed",
            "08",
        )
        plain = run(
            "--openings", BOOK, "--pairs", 2, "--shards", 2, "--shard", 0, "--seed", 8
        )
        assert leading.returncode == 0
        assert leading.stdout == plain.stdout == "9\n"

    def test_a_book_too_small_for_the_run_warns_and_starts_at_the_front(self):
        result = run(
            "--openings", 300, "--pairs", 100, "--shards", 5, "--shard", 1, "--seed", 99
        )
        # still a match, since fastchess reads on around the end of the book
        assert result.returncode == 0
        assert result.stdout == "101\n"
        assert "the shards repeat each other" in result.stderr

    def test_a_shard_outside_the_run_is_an_error(self):
        result = run(
            "--openings", BOOK, "--pairs", 100, "--shards", 5, "--shard", 5, "--seed", 1
        )
        assert result.returncode != 0
        assert "not one of 5" in result.stderr

    def test_a_negative_seed_is_an_error(self):
        result = run(
            "--openings",
            BOOK,
            "--pairs",
            100,
            "--shards",
            5,
            "--shard",
            0,
            "--seed",
            -1,
        )
        assert result.returncode != 0
