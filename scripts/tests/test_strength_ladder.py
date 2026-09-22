# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2022-2026 Andrew Wright

"""Tests for the batch ladder Strength plays an sprt over.

Actions has no loop and `uses:` is not an expression, so the stages are
written out rather than generated. What a generated ladder would get for free
is what is checked here: that each stage waits on the one before, that they
count from zero without a gap, that they share a seed, and that the ceiling
the run is refused above is the number of stages there actually are.

A stage that lost its guard would play a batch a decided test did not need. A
stage that lost its index would replay another's openings, and the pairs would
be pooled as though they were new games.
"""

import re
from pathlib import Path

import yaml

ROOT = Path(__file__).resolve().parent.parent.parent
WORKFLOWS = ROOT / ".github" / "workflows"
STRENGTH = WORKFLOWS / "strength.yml"
BATCH = WORKFLOWS / "strength-batch.yml"

# the relative form, so the stages are this file's own batch definition
CALLS = "./.github/workflows/strength-batch.yml"

# `[ "$BATCHES" -ge 1 ] && [ "$BATCHES" -le 4 ]` in the resolve job
CEILING = re.compile(r'"\$BATCHES"\s+-le\s+([0-9]+)')


def workflow(path):
    return yaml.safe_load(path.read_text(encoding="utf-8"))


def stages(jobs):
    """The ladder's jobs, in the order the file writes them."""
    return {name: job for name, job in jobs.items() if job.get("uses") == CALLS}


def test_the_stages_run_one_after_another_and_stop_when_the_test_decides():
    ladder = stages(workflow(STRENGTH)["jobs"])
    names = list(ladder)
    assert len(names) >= 2, "a ladder of one stage is the workflow without one"
    # the first plays whenever there is something to play against
    assert ladder[names[0]]["if"] == "needs.resolve.outputs.baseline_sha != ''"
    for place, name in enumerate(names[1:], start=1):
        before = names[place - 1]
        guard = ladder[name]["if"]
        # the early stop: a batch that settled it is not followed
        assert f"needs.{before}.outputs.verdict == 'inconclusive'" in guard, (
            f"{name} does not wait on what {before} came to"
        )
        # and the cap: without this a test still running at its last batch
        # would start one more, which the book reserved no room for
        assert f"inputs.batches > {place}" in guard, (
            f"{name} plays whatever batches was asked for"
        )
        assert before in ladder[name]["needs"]
        # the pairs of every batch so far, which is what makes the test
        # sequential rather than four tests
        assert ladder[name]["with"]["prior_pairs"] == (
            "${{ needs." + before + ".outputs.carried }}"
        )


def test_a_shard_that_fell_over_does_not_stop_the_ladder():
    """play is fail-fast: false and summarise runs anyway, but a failed shard
    still concludes the batch that held it as a failure. A stage waiting on
    that batch would be skipped before its verdict was read, and a test with
    batches to spare would stop after one. Every guard after the first says
    so, which is what lets the verdict rather than the result decide."""
    ladder = stages(workflow(STRENGTH)["jobs"])
    for name in list(ladder)[1:]:
        assert "!cancelled()" in ladder[name]["if"], (
            f"{name} waits on the batch before it succeeding, not on what it came to"
        )


def test_the_line_is_published_only_where_every_stage_that_played_reported():
    """A stage that played and reported nothing would fall through to the
    line of the stage before it, which is a figure over part of the test.
    A skipped stage is the ordinary case and is not that."""
    jobs = workflow(STRENGTH)["jobs"]
    guard = jobs["release-notes"]["if"]
    names = list(stages(jobs))
    assert f"needs.{names[0]}.outputs.line != ''" in guard
    for name in names[1:]:
        assert (
            f"(needs.{name}.result == 'skipped' || needs.{name}.outputs.line != '')"
        ) in guard, f"{name} playing and reporting nothing publishes a stale line"


def test_the_stages_count_from_zero_without_a_gap():
    ladder = stages(workflow(STRENGTH)["jobs"])
    played = [job["with"]["batch"] for job in ladder.values()]
    assert played == list(range(len(ladder))), (
        "the batch index is what offsets a batch's slice of the book, so a "
        f"repeat or a gap replays another batch's openings: {played}"
    )


