# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2022-2026 Andrew Wright

"""Every source file carries the licence notice. A scanner reads files, not
repositories, and the GPL's own instructions ask each file to say whose it is
and under what terms."""

import os
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent

SPDX = "SPDX-License-Identifier: GPL-3.0-or-later"

# By extension rather than by path: a list of path patterns once missed
# docker/build_book.py, and a suffix cannot grow that hole.
SUFFIXES = {".rs", ".py", ".sh"}

# pruned rather than filtered afterwards, because walking target/ takes real
# seconds
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
        # after a shebang if there is one, and nowhere lower, or a scanner
        # reading heads will not see it
        head = "".join(path.read_text(encoding="utf-8").splitlines(keepends=True)[:3])
        if SPDX not in head:
            missing.append(str(path.relative_to(ROOT)))
    assert not missing, f"no licence header in: {', '.join(sorted(missing))}"


def test_the_walk_actually_finds_the_sources():
    # a walk that silently matched nothing would pass the test above forever.
    # The floor is well under what `git ls-files` lists for these suffixes,
    # since what is caught is a walk that has stopped working
    found = list(source_files())
    assert len(found) > 50, (
        f"only {len(found)} source files found, the walk has drifted"
    )


def test_the_walk_reaches_every_directory_that_holds_sources():
    # one per directory the sources live in; docker/build_book.py is the one
    # the old pattern list missed
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
