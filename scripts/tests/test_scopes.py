# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2022-2026 Andrew Wright

"""The commit scope list is kept in four places, and they have to agree.

The commit-msg hook holds the scopes it accepts. cliff.toml names the build
scopes, which go under Development whatever their type. The trailer check
names the engine scopes, which need a bench. And DEVELOPMENT.md has a table
of each kind. A scope added to the hook and not the others lands a commit in
the wrong changelog section, or lets an engine commit through with no bench
stated, and nothing else would notice. The hook is the gate, so that is the
direction checked: a scope the others name and the hook does not is one no
commit can carry.
"""

import re
from pathlib import Path

import check_trailers
import tomllib
import yaml

ROOT = Path(__file__).resolve().parent.parent.parent

# listed by type like an engine scope, but needing no bench: the protocol
# layer never changes what the search counts
NEITHER = {"uci"}


def hook_scopes() -> set[str]:
    config = yaml.safe_load((ROOT / ".pre-commit-config.yaml").read_text("utf-8"))
    for repo in config["repos"]:
        for hook in repo["hooks"]:
            if hook["id"] == "conventional-pre-commit":
                args = hook["args"]
                return set(args[args.index("--scopes") + 1].split(","))
    raise AssertionError("the conventional-pre-commit hook is not configured")


def cliff_build_pattern() -> re.Pattern:
    """The alternation cliff.toml matches build scopes with, as a pattern a
    scope either matches whole or does not."""
    config = tomllib.loads((ROOT / "cliff.toml").read_text("utf-8"))
    for parser in config["git"]["commit_parsers"]:
        if match := re.fullmatch(r"\^\\w\+\\\((.+)\\\)!\?:", parser.get("message", "")):
            return re.compile(match.group(1))
    raise AssertionError("cliff.toml has no parser for the build scopes")


def documented_scopes() -> tuple[set[str], set[str]]:
    """The build table and the engine table of DEVELOPMENT.md, as the
    scopes named in their first columns."""
    text = (ROOT / "docs" / "DEVELOPMENT.md").read_text("utf-8")
    section = text.split("## Commit messages")[1].split("## Commit trailers")[0]
    tables = []
    for table in re.findall(
        r"\| scope \| covers \|\n\| --- \| --- \|\n((?:\|.*\|\n)+)", section
    ):
        first_cells = [row.split("|")[1] for row in table.strip().splitlines()]
        tables.append(
            {name for cell in first_cells for name in re.findall(r"`([\w-]+)`", cell)}
        )
    assert len(tables) == 2, "expected a build table and an engine table"
    return tables[0], tables[1]


def test_every_scope_the_hook_accepts_is_a_build_scope_or_an_engine_scope():
    hook = hook_scopes()
    build = {scope for scope in hook if cliff_build_pattern().fullmatch(scope)}
    engine = check_trailers.ENGINE_SCOPES
    assert engine <= hook, "an engine scope the hook would refuse"
    assert not build & engine, "a scope is both build and engine"
    unplaced = hook - build - engine - NEITHER
    assert not unplaced, (
        f"the hook accepts {sorted(unplaced)}, which cliff.toml and the trailer check do not place"
    )


def test_the_development_notes_list_the_same_scopes():
    hook = hook_scopes()
    build = {scope for scope in hook if cliff_build_pattern().fullmatch(scope)}
    documented_build, documented_engine = documented_scopes()
    assert documented_build == build
    assert documented_engine == check_trailers.ENGINE_SCOPES | NEITHER
