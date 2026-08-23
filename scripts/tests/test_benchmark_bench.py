"""Tests for the bench rows the benchmark summary adds: the engine's own
bench on each side of a pull request, measured on the one runner."""

import benchmark_summary

BASE = "bench depth 7 hash 16MB positions 18\n...\n42847751 nodes 12473872 nps\n"
SAME = "bench depth 7 hash 16MB positions 18\n...\n42847751 nodes 12600000 nps\n"
MOVED = "bench depth 7 hash 16MB positions 18\n...\n4571056 nodes 12000000 nps\n"


def test_the_rows_carry_both_sides_and_the_change_in_rate():
    text = benchmark_summary.bench_table(BASE, SAME)
    assert "| nodes | 42847751 | 42847751 | same |" in text
    assert "| nps | 12473872 | 12600000 | +1.0% |" in text


def test_a_moved_node_count_is_named_rather_than_compared_as_speed():
    text = benchmark_summary.bench_table(BASE, MOVED)
    assert "| nodes | 42847751 | 4571056 | moved |" in text
    assert "search change" in text


def test_a_slower_rate_carries_its_sign():
    slower = "x\n42847751 nodes 12000000 nps\n"
    text = benchmark_summary.bench_table(BASE, slower)
    assert "| nps | 12473872 | 12000000 | -3.8% |" in text


def test_a_side_that_did_not_run_says_so():
    text = benchmark_summary.bench_table(BASE, "")
    assert "not measured" in text
