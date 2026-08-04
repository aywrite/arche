#!/usr/bin/env bash
# Writes the changelog section for a release. Run by cargo-release as a
# pre-release hook, with the version being released as the only argument.
set -euo pipefail

version="${1:?usage: changelog.sh <version>}"

# Everything since the last full release rather than since the last tag, so that
# a release preceded by candidates still lists all of its changes. git-cliff has
# --ignore-tags, but it does not affect the range --unreleased picks.
previous=$(git tag --list 'v[0-9]*' --sort=-v:refname | grep -v -- '-' | head -1)

# A candidate does get a section, because the github release is created from the
# section matching the tag and there is nothing to create it from otherwise. It
# is a preview rather than a record though: it covers the same range the release
# will, so whatever is written next replaces it rather than joining it, and the
# release at the end of a run of candidates reads as though none of them
# happened.
#
# Dropping any section for this version too makes the hook safe to run twice
# over, which is worth having because the only sign that it had would be a
# changelog with the same release in it twice.
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
