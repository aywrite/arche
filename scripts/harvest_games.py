#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2022-2026 Andrew Wright

"""Download the strength runs' games into an archive, and rebuild the corpus.

`build_corpus.py` turns a pile of pgns into the epd `arche terms` reads. Getting
that pile was the step with no tooling: the artifacts sit on the runs and came
down by hand, so the corpus grew when somebody remembered rather than when the
games were played.

    python3 scripts/harvest_games.py --archive runs --out corpus.epd

What it does is fetch every strength artifact that is not in the archive
already, then rebuild the corpus from the whole archive. Running it twice in a
row downloads nothing the second time, which is the point: it is meant to run
after every arm, and an arm's games are gone for good if nobody runs it before
the artifact expires.

## Only the strength runs

A strength game is arche against arche. A calibrate game is arche against
another engine, and `build_corpus.py` says what that would cost: the corpus
carries the caveat that these are the engine's own games, so what the engine
never reaches is unlabelled and its mistakes are labelled as if they were normal
play. That caveat is only stateable while the corpus has one source, and a loss
change measured over a corpus of two cannot be attributed to either. So the rule
is not that a calibrate game is worse, it is that it is a different question.

The prefix below is what enforces it, and it also excludes the `gauntlet-<run>`
artifacts still in the listing. Those were the rungs' games concatenated, and
calibrate stopped uploading them in `dcf87b7` when the rungs began playing at
the same time, so what remains of them is residue that will expire on its own.

## The rebuild is whole, not incremental

A position two games reached belongs to the group of the lower of their keys and
is labelled by that group's games alone, which is what keeps a sealed game's
result out of a row the fit reads. That assignment depends on every game in the
archive, so adding games changes labels on positions that were already there.
Appending to an existing epd would leave those stale. The whole corpus is
therefore rebuilt from the whole archive every time, and the archive rather than
the epd is the thing that must not be lost.
"""

import argparse
import json
import subprocess
import sys
from pathlib import Path

import build_corpus

# What the strength workflow names its artifact, which is the only prefix this
# harvests. See the module docstring for why calibrate is not.
PREFIX = "strength-"

# The file the archive writes once an artifact is down and intact. A directory
# on its own is not the marker: an interrupted download leaves one behind, and a
# run that silently skipped it would be taken for harvested ever after.
MARKER = ".harvested"

REPO = "aywrite/arche"


def gh(*arguments):
    """One `gh` call, returning stdout. Replaced wholesale by the tests, which
    is why every call this script makes goes through here."""
    finished = subprocess.run(
        ["gh", *arguments], check=True, capture_output=True, text=True
    )
    return finished.stdout


def artifacts(repo):
    """Every artifact the repository holds, as the fields this reads.

    `--jq` over the paginated listing gives one json object a line, so a page
    boundary is not something this has to know about.
    """
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

    The upload names four paths under `tools/`, so the artifact's own root is
    that directory and what arrives here is `games.pgn` beside its manifest.
    """
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
            # unrecoverable, and the only outcome here worth a reader's
            # attention: the games it held were never archived, and the run
            # cannot be played again to produce them
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
    """Artifacts the archive holds that carry no games.pgn.

    A shard that died before it played uploads its manifest and no games, and
    `if-no-files-found: warn` keeps the run green, so this arrives looking
    exactly like a shard that played. Naming them is the difference between an
    archive that is short and an archive that is short and says so.
    """
    return sorted(
        marker.parent.name
        for marker in archive.glob(f"*/{MARKER}")
        if not (marker.parent / "games.pgn").exists()
    )


def soonest(archive, listing):
    """When the first strength artifact the archive does not hold expires.

    Held artifacts are excluded because their games are already safe. Without
    that the figure is the oldest live artifact's expiry whatever the archive
    holds, so it reads as a deadline on a run where nothing is at risk.
    """
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
        # gh says what is wrong (a lapsed login, a rate limit) and check=True
        # puts that inside the exception, where nothing would print it
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
        # a failed download still fails the run. Returning the builder's status
        # alone would report success over an arm's games left on a run that
        # expires, which is the loss this script exists to prevent
        built = build_corpus.main([*files, "--out", args.out])
        return built or (1 if failed else 0)
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
