#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2022-2026 Andrew Wright

"""Download the strength runs' games into an archive, and rebuild the corpus.

    python3 scripts/harvest_games.py --archive runs --out corpus.epd

Fetches every strength artifact not already in the archive, then rebuilds the
corpus from the whole archive with `build_corpus.py`. A second run downloads
nothing, so it can run after every arm, and an arm's games are gone for good
if nobody runs it before the artifact expires.

Only the strength runs. A calibrate game is arche against another engine,
which is a second source, and a loss change over a corpus of two sources
cannot be attributed to either. The prefix also excludes the `gauntlet-<run>`
artifacts still in the listing, the rungs' games concatenated, which
calibrate stopped uploading in `dcf87b7`.

The rebuild is whole, not incremental: a position two games reached belongs
to the group of the lower key, so adding games changes labels on positions
already there. The archive rather than the epd is what must not be lost.
"""

import argparse
import json
import subprocess
import sys
from pathlib import Path

import build_corpus

# What the strength workflow names its artifacts.
PREFIX = "strength-"

# Written once an artifact is down and intact. A directory on its own is not
# the marker, since an interrupted download leaves one behind.
MARKER = ".harvested"

REPO = "aywrite/arche"


def gh(*arguments):
    """One `gh` call, returning stdout. The tests replace this, so every call
    goes through here."""
    finished = subprocess.run(
        ["gh", *arguments], check=True, capture_output=True, text=True
    )
    return finished.stdout


def artifacts(repo):
    """Every artifact the repository holds, as the fields this reads. `--jq`
    over the paginated listing gives one json object a line."""
    out = gh(
        "api",
        "--paginate",
        f"repos/{repo}/actions/artifacts?per_page=100",
        "--jq",
        ".artifacts[] | {name: .name, expired: .expired, "
        "run: .workflow_run.id, expires: .expires_at}",
    )
    return [json.loads(line) for line in out.splitlines() if line.strip()]


def fetch(repo, archive, artifact):
    """One artifact into `<archive>/<name>/`, with its marker written last.
    The upload names its paths under `tools/`, so what arrives is `games.pgn`
    beside its manifest."""
    into = archive / artifact["name"]
    into.mkdir(parents=True, exist_ok=True)
    gh(
        "run",
        "download",
        str(artifact["run"]),
        "--repo",
        repo,
        "-n",
        artifact["name"],
        "--dir",
        str(into),
    )
    (into / MARKER).write_text("", encoding="utf-8")
    return into


def held(archive, name):
    return (archive / name / MARKER).exists()


def harvest(repo, archive, listing):
    """Fetch what is missing. Returns what was taken, skipped and lost."""
    archive.mkdir(parents=True, exist_ok=True)
    wanted = [item for item in listing if item["name"].startswith(PREFIX)]
    taken, skipped, expired, failed = [], [], [], []
    for artifact in sorted(wanted, key=lambda item: item["name"]):
        if held(archive, artifact["name"]):
            skipped.append(artifact["name"])
            continue
        if artifact["expired"]:
            # its games were never archived and cannot be played again
            expired.append(artifact["name"])
            continue
        try:
            fetch(repo, archive, artifact)
        except subprocess.CalledProcessError as error:
            failed.append((artifact["name"], (error.stderr or "").strip()))
            continue
        taken.append(artifact["name"])
    return taken, skipped, expired, failed


def pgns(archive):
    """Every harvested games.pgn, in a fixed order so two runs of the corpus
    builder over one archive agree."""
    found = []
    for marker in archive.glob(f"*/{MARKER}"):
        games = marker.parent / "games.pgn"
        if games.exists():
            found.append(str(games))
    return sorted(found)


def gameless(archive):
    """Artifacts the archive holds that carry no games.pgn: a shard that died
    before it played uploads its manifest and no games, and `if-no-files-found:
    warn` keeps the run green."""
    return sorted(
        marker.parent.name
        for marker in archive.glob(f"*/{MARKER}")
        if not (marker.parent / "games.pgn").exists()
    )


def soonest(archive, listing):
    """When the first strength artifact the archive does not hold expires.
    Held artifacts are excluded, or the figure would read as a deadline on a
    run where nothing is at risk."""
    waiting = [
        item["expires"]
        for item in listing
        if item["name"].startswith(PREFIX)
        and not item["expired"]
        and not held(archive, item["name"])
    ]
    return min(waiting, default=None)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--archive", required=True, help="directory the artifacts accumulate in"
    )
    parser.add_argument(
        "--out", help="rebuild the corpus epd here once the archive is current"
    )
    parser.add_argument(
        "--sealed",
        help="a file naming the sealed pairs, passed to the rebuild; without "
        "it the sealed group is drawn from the keys",
    )
    parser.add_argument(
        "--repo", default=REPO, help="owner/name to harvest (default %(default)s)"
    )
    args = parser.parse_args(argv)

    archive = Path(args.archive)
    try:
        listing = artifacts(args.repo)
    except FileNotFoundError:
        print(
            "harvest_games.py: gh is not on PATH, and the listing comes from it. "
            "Install the github cli, or run this where it is installed.",
            file=sys.stderr,
        )
        return 1
    except subprocess.CalledProcessError as error:
        # gh's own diagnosis (a lapsed login, a rate limit) is in the exception
        print("harvest_games.py: gh could not list the artifacts.", file=sys.stderr)
        print((error.stderr or "").strip(), file=sys.stderr)
        return 1

    taken, skipped, expired, failed = harvest(args.repo, archive, listing)

    print(
        f"harvest taken {len(taken)} already_held {len(skipped)} "
        f"expired {len(expired)} failed {len(failed)}"
    )
    for name in expired:
        print(f"  expired, its games are not recoverable: {name}", file=sys.stderr)
    for name, why in failed:
        print(f"  download failed: {name}: {why}", file=sys.stderr)
    for name in gameless(archive):
        print(f"  held but carries no games.pgn: {name}", file=sys.stderr)
    deadline = soonest(archive, listing)
    if deadline:
        print(f"  the next unharvested artifact expires at {deadline}")

    files = pgns(archive)
    if not files:
        print("harvest_games.py: the archive holds no games.pgn", file=sys.stderr)
        return 1
    print(f"archive holds {len(files)} pgn files")
    if args.out:
        # a failed download still fails the run, whatever the builder returns
        sealed = ["--sealed", args.sealed] if args.sealed else []
        built = build_corpus.main([*files, "--out", args.out, *sealed])
        return built or (1 if failed else 0)
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
