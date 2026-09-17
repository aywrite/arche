#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2022-2026 Andrew Wright

# Writes the changelog section for a release. Run by cargo-release as a
# pre-release hook, with the version being released as the only argument.
set -euo pipefail

version="${1:?usage: changelog.sh <version>}"

# Since the last full release rather than the last tag, so a release preceded
# by candidates still lists all of its changes. git-cliff's --ignore-tags does
# not affect the range --unreleased picks.
#
# grep exits 1 on finding nothing, and under pipefail the || true is what
# makes the empty answer the answer.
previous=$(git tag --list 'v[0-9]*' --sort=-v:refname | grep -v -- '-' | head -1 || true)

# A candidate gets a section, since the github release is created from the
# section matching the tag. It covers the same range the release will, so the
# next section written replaces it. Dropping any section for this version too
# makes the hook safe to run twice over.
if [ -f CHANGELOG.md ]; then
  awk -v version="$version" '
    # the version is what is inside the brackets, so that the date after them,
    # which always has dashes in it, is not read as a pre-release
    /^## \[/ {
      bracketed = $0
      sub(/^## \[/, "", bracketed)
      sub(/\].*/, "", bracketed)
      superseded = (bracketed ~ /-/) || (bracketed == version)
    }
    !superseded
  ' CHANGELOG.md > CHANGELOG.md.tmp
  mv CHANGELOG.md.tmp CHANGELOG.md
fi

if [ -n "$previous" ]; then
  echo "changelog: ${version}, covering ${previous}..HEAD"
  git-cliff --tag "$version" "${previous}..HEAD" --prepend CHANGELOG.md
else
  echo "changelog: ${version}, no previous release, covering everything"
  git-cliff --tag "$version" --prepend CHANGELOG.md
fi
