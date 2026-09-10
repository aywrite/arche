# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2022-2026 Andrew Wright

"""The tools a match is read with: the pooled estimate, the ccrl fit, the
terminations count and the book slice.

They are one package because they read one thing. The fit owns the two regular
expressions a fastchess pgn is split into games with, and the estimate reads
the terminations count as well as the fit.
"""

__version__ = "0.1.0"

# The version of the --json shape, and the one thing every --json object says
# about itself. It is 0 because the shape is provisional: nothing outside this
# repository reads it yet, the workflows read --line and --trailer as they
# always have, and a shape frozen before a consumer has read it is the wrong
# shape frozen. It becomes 1, and additions only from then on, once something
# outside reads it.
JSON_FORMAT = 0


def tool(command: str) -> dict[str, str]:
    """What produced a --json object, so a figure can be read back against the
    version of the tooling that produced it."""
    return {"name": "match-tools", "version": __version__, "command": command}
