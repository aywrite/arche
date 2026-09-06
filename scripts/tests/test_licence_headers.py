# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2022-2026 Andrew Wright

"""Every source file carries the licence notice, so a new one cannot ship
without it.

The repository is GPL-3.0-or-later and says so in LICENSE, but a scanner
reads files, not repositories, and the GPL's own instructions ask each file
to say whose it is and under what terms. The header is two lines and this
test is what keeps them there.
"""

import os
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent

SPDX = "SPDX-License-Identifier: GPL-3.0-or-later"

# What counts as a source file, by extension rather than by where it sits.
# This was a list of path patterns, and one of them was docker/*.sh, so
# docker/build_book.py was never looked at and the gap was invisible: the
# test passed because it did not go there. A suffix cannot grow that hole,
# and a new directory of sources is covered the day it appears.
SUFFIXES = {".rs", ".py", ".sh"}

# pruned rather than filtered afterwards, because target/ holds hundreds of
# thousands of files and walking it to throw it away takes real seconds
PRUNE = {".git", "target", "__pycache__", ".venv", "node_modules"}


def source_files():
    for directory, subdirectories, names in os.walk(ROOT):
        subdirectories[:] = [d for d in subdirectories if d not in PRUNE]
        for name in names:
            if Path(name).suffix in SUFFIXES:
                yield Path(directory) / name


def test_every_source_file_states_its_licence():
    missing = []
    for path in source_files():
        # the notice sits in the first few lines: after a shebang if there is
        # one, and nowhere lower, or a scanner reading heads will not see it
        head = "".join(path.read_text(encoding="utf-8").splitlines(keepends=True)[:3])
        if SPDX not in head:
            missing.append(str(path.relative_to(ROOT)))
    assert not missing, f"no licence header in: {', '.join(sorted(missing))}"


def test_the_walk_actually_finds_the_sources():
    # a walk that silently matched nothing would pass the test above forever.
    # It finds sixty five today, exactly what `git ls-files` lists for these
    # three suffixes; the floor is well under that, because what is being
    # caught is a walk that has stopped working rather than one that drifted
    # by a file or two.
    found = list(source_files())
    assert len(found) > 50, (
        f"only {len(found)} source files found, the walk has drifted"
    )


def test_the_walk_reaches_every_directory_that_holds_sources():
    # the named cases are one per directory the sources live in, and
    # docker/build_book.py is the one the old pattern list missed. A walk
    # that stops covering a directory fails here rather than going quiet.
    found = {Path(p).resolve().relative_to(ROOT).as_posix() for p in source_files()}
    for path in (
        "src/main.rs",
        "arche-core/src/board.rs",
        "tests/uci_session.rs",
        "scripts/speed.py",
        "scripts/tests/conftest.py",
        "docker/build_book.py",
        "docker/smoke_test.sh",
    ):
        assert path in found, f"{path} is not covered by the licence walk"
