#!/usr/bin/env bash
# Says whether version.txt names a release that has not been tagged yet: the
# state right after release-please's pull request is merged, before
# release-please has created the release. The Release workflow builds the
# binaries first in that state, so a release is published only once its
# binaries exist. Prints three lines for $GITHUB_OUTPUT:
#   pending=true|false
#   tag=v<version>
#   commit=<the last commit that changed version.txt>
# That commit is the one release-please tags, so it is the one to build: a retry
# on a later push must not build the newer code on main under the old tag.
# Tags and the full history must have been fetched (git fetch --tags, and a
# checkout with fetch-depth 0); a shallow clone is refused, because there the
# last commit to touch version.txt would wrongly be HEAD.
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
if [ "$(git -C "$repo" rev-parse --is-shallow-repository)" = "true" ]; then
    echo "release-pending.sh: the clone is shallow; check out with fetch-depth 0" >&2
    exit 1
fi
commit="$(git -C "$repo" log -1 --format=%H -- version.txt)"
if [ -z "$commit" ]; then
    echo "release-pending.sh: no commit has changed version.txt" >&2
    exit 1
fi
printf 'pending=%s\ntag=%s\ncommit=%s\n' "$pending" "$tag" "$commit"
