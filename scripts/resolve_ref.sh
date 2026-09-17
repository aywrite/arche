#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2022-2026 Andrew Wright

# Resolve what a workflow was asked to play into a commit, and print it.
#
# A number is a pull request, whose head a checkout does not fetch. Anything
# else is a branch, a tag or a commit. A script rather than workflow text so
# that shellcheck sees it and both match workflows resolve a ref the same way.
set -euo pipefail

# The caller builds and runs whatever this prints, and a pull request head is
# anybody's to write. Under pull_request_target, workflow_run or a comment
# event the job also holds this repository's secrets and a write token, so
# the two together would hand a fork the repository. Only the triggers where
# the ref was chosen by somebody who can already push are allowed.
#
# GITHUB_EVENT_NAME is the one part of that a step can check: the runner sets
# it on every job, an env block cannot override a GITHUB_ name, and a
# workflow_call sees its caller's event. A step cannot read permissions or
# secrets, and docs/DEVELOPMENT.md says what that leaves open. Unset means
# this is not a runner at all.
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

if sha=$(git rev-parse --verify --quiet "${ref}^{commit}"); then
    echo "$sha"
    exit 0
fi

git fetch -q origin "$ref"
git rev-parse FETCH_HEAD
