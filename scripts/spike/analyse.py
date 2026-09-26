#!/usr/bin/env python3
"""Split run-to-run from between-layout variance in the spike's raw csv.

Every metric is taken as 100*ln(value), so the sds read as percent. The model
is additive in variant and pass (one run per cell), which takes machine drift
across passes out of the run sd; the one-way split is printed too. Throwaway
spike code.
"""

import csv
import math
import sys
from collections import defaultdict


def split(rows, metric):
    cells = {}
    for r in rows:
        v = r[metric]
        if v in ("", None):
            return None
        try:
            v = float(v)
        except ValueError:
            return None
        if v <= 0:
            return None
        cells[(r["name"], int(r["pass"]))] = 100 * math.log(v)
    names = sorted({n for n, _ in cells})
    passes = sorted({p for _, p in cells})
    passes = [p for p in passes if all((n, p) in cells for n in names)]
    V, P = len(names), len(passes)
    if V < 2 or P < 2:
        return None
    y = [[cells[(n, p)] for p in passes] for n in names]
    grand = sum(map(sum, y)) / (V * P)
    vm = [sum(row) / P for row in y]
    pm = [sum(y[i][j] for i in range(V)) / V for j in range(P)]
    ss_v = P * sum((m - grand) ** 2 for m in vm)
    ss_p = V * sum((m - grand) ** 2 for m in pm)
    ss_t = sum((y[i][j] - grand) ** 2 for i in range(V) for j in range(P))
    ss_r = ss_t - ss_v - ss_p
    ms_v = ss_v / (V - 1)
    ms_r = ss_r / ((V - 1) * (P - 1))
    ms_w1 = (ss_t - ss_v) / (V * (P - 1))  # one-way within
    sb2 = max(0.0, (ms_v - ms_r) / P)
    sb2_1 = max(0.0, (ms_v - ms_w1) / P)
    return {
        "V": V, "P": P, "sw": math.sqrt(ms_r), "sb": math.sqrt(sb2),
        "F": ms_v / ms_r if ms_r > 0 else float("inf"),
        "sw1": math.sqrt(ms_w1), "sb1": math.sqrt(sb2_1),
        "pass_sd": math.sqrt(max(0.0, (ss_p / (P - 1) - ms_r) / V)),
        "means": dict(zip(names, vm)),
    }


def half_width(sb, sw, k, r):
    # difference of two K-layout means, r runs per layout per side
    return 1.96 * math.sqrt(2 * (sb * sb / k + sw * sw / (k * r)))


def main():
    rows = list(csv.DictReader(open(sys.argv[1])))
    nodes = {r["nodes"] for r in rows}
    print(f"runs: {len(rows)}, distinct node counts: {sorted(nodes)}")
    for kind in ("ctrl", "shuf", "pad"):
        kr = [r for r in rows if r["kind"] == kind]
        if not kr:
            continue
        print(f"\n=== {kind}")
        for metric in ("nps", "wall", "cycles", "instructions", "branch_misses"):
            s = split(kr, metric)
            if s is None:
                print(f"  {metric:14s} n/a")
                continue
            print(f"  {metric:14s} V={s['V']} P={s['P']}  run sd {s['sw']:.3f}%  "
                  f"layout sd {s['sb']:.3f}%  F={s['F']:.2f}  pass sd {s['pass_sd']:.3f}%"
                  f"  (one-way: run {s['sw1']:.3f}% layout {s['sb1']:.3f}%)")
            if metric in ("nps", "cycles") and kind != "ctrl":
                for k in (8, 16, 32):
                    print(f"      K={k:2d}: 95% half-width r=1 {half_width(s['sb'], s['sw'], k, 1):.3f}%"
                          f"  r={s['P']} {half_width(s['sb'], s['sw'], k, s['P']):.3f}%")
        s = split(kr, "nps")
        if s:
            ms = sorted(s["means"].items(), key=lambda t: t[1])
            print("  per-variant mean 100*ln(nps), low to high:")
            print("   " + "  ".join(f"{n}:{m - ms[0][1]:+.2f}" for n, m in ms))


if __name__ == "__main__":
    main()
