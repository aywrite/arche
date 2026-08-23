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


def bench_last_line(text):
    """The nodes and rate from a bench's output, or None when there is none."""
    for line in reversed(text.strip().splitlines()):
        words = line.split()
        if len(words) == 4 and words[1] == "nodes" and words[3] == "nps":
            return int(words[0]), int(words[2])
    return None


def bench_table(base_text, pr_text):
    """The engine's own bench on each side of the pull request, as markdown.

    The node count is compared for being the same, not for size: a search
    change moves it and that is the commit's Bench trailer's business. The
    rate is the speed figure over a real search rather than a microbenchmark,
    read against the same noise as the rows above it.
    """
    base, pr = bench_last_line(base_text), bench_last_line(pr_text)
    lines = [
        "## Bench",
        "",
        "The engine's own bench on each side, on the same runner.",
        "",
        "| | base | pull request | change |",
        "| --- | --- | --- | --- |",
    ]
    if base is None or pr is None:
        missing = "not measured"
        lines.append(
            f"| nodes | {base[0] if base else missing} | {pr[0] if pr else missing} | |"
        )
        return "\n".join(lines) + "\n"
    moved = base[0] != pr[0]
    lines.append(f"| nodes | {base[0]} | {pr[0]} | {'moved' if moved else 'same'} |")
    change = 100.0 * (pr[1] - base[1]) / base[1]
    lines.append(f"| nps | {base[1]} | {pr[1]} | {change:+.1f}% |")
    if moved:
        lines += [
            "",
            (
                "The node counts moved, so this is a search change as well as "
                "whatever it does to the speed; the commit's Bench trailer says "
                "by how much."
            ),
        ]
    return "\n".join(lines) + "\n"


def bench_files(argv):
    """The two bench outputs named after --bench, or nothing."""
    if "--bench" not in argv:
        return None
    at = argv.index("--bench")
    return argv[at + 1], argv[at + 2]


def read_or_empty(path):
    try:
        with open(path, encoding="utf-8") as file:
            return file.read()
    except OSError:
        return ""


def main():
    # criterion writes utf-8, a minus sign and a micro sign among it, whatever
    # the console's own encoding is. Read and write the same way, so a windows
    # clone parses the sign rather than a mangled one and prints the table
    sys.stdin.reconfigure(encoding="utf-8")
    sys.stdout.reconfigure(encoding="utf-8")
    bench = bench_files(sys.argv[1:])
    results = list(parse(sys.stdin))
    if not results:
        print("No benchmark comparison was produced.")
        if bench:
            print()
            print(bench_table(read_or_empty(bench[0]), read_or_empty(bench[1])))
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
    if bench:
        print()
        print(bench_table(read_or_empty(bench[0]), read_or_empty(bench[1])))
    return 0


if __name__ == "__main__":
    sys.exit(main())
