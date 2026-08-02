#!/usr/bin/env python3
"""Turns a criterion comparison into a markdown table.

Reads the output of `cargo bench -- --baseline <name>` and writes the change
for each benchmark, worst first. Criterion prints its verdict per benchmark but
buries it in several hundred lines of sampling chatter, and the interesting
part of a comparison is the handful of entries that moved.
"""

import re
import sys

# criterion prints the benchmark id on a line of its own, then indents the
# measurements underneath it
ID = re.compile(r"^(\S.*?)\s*$")
TIME = re.compile(r"^\s+time:\s+\[\S+ \S+ (\S+ \S+) \S+ \S+\]")
CHANGE = re.compile(r"^\s+change:\s+\[\S+ (\S+) \S+\]")
VERDICT = re.compile(
    r"^\s+(Performance has regressed|Performance has improved"
    r"|No change in performance detected|Change within noise threshold)"
)

SKIP = ("Benchmarking", "Found", "Warning", "Gnuplot", "gnuplot")

LABEL = {
    "Performance has regressed": "slower",
    "Performance has improved": "faster",
    "No change in performance detected": "no change",
    "Change within noise threshold": "noise",
}


def parse(lines):
    """Yields (benchmark, time, percent change, verdict) in file order.

    A benchmark the baseline does not have prints no change and no verdict, so
    entries are flushed when the next one starts rather than when a verdict
    arrives. A benchmark added by the branch being measured is worth listing.
    """
    name = time = change = verdict = None

    def flush():
        if name is not None and time is not None:
            return (name, time, change, verdict or "new")
        return None

    for line in lines:
        if match := TIME.match(line):
            time = match.group(1)
        elif match := CHANGE.match(line):
            # criterion writes a minus sign, not a hyphen
            change = match.group(1).replace("−", "-")
        elif match := VERDICT.match(line):
            verdict = match.group(1)
        elif not line.startswith(SKIP) and (match := ID.match(line)):
            if record := flush():
                yield record
            name, time, change, verdict = match.group(1), None, None, None
    if record := flush():
        yield record


def percent(change):
    try:
        return abs(float(change.rstrip("%")))
    except (AttributeError, ValueError):
        return 0.0


def main():
    results = list(parse(sys.stdin))
    if not results:
        print("No benchmark comparison was produced.")
        return 1

    moved = [r for r in results if r[3] == "Performance has regressed"]
    print("Both sides were measured on the same runner in the same job, minutes")
    print("apart. Criterion still calls small differences significant on a shared")
    print("runner, so treat single digit changes as noise.\n")
    if moved:
        print(f"{len(moved)} of {len(results)} benchmarks are slower.\n")
    else:
        print(f"None of the {len(results)} benchmarks are slower.\n")

    print("| benchmark | time | change | |")
    print("| --- | --- | --- | --- |")
    for name, time, change, verdict in sorted(
        results, key=lambda r: percent(r[2]), reverse=True
    ):
        label = LABEL.get(verdict, verdict)
        print(f"| `{name}` | {time or ''} | {change or 'no baseline'} | {label} |")
    return 0


if __name__ == "__main__":
    sys.exit(main())
