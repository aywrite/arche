# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2022-2026 Andrew Wright

"""Every source file carries the licence notice, so a new one cannot ship
without it.

The repository is GPL-3.0-or-later and says so in LICENSE, but a scanner
reads files, not repositories, and the GPL's own instructions ask each file
to say whose it is and under what terms. The header is two lines and this
test is what keeps them there.
"""

from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent

SPDX = "SPDX-License-Identifier: GPL-3.0-or-later"


def source_files():
    for pattern in ("**/*.rs", "scripts/**/*.py", "scripts/**/*.sh", "docker/*.sh"):
        for path in ROOT.glob(pattern):
            parts = path.relative_to(ROOT).parts
            if parts[0] in (".git", "target") or "target" in parts:
                continue
            yield path


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
    # a glob that silently matched nothing would pass the test above forever
    found = list(source_files())
    assert len(found) > 40, (
        f"only {len(found)} source files found, the glob has drifted"
    )
