#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Fetches files that the release workflow keeps off the release page (latest.json,
# the store uploads, the package manifests, the SBOMs) for a published release, and
# checks them against their SHA256SUMS.
#
#   release-internal.sh <tag> <dest dir> <file>...
#
# They are in the `release-internal` artifact of the release run for <tag>
# (release.yml, "Only what people need on the release page"). Releases made
# before that (1.0.0-beta.3 and older) attached them to the release itself, so
# those are taken from there. Needs GH_TOKEN and GITHUB_REPOSITORY, and the
# `actions: read` permission for the artifact.
#
# Exit status: 0 when every file is there and matches; 3 when a file is not
# there at all (a release built without an updater key has no latest.json);
# 1 on any other failure, a checksum mismatch included.
set -euo pipefail

tag=$1 dest=$2
shift 2
[ "$#" -gt 0 ] || { echo "usage: release-internal.sh <tag> <dest dir> <file>..." >&2; exit 2; }
mkdir -p "$dest"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

run=$(gh run list --repo "$GITHUB_REPOSITORY" --workflow release.yml --branch "$tag" --event push \
  --status success --limit 1 --json databaseId --jq '.[0].databaseId // empty')
if [ -n "$run" ] && gh run download "$run" --repo "$GITHUB_REPOSITORY" --name release-internal --dir "$work" 2>/dev/null; then
  echo "release-internal: from run $run of $tag"
else
  echo "release-internal: no internal artifact for $tag; taking the files attached to the release"
  patterns=(--pattern SHA256SUMS)
  for f in "$@"; do patterns+=(--pattern "$f"); done
  gh release download "$tag" --repo "$GITHUB_REPOSITORY" --dir "$work" "${patterns[@]}"
fi

for f in "$@"; do
  [ -e "$work/$f" ] || { echo "$f is not in the internal files of $tag" >&2; exit 3; }
  line=$(awk -v f="$f" '$2 == f || $2 == "*" f' "$work/SHA256SUMS")
  [ -n "$line" ] || { echo "::error::$f is not listed in SHA256SUMS"; exit 1; }
  (cd "$work" && printf '%s\n' "$line" | sha256sum -c -)
  mv "$work/$f" "$dest/"
done
