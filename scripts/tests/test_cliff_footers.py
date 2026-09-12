# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2022-2026 Andrew Wright

"""The changelog prints a trailer's whole value or none of it.

A trailer is written to be read as one thing. `Speed: +6.4% (bench nps, 9
interleaved rounds vs abc1234, spread 50.5%)` says the change and what it has
to be read against, and the project's own rule is that a change inside its
spread is a measurement making no claim. The template printed the first word
and dropped the rest, so the changelog turned eighteen of twenty two
non-claims into claims.

git-cliff is a rust binary and is not installed to run the script tests, so
what is checked here is the template rather than its output: every footer
branch prints `footer.value` whole, with no filter cutting it down.
"""

import re
from pathlib import Path

import tomllib

ROOT = Path(__file__).resolve().parent.parent.parent

# the three trailers docs/DEVELOPMENT.md describes, which are the three the
# template has a branch for
FOOTERS = ("Bench", "Speed", "Elo")


def footer_branches() -> dict[str, str]:
    """Each footer token's branch of the changelog body template, as the text
    between the test of the token and the end of that branch."""
    body = tomllib.loads((ROOT / "cliff.toml").read_text("utf-8"))["changelog"]["body"]
    found = {}
    for token in FOOTERS:
        match = re.search(
            rf'footer\.token == "{token}" %}}(.*?)(?=\{{%|$)', body, re.DOTALL
        )
        assert match, f"cliff.toml has no branch for the {token} footer"
        found[token] = match.group(1)
    return found


def test_every_footer_prints_its_whole_value():
    for token, branch in footer_branches().items():
        assert "{{ footer.value }}" in branch, (
            f"the {token} footer is not printed whole: {branch!r}"
        )


def test_no_footer_is_cut_down_by_a_filter():
    # split/first/truncate are the shapes this went wrong in: the value was
    # printed through a filter that kept the first word
    for token, branch in footer_branches().items():
        for filtered in ("split(", "first", "truncate"):
            assert filtered not in branch, (
                f"the {token} footer is printed through {filtered}, "
                "which drops part of what the trailer says"
            )
