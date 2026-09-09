# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2022-2026 Andrew Wright

"""Tests for the defaults of the inputs Strength and Calibrate are called with.

Some of their inputs are workflow_call inputs and not boxes on the actions
tab. A workflow_call default is not applied to a run started from the tab, so
every one of those reaches a dispatch run empty unless the expression that
reads it repeats the default itself. Nothing in the workflow says so, and a
run that lost one plays on a table of nothing or publishes under no marker
rather than failing, so what is checked here is that each of them is read as
inputs.<name> || <default>.
"""

import re
from pathlib import Path

import yaml

ROOT = Path(__file__).resolve().parent.parent.parent
WORKFLOWS = [
    ROOT / ".github" / "workflows" / name for name in ("strength.yml", "calibrate.yml")
]

# inputs.<name> anywhere in an expression
READ = re.compile(r"inputs\.([A-Za-z0-9_-]+)")


def workflows():
    for path in WORKFLOWS:
        yield path.name, yaml.safe_load(path.read_text(encoding="utf-8"))


def call_only(workflow) -> dict[str, object]:
    """The workflow_call inputs with a default that the tab does not offer."""
    # yaml reads a bare on: as a boolean, so the triggers are under True
    triggers = workflow[True]
    dispatch = (triggers.get("workflow_dispatch") or {}).get("inputs") or {}
    call = (triggers.get("workflow_call") or {}).get("inputs") or {}
    return {
        name: spec["default"]
        for name, spec in call.items()
        if name not in dispatch and "default" in spec
    }


def values(workflow):
    """Where a value is read into env or handed to another workflow."""
    for key, value in (workflow.get("env") or {}).items():
        yield f"env.{key}", value
    for job_name, job in workflow["jobs"].items():
        for key, value in (job.get("env") or {}).items():
            yield f"jobs.{job_name}.env.{key}", value
        for key, value in (job.get("with") or {}).items():
            yield f"jobs.{job_name}.with.{key}", value


def written(default) -> str:
    """The default as an expression writes it."""
    if isinstance(default, bool):
        return str(default).lower()
    if isinstance(default, str):
        return f"'{default}'"
    return str(default)


def test_a_call_only_default_is_repeated_where_it_is_read():
    for name, workflow in workflows():
        defaults = call_only(workflow)
        checked = 0
        for where, value in values(workflow):
            for read in sorted(set(READ.findall(str(value)))):
                if read not in defaults:
                    continue
                checked += 1
                repeated = f"inputs.{read} || {written(defaults[read])}"
                assert repeated in str(value), (
                    f"{name} {where} reads {read} without its default: "
                    f"expected {repeated}"
                )
        # a rename that left the scan finding nothing would pass silently
        assert checked, f"{name} has no call-only default read anywhere"
