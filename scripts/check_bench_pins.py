#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2022-2026 Andrew Wright

"""Check every commit's stated bench against the counts its own tree pins.

    check_bench_pins.py <base> <head>

The bench is the sum of the per position counts that
`node_counts_have_not_moved` pins, because the test runs the suite at the
same depth, table size and config the bare command does, and the last line
the command prints is that sum. The file holding the pins is found in each
commit's own tree rather than named here, so a crate rename does not
strand the check. So a commit's tree
already states its own bench, twice: once in the message and once in the pins.
This compares the two, reading both out of the commit rather than building it.

That makes it cheap enough to run on a push to master, which is where the
gap was. The bench workflow verifies a pull request's commits by building
each one, but a commit that is rebased before it lands is not the commit that
was verified: if the base changed what the bench counts, the message that
passed is stale by the time it arrives. Nothing rebuilt it afterwards, and
`f1f0730` and `5b12f03` reached master saying 36130893 with trees counting
35561814.

What this does not check is whether the pins themselves are true, which is
the other half. `cargo test --release` runs `node_counts_have_not_moved` on
every push to master already, so between the two a landed commit's message,
its pins and its tree all have to agree.
"""

import re
import subprocess
import sys

PINNED_TEST = "fn node_counts_have_not_moved"

# ("some position", 1_234_567), as the pinned list writes them
PIN = re.compile(r'\(\s*"[^"]*"\s*,\s*([\d_]+)\s*\)')


def run(*args: str, no_match_is_an_answer: bool = False) -> str:
    """Ask git something, or die saying what was asked.

    git grep exits one when it found nothing, which is an answer to the one
    caller that asks it, not a death.
    """
    done = subprocess.run(["git", *args], capture_output=True, text=True, check=False)
    if done.returncode > (1 if no_match_is_an_answer else 0):
        sys.exit(f"git {' '.join(args)}: {done.stderr.strip()}")
    return done.stdout


def stated_bench(sha: str) -> str | None:
    """The Bench trailer, read the way git reads it.

    Git prints a value a line and a blank line after them, so the last line
    with anything on it is the last Bench trailer, which is the one that
    counts: openbench reads the last, and check_trailers.py holds a commit to
    stating no other bench-like number after it.
    """
    values = [
        line
        for line in run(
            "log", "-1", "--format=%(trailers:key=Bench,valueonly)", sha
        ).splitlines()
        if line.strip()
    ]
    return values[-1].strip() if values else None


def pins_path(sha: str) -> str:
    """The one file in this commit's tree that holds the pinned test."""
    listed = run(
        "grep", "-l", PINNED_TEST, sha, "--", "*.rs", no_match_is_an_answer=True
    )
    hits = [line.split(":", 1)[1] for line in listed.splitlines() if line.strip()]
    if len(hits) != 1:
        found = ", ".join(hits) if hits else "none"
        sys.exit(
            f"{sha[:7]}: expected one file holding {PINNED_TEST}, found "
            f"{found}. The pins have moved; this check reads them wherever "
            "they live, but they must live in one place."
        )
    return hits[0]


def pinned_total(sha: str) -> int:
    """The sum of the counts pinned at this commit."""
    source = run("show", f"{sha}:{pins_path(sha)}")
    body = source.split(PINNED_TEST, 1)[1]
    # only this test's own list: the next one pins the reference search, and
    # the two are written the same way, so the walk has to stop at the
    # bracket that closes this one rather than at the next thing that looks
    # like an end
    start = body.index("vec![") + len("vec![")
    depth = 1
    end = start
    while depth:
        if body[end] == "[":
            depth += 1
        elif body[end] == "]":
            depth -= 1
        end += 1
    counts = [int(n.replace("_", "")) for n in PIN.findall(body[start : end - 1])]
    if not counts:
        sys.exit(f"{sha[:7]}: found {PINNED_TEST} but no counts pinned in it.")
    return sum(counts)


def main() -> int:
    if len(sys.argv) != 3:
        sys.exit("usage: check_bench_pins.py <base> <head>")
    base, head = sys.argv[1], sys.argv[2]

    failed = False
    for sha in run("rev-list", "--reverse", f"{base}..{head}").split():
        subject = run("log", "-1", "--format=%s", sha).strip()
        stated = stated_bench(sha)
        if stated is None:
            print(f"{sha[:7]} {subject}: no bench stated")
            continue
        total = pinned_total(sha)
        if stated == str(total):
            print(f"{sha[:7]} {subject}: bench {stated} matches its pins")
        else:
            print(f"{sha[:7]} {subject}: bench stated {stated}, pins count {total}")
            failed = True
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
