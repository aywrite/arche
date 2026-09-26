#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2022-2026 Andrew Wright

"""Check every commit's stated bench against the counts its own tree pins.

    check_bench_pins.py <base> <head> [--acknowledged <file>]

The bench is the sum of the per position counts `node_counts_have_not_moved`
pins, since the test runs the suite at the same depth, table size and config
the bare command does. So a commit states its bench twice, in the message and
in the pins, and this compares the two without building anything. The file
holding the pins is found in each commit's own tree, so a crate rename does
not strand the check.

Cheap enough to run on a push to master, which is where the gap was: the
bench workflow builds a pull request's commits, but a commit rebased before
it lands is not the commit that was built, and `f1f0730` and `5b12f03`
reached master saying 36130893 with trees counting 35561814.

A commit on master cannot be rewritten, so a lapse that is history would fail
every release after it. `acknowledged_bench_pins.txt` beside this script
lists them as `<sha> <stated> <pinned>` lines a run reports and steps over;
the numbers are part of the entry, so a commit acknowledged at one pair of
figures is not acknowledged at another.

Whether the pins themselves are true is the other half, which `cargo test
--release` checks on every push to master.
"""

import argparse
import re
import subprocess
import sys
from pathlib import Path

PINNED_TEST = "fn node_counts_have_not_moved"

ACKNOWLEDGED = Path(__file__).resolve().parent / "acknowledged_bench_pins.txt"

# ("some position", 1_234_567), as the pinned list writes them
PIN = re.compile(r'\(\s*"[^"]*"\s*,\s*([\d_]+)\s*\)')


def run(*args: str, no_match_is_an_answer: bool = False) -> str:
    """Ask git something, or die saying what was asked. git grep exits one on
    finding nothing, which is an answer to the one caller that asks it."""
    done = subprocess.run(["git", *args], capture_output=True, text=True, check=False)
    if done.returncode > (1 if no_match_is_an_answer else 0):
        sys.exit(f"git {' '.join(args)}: {done.stderr.strip()}")
    return done.stdout


def stated_bench(sha: str) -> str | None:
    """The last Bench trailer, read the way git reads it, which is the one
    openbench reads. Git prints a blank line after the values."""
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
    # only this test's list: the next test pins the reference search the same
    # way, so the walk stops at the bracket that closes this one
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


def acknowledged(path: Path) -> list[tuple[str, str, str]]:
    """The lapses the list names, as `(sha, stated, pinned)`. Blank lines and
    `#` comments are skipped; anything else that is not three fields is an
    error, since a typo would otherwise turn the check off silently."""
    if not path.exists():
        sys.exit(f"{path}: no acknowledgement list there")
    entries = []
    for number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        fields = line.split()
        if len(fields) != 3 or not re.fullmatch(r"[0-9a-f]{7,40}", fields[0]):
            sys.exit(f"{path}:{number}: not a <sha> <stated> <pinned> line: {line}")
        entries.append((fields[0], fields[1], fields[2]))
    return entries


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("base", help="the commit to start after")
    parser.add_argument("head", help="the commit to stop at")
    parser.add_argument(
        "--acknowledged",
        type=Path,
        default=ACKNOWLEDGED,
        help="the list of lapses already on master, which are reported"
        " rather than failed on",
    )
    args = parser.parse_args()
    known = acknowledged(args.acknowledged)

    failed = False
    for sha in run("rev-list", "--reverse", f"{args.base}..{args.head}").split():
        subject = run("log", "-1", "--format=%s", sha).strip()
        stated = stated_bench(sha)
        if stated is None:
            print(f"{sha[:7]} {subject}: no bench stated")
            continue
        total = pinned_total(sha)
        if stated == str(total):
            print(f"{sha[:7]} {subject}: bench {stated} matches its pins")
        elif any(
            sha.startswith(listed) and (stated, str(total)) == (was, pins)
            for listed, was, pins in known
        ):
            print(
                f"{sha[:7]} {subject}: bench stated {stated}, pins count {total}"
                ", acknowledged"
            )
        else:
            print(f"{sha[:7]} {subject}: bench stated {stated}, pins count {total}")
            failed = True
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
