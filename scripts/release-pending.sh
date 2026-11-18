#!/usr/bin/env bash
# Says whether version.txt names a release that has not been tagged yet: the
# state right after release-please's pull request is merged, before
# release-please has created the release. The Release workflow builds the
# binaries first in that state, so a release is published only once its
# binaries exist. Prints two lines for $GITHUB_OUTPUT:
#   pending=true|false
#   tag=v<version>
# Tags must have been fetched (git fetch --tags).
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
version="$(tr -d '[:space:]' < "$repo/version.txt")"
tag="v$version"

if [ "$version" = "0.0.0" ]; then
    # Nothing has been released and no release pull request has been merged.
    pending=false
elif git -C "$repo" rev-parse -q --verify "refs/tags/$tag" >/dev/null; then
    pending=false
else
    pending=true
fi
printf 'pending=%s\ntag=%s\n' "$pending" "$tag"