def test_every_stage_reserves_the_book_for_the_whole_ladder():
    ladder = stages(workflow(STRENGTH)["jobs"])
    for name, job in ladder.items():
        assert job["with"]["batches"] == (
            "${{ fromJSON(needs.resolve.outputs.batches) }}"
        ), f"{name} reserves for a different number of batches than the rest"


def test_the_stages_share_one_seed():
    """The seed picks the region of the book, and the batch index offsets
    inside it. Two stages on different seeds would land wherever the two
    regions happened to, which is the failure a reserved book prevents."""
    ladder = stages(workflow(STRENGTH)["jobs"])
    seeds = {job["with"]["seed"] for job in ladder.values()}
    assert seeds == {"${{ inputs.seed || github.run_id }}"}, seeds


def test_a_run_is_refused_above_the_stages_there_are():
    strength = workflow(STRENGTH)
    ladder = stages(strength["jobs"])
    checks = [
        CEILING.search(step.get("run", ""))
        for step in strength["jobs"]["resolve"]["steps"]
        if CEILING.search(step.get("run", ""))
    ]
    assert len(checks) == 1, "one place says how many batches a run may ask for"
    assert int(checks[0].group(1)) == len(ladder), (
        "a run may ask for more batches than the ladder has stages, and the "
        "ones past the end would skip in silence"
    )


def test_the_stages_hand_over_every_input_the_batch_workflow_takes():
    """A reusable workflow refuses a call that misses a required input or
    names one it does not take, so this only moves the failure off the runner
    and into the tests. It is worth the move: the call is an hour of play in."""
    # yaml reads a bare on: as a boolean, so the triggers are under True
    takes = workflow(BATCH)[True]["workflow_call"]["inputs"]
    required = {name for name, spec in takes.items() if spec.get("required")}
    for name, job in stages(workflow(STRENGTH)["jobs"]).items():
        handed = set(job["with"])
        assert required <= handed, f"{name} misses {sorted(required - handed)}"
        assert handed <= set(takes), f"{name} names {sorted(handed - set(takes))}"


def test_a_typed_input_is_never_handed_a_dispatch_input_raw():
    """An input reaches a dispatch run as a string whatever its type says.

    A reusable workflow call that hands a string to an input declared number
    or boolean is refused when the template is read, so the stage never
    becomes a job at all: no failed job, no log, and the stages after it skip
    on a `needs` that never reported. Runs 35696029101 and 35696034898 both
    ended that way.

    So a number or a boolean the batch workflow takes is either written out,
    or read through fromJSON, which a job output always survives. The one
    other safe shape is an input the dispatch tab does not offer: it is empty
    there, so the `|| <default>` beside it supplies the literal, and on a call
    it arrives as the type it declares.
    """
    strength = workflow(STRENGTH)
    # yaml reads a bare on: as a boolean, so the triggers are under True
    triggers = strength[True]
    dispatch = set((triggers["workflow_dispatch"] or {}).get("inputs") or {})
    takes = workflow(BATCH)[True]["workflow_call"]["inputs"]
    typed = {name for name, spec in takes.items() if spec["type"] != "string"}

    for name, job in stages(strength["jobs"]).items():
        for key in typed & set(job["with"]):
            value = str(job["with"][key])
            if "${{" not in value or "fromJSON(" in value:
                continue
            offered = sorted(
                read
                for read in re.findall(r"inputs\.([A-Za-z0-9_]+)", value)
                if read in dispatch
            )
            assert not offered, (
                f"{name} hands {key}, which is a {takes[key]['type']}, an"
                f" expression reading {offered} off the dispatch tab, where"
                " every input is a string. The call is refused before the"
                " stage becomes a job"
            )


def test_the_published_line_is_the_last_batch_that_played():
    """Each summary reads every pair the test has played, not just its own, so
    the line to publish is the newest one and not the sum of them."""
    jobs = workflow(STRENGTH)["jobs"]
    names = list(stages(jobs))
    line = jobs["release-notes"]["with"]["line"]
    reads = re.findall(r"needs\.(\w+)\.outputs\.line", line)
    assert reads == list(reversed(names)), reads
