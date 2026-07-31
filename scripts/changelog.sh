#!/usr/bin/env bash
# Writes the changelog section for a release. Run by cargo-release as a
# pre-release hook, with the version being released as the only argument.
set -euo pipefail

version="${1:?usage: changelog.sh <version>}"

# Release candidates do not get a section of their own. Their commits belong to
# the release they lead up to, and are written out when that release is cut.
case "$version" in
  *-*)
    echo "changelog: skipping pre-release $version"
    exit 0
    ;;
esac

# Everything since the last full release rather than since the last tag, so that
# a release preceded by candidates still lists all of its changes. git-cliff has
# --ignore-tags, but it does not affect the range --unreleased picks.
previous=$(git tag --list 'v[0-9]*' --sort=-v:refname | grep -v -- '-' | head -1)

if [ -n "$previous" ]; then
  echo "changelog: ${version}, covering ${previous}..HEAD"
  git-cliff --tag "$version" "${previous}..HEAD" --prepend CHANGELOG.md
else
  echo "changelog: ${version}, no previous release, covering everything"
  git-cliff --tag "$version" --prepend CHANGELOG.md
fi
