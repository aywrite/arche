#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2022-2026 Andrew Wright

"""Count the bench's instructions at a run of master's commits, and report
how the count moved from each to the next.

A pull request's count is read against its own base, so a change too small
to notice is let through, and the next one after it, and the count can climb
over a month without any one comment showing it. This builds every commit in
a range with one compiler, in one run, and counts each under cachegrind as
instructions.py does. One compiler matters: a new rustc can move the count
as far as a commit does, so counts taken at different times are not a
series, and the range is counted again rather than kept.

Where a step leaves the node count alone the tree did not move, so the
change in instructions is the code's. Those steps multiplied together are
the drift in what the code costs over the range, with the search's changes
left out.

    instruction_history.py count <commit>... --out FILE [--depth D]
    instruction_history.py report FILE... [--ref REF | --commits LIST]

count builds each commit with build_at.sh and appends its counts to a csv. A
commit that fails to build or bench is named and skipped, and the command
fails at the end. report orders the rows of any number of those files along
the first parent line of REF, or along LIST, a file of the shas that were
meant to be counted, and prints the table in markdown. Given LIST, a commit
on it with no row is a gap, and the step across it is not read.

build_at.sh exports each commit under CARGO_TARGET_DIR, and inside the
checkout the checkout's own .cargo/config.toml applies to every commit as
well as the commit's. A CARGO_TARGET_DIR outside the checkout, as the
workflow sets, builds each commit with its own config alone.
"""

import argparse
import csv
import hashlib
import os
import pathlib
import subprocess
import sys
from dataclasses import dataclass

import instructions

FIELDS = ["sha", "instructions", "nodes", "md5", "rustc"]


@dataclass
class Row:
    sha: str
    instructions: int
    nodes: int
    md5: str
    rustc: str


@dataclass
class Commit:
    subject: str
    # the Speed trailer's estimate, or empty where the commit has none
    speed: str


def git(*args: str) -> str:
    return subprocess.run(
        ["git", *args], check=True, capture_output=True, text=True
    ).stdout


def measure(commit: str, valgrind: str, depth: int | None) -> list[str]:
    """One commit's csv row."""
    # where build_at.sh puts the export
    target = pathlib.Path(os.environ.get("CARGO_TARGET_DIR", "target"))
    sha = git("rev-parse", "--verify", f"{commit}^{{commit}}").strip()
    binary = target / "history" / sha
    subprocess.run(["scripts/build_at.sh", sha, str(binary)], check=True)
    # the compiler the export's rust-toolchain.toml chose
    rustc = subprocess.run(
        ["rustc", "--version"],
        cwd=target / "at" / "src",
        check=True,
        capture_output=True,
        text=True,
    ).stdout.strip()
    counted = instructions.count(valgrind, str(binary), depth)
    md5 = hashlib.md5(binary.read_bytes()).hexdigest()
    return [sha, str(counted.instructions), str(counted.nodes), md5, rustc]


def count(args: argparse.Namespace) -> int:
    new = not args.out.exists()
    failed = []
    with args.out.open("a", newline="") as out:
        writer = csv.writer(out)
        if new:
            writer.writerow(FIELDS)
        for commit in args.commits:
            try:
                row = measure(commit, args.valgrind, args.depth)
            except (subprocess.CalledProcessError, SystemExit) as error:
                # the rest are still worth counting, and report shows the gap
                print(f"{commit}: not counted: {error}", file=sys.stderr, flush=True)
                failed.append(commit)
                continue
            writer.writerow(row)
            out.flush()
            print(f"{row[0][:10]} {row[1]} {row[2]}", flush=True)
    if failed:
        print(f"{len(failed)} not counted: {' '.join(failed)}", file=sys.stderr)
        return 1
    return 0


def read(paths: list[pathlib.Path]) -> list[Row]:
    rows = []
    for path in paths:
        with path.open(newline="") as file:
            for record in csv.DictReader(file):
                rows.append(
                    Row(
                        record["sha"],
                        int(record["instructions"]),
                        int(record["nodes"]),
                        record["md5"],
                        record["rustc"],
                    )
                )
    return rows


def along(rows: list[Row], line: list[str]) -> list[Row]:
    """The rows in the order of `line`, oldest first, one a commit. A row
    off the line is dropped, since there is no step to read it against."""
    place = {sha: i for i, sha in enumerate(line)}
    kept = {row.sha: row for row in rows if row.sha in place}
    return sorted(kept.values(), key=lambda row: place[row.sha])


