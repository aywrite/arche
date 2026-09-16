#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2022-2026 Andrew Wright

# Resolve what a workflow was asked to play into a commit, and print it.
#
# A number is a pull request, whose head a checkout does not fetch. Anything
# else is a branch, a tag or a commit, which a full clone may already have and
# otherwise has to be asked for by name.
#
# Lives here rather than inside a workflow so that the shellcheck hook sees it,
# and so that the two match workflows resolve a ref the same way rather than
# each having their own idea of what a ref is.
set -euo pipefail

# Refuse outside the triggers where the ref was chosen by somebody who can
# already push here. A number is a pull request, whose head anybody can write,
# and the caller builds and runs whatever this prints. Under a trigger such as
# pull_request_target or workflow_run the job also holds this repository's
# secrets and a token that can write to it, and the two together hand a fork
# the repository.
#
# GITHUB_EVENT_NAME is what this checks, because it is the only part of that
# worth checking from here. The runner sets it on every job, an env block
# cannot override a GITHUB_ name, and a workflow called with workflow_call sees
# the event of the workflow that called it, so a call through a reusable
# workflow is checked as well. It is not a check on permissions or on secrets:
# a step cannot read either, and docs/DEVELOPMENT.md says what that leaves
# open. Unset means this is not a runner at all, where there is nothing to
# take.
case "${GITHUB_EVENT_NAME-}" in
    "" | workflow_dispatch | push | schedule | release) ;;
    *)
        echo "resolve_ref.sh: refusing to resolve a ref under" \
            "${GITHUB_EVENT_NAME}. It builds a ref that somebody without" \
            "write access can choose, and that trigger runs with this" \
            "repository's secrets." >&2
        exit 1
        ;;
esac

ref=${1:?usage: resolve_ref.sh <branch|tag|commit|pull request number>}

if [ "$ref" -eq "$ref" ] 2>/dev/null; then
    git fetch -q origin "refs/pull/${ref}/head"
    git rev-parse FETCH_HEAD
    exit 0
fi

# already here, so nothing to fetch
if sha=$(git rev-parse --verify --quiet "${ref}^{commit}"); then
    echo "$sha"
    exit 0
fi

git fetch -q origin "$ref"
git rev-parse FETCH_HEAD
