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
