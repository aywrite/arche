#!/usr/bin/env python3
"""Run `arche bench` over every variant in passes, each pass in a random order.

Writes one csv row per run. Uses `perf stat` around each run when --perf is
given. Throwaway spike code.
"""

import argparse
import csv
import os
import random
import subprocess
import sys
import tempfile
import time

EVENTS = "cycles:u,instructions:u,branch-misses:u"


def run_one(binary, perf):
    counters = {}
    with tempfile.NamedTemporaryFile(suffix=".perf", delete=False) as fh:
        perf_out = fh.name
    cmd = [binary, "bench"]
    if perf:
        cmd = ["perf", "stat", "-x,", "-e", EVENTS, "-o", perf_out, "--"] + cmd
    t0 = time.perf_counter()
    res = subprocess.run(cmd, capture_output=True, text=True)
    wall = time.perf_counter() - t0
    if res.returncode != 0:
        raise RuntimeError(f"{binary} failed: {res.stderr[-500:]}")
    last = [ln for ln in res.stdout.splitlines() if ln.strip()][-1].split()
    nodes, nps = int(last[0]), int(last[2])
    if perf:
        with open(perf_out) as fh:
            for line in fh:
                parts = line.strip().split(",")
                if len(parts) >= 3 and not line.startswith("#"):
                    name = parts[2]
                    try:
                        counters[name] = int(float(parts[0]))
                    except ValueError:
                        counters[name] = parts[0]
    os.unlink(perf_out)
    return nodes, nps, wall, counters


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("variants")
    ap.add_argument("out")
    ap.add_argument("--passes", type=int, default=8)
    ap.add_argument("--budget", type=float, default=1500, help="seconds")
    ap.add_argument("--perf", action="store_true")
    ap.add_argument("--seed", type=int, default=1)
    args = ap.parse_args()

    names = []
    with open(os.path.join(args.variants, "manifest.tsv")) as fh:
        for row in csv.DictReader(fh, delimiter="\t"):
            names.append((row["name"], row["kind"]))
    rng = random.Random(args.seed)
    fields = ["pass", "pos", "name", "kind", "nodes", "nps", "wall",
              "cycles", "instructions", "branch_misses"]
    start = time.time()
    pass_time = None
    with open(args.out, "w", newline="") as fh:
        w = csv.DictWriter(fh, fieldnames=fields)
        w.writeheader()
        for p in range(args.passes):
            elapsed = time.time() - start
            if pass_time and elapsed + pass_time * 1.05 > args.budget:
                print(f"stopping after {p} passes: budget", flush=True)
                break
            t0 = time.time()
            order = names[:]
            rng.shuffle(order)
            for pos, (name, kind) in enumerate(order):
                nodes, nps, wall, c = run_one(
                    os.path.join(args.variants, name), args.perf)
                w.writerow({
                    "pass": p, "pos": pos, "name": name, "kind": kind,
                    "nodes": nodes, "nps": nps, "wall": f"{wall:.4f}",
                    "cycles": c.get("cycles:u", ""),
                    "instructions": c.get("instructions:u", ""),
                    "branch_misses": c.get("branch-misses:u", ""),
                })
            fh.flush()
            pass_time = time.time() - t0
            print(f"pass {p} done in {pass_time:.0f} s", flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
