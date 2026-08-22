"""Tests for the criterion comparison table.

The parser reads criterion's human output, which criterion does not promise to
keep stable. These fixtures are copies of the real shape, so a criterion
upgrade that changes it fails here rather than publishing an empty table.
"""

import subprocess
import sys
from pathlib import Path

import benchmark_summary

SCRIPT = Path(benchmark_summary.__file__)

# criterion prints the id on its own line, then the measurements indented,
# with the verdict last; the minus sign is U+2212, not a hyphen
COMPARISON = """\
Benchmarking generate_moves/start
Benchmarking generate_moves/start: Collecting 100 samples in estimated 5s
generate_moves/start
                        time:   [88.123 ns 89.136 ns 90.312 ns]
                        change: [+0.5000% +1.8959% +3.2000%] (p = 0.03 < 0.05)
                        Performance has regressed.
perft_3/start
                        time:   [400.00 µs 410.00 µs 420.00 µs]
                        change: [−2.0000% −1.0000% +0.1000%] (p = 0.20 > 0.05)
                        No change in performance detected.
brand_new_bench/start
                        time:   [1.0000 ms 1.1000 ms 1.2000 ms]
"""


def test_each_benchmark_yields_time_change_and_verdict():
    records = list(benchmark_summary.parse(COMPARISON.splitlines()))
    assert records == [
        (
            "generate_moves/start",
            "89.136 ns",
            "+1.8959%",
            "Performance has regressed",
        ),
        ("perft_3/start", "410.00 µs", "-1.0000%", "No change in performance detected"),
        ("brand_new_bench/start", "1.1000 ms", None, "new"),
    ]


def test_criterions_minus_sign_becomes_a_hyphen():
    records = list(benchmark_summary.parse(COMPARISON.splitlines()))
    assert records[1][2] == "-1.0000%"
    assert "−" not in records[1][2]


def run(text):
    return subprocess.run(
        [sys.executable, str(SCRIPT)],
        input=text,
        check=False,
        capture_output=True,
        # the fixture carries criterion's minus sign, which the console
        # encoding of a windows clone cannot hold
        encoding="utf-8",
    )


def test_the_table_counts_regressions_and_sorts_worst_first():
    result = run(COMPARISON)
    assert result.returncode == 0
    assert "1 of 3 benchmarks are slower." in result.stdout
    rows = [line for line in result.stdout.splitlines() if line.startswith("| `")]
    assert rows == [
        "| `generate_moves/start` | 89.136 ns | +1.8959% | slower |",
        "| `perft_3/start` | 410.00 µs | -1.0000% | no change |",
        "| `brand_new_bench/start` | 1.1000 ms | no baseline | new |",
    ]


def test_nothing_slower_is_said_outright():
    result = run(
        COMPARISON.replace("Performance has regressed", "Performance has improved")
    )
    assert "None of the 3 benchmarks are slower." in result.stdout


def test_empty_input_fails_rather_than_prints_an_empty_table():
    result = run("")
    assert result.returncode == 1
    assert "No benchmark comparison was produced." in result.stdout