def commits(shas: list[str]) -> dict[str, Commit]:
    if not shas:
        return {}
    text = git(
        "show",
        "-s",
        "--format=%H%x1f%s%x1f%(trailers:key=Speed,valueonly,separator=%x20)%x1e",
        *shas,
    )
    return described(text)


def described(text: str) -> dict[str, Commit]:
    """git's records, a sha, a subject and a Speed trailer each. Only the
    newlines between records come off: str.strip counts the separators as
    whitespace, and would take an empty trailer's separator with it."""
    found = {}
    for record in text.split("\x1e"):
        fields = record.strip("\n").split("\x1f")
        if len(fields) == 3:
            found[fields[0]] = Commit(fields[1], fields[2].split(" (")[0].strip())
    return found


def percent(before: float, after: float) -> float:
    return 100.0 * (after - before) / before


def report(
    rows: list[Row], described: dict[str, Commit], meant: list[str] | None = None
) -> list[str]:
    """The table, each commit with its step from the one before, and the
    drift over the steps that left the tree alone. Given the commits that
    were `meant` to be counted, in order, a step from a row that is not the
    one just before on that list crosses a gap and is not read."""
    counted = {row.sha for row in rows}
    previous = {}
    if meant is not None:
        previous = {sha: meant[i - 1] for i, sha in enumerate(meant) if i > 0}
    lines = ["### Instructions along master", ""]
    if not rows:
        return lines + ["No commit in the range was counted."]
    compilers = sorted({row.rustc for row in rows})
    if len(compilers) == 1:
        lines.append(f"Every commit built with {compilers[0]}.")
    else:
        lines.append(
            "The commits did not all build with one compiler ("
            + ", ".join(compilers)
            + "), so a step across two of them is the compiler's as well."
        )
    lines += [
        "",
        "| commit | nodes | instructions | per node | step | speed trailer | subject |",
        "|---|---:|---:|---:|---:|---:|---|",
    ]
    drift = 1.0
    steps = 0
    for before, row in zip([None, *rows], rows):
        if before is None:
            step = ""
        elif meant is not None and previous.get(row.sha) != before.sha:
            step = "after a gap"
        elif row.md5 == before.md5:
            step = "same binary"
        elif row.nodes == before.nodes:
            step = f"{percent(before.instructions, row.instructions):+.2f}%"
            drift *= row.instructions / before.instructions
            steps += 1
        else:
            # the tree moved, so only the cost of a node compares
            step = (
                f"{percent(before.instructions / before.nodes, row.instructions / row.nodes):+.2f}%"
                " a node"
            )
        commit = described.get(row.sha, Commit("", ""))
        lines.append(
            f"| {row.sha[:8]} | {row.nodes:,} | {row.instructions:,} "
            f"| {row.instructions / row.nodes:,.1f} | {step} | {commit.speed} "
            f"| {commit.subject[:72]} |"
        )
    lines += [
        "",
        (
            f"Over the {steps} steps that changed the code and left the node "
            f"count alone, the instructions moved {100.0 * (drift - 1.0):+.2f}% "
            "in all. A step that changed the node count changed the tree, and "
            "its per node change is the tree's as much as the code's, so it is "
            "left out of that."
        ),
    ]
    missing = [sha for sha in meant or [] if sha not in counted]
    if missing:
        lines += [
            "",
            "Not counted, so the steps across them are not read: "
            + ", ".join(sha[:8] for sha in missing)
            + ".",
        ]
    return lines


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    commands = parser.add_subparsers(dest="command", required=True)
    counting = commands.add_parser("count")
    counting.add_argument("commits", nargs="+")
    counting.add_argument("--out", type=pathlib.Path, required=True)
    counting.add_argument("--depth", type=int, default=None)
    counting.add_argument("--valgrind", default="valgrind")
    reporting = commands.add_parser("report")
    reporting.add_argument("files", nargs="+", type=pathlib.Path)
    line_of = reporting.add_mutually_exclusive_group()
    line_of.add_argument("--ref", default="HEAD")
    line_of.add_argument("--commits", type=pathlib.Path)
    args = parser.parse_args(argv)

    if args.command == "count":
        return count(args)
    meant = args.commits.read_text().split() if args.commits else None
    line = meant or git("rev-list", "--first-parent", "--reverse", args.ref).split()
    rows = along(read(args.files), line)
    for text in report(rows, commits([row.sha for row in rows]), meant):
        print(text)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
