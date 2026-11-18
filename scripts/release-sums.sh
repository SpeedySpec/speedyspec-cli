#!/usr/bin/env bash
# Writes <dir>/SHA256SUMS for the release binaries in <dir> (every regular file
# named speedyspec-*), sorted by name, in `sha256sum` format:
#   scripts/release-sums.sh <dir>
# `speedyspec self update` reads this file back to verify its download.
set -euo pipefail

if [ "$#" -ne 1 ]; then
    echo "usage: scripts/release-sums.sh <dir>" >&2
    exit 2
fi
cd "$1"

names=()
for name in speedyspec-*; do
    [ -f "$name" ] && names+=("$name")
done
if [ "${#names[@]}" -eq 0 ]; then
    echo "release-sums.sh: no speedyspec-* file in $1" >&2
    exit 1
fi

if command -v sha256sum >/dev/null 2>&1; then
    hash=(sha256sum)
else
    hash=(shasum -a 256)
fi
# The glob already sorts by name; LC_ALL=C pins that order whatever the locale.
LC_ALL=C "${hash[@]}" "${names[@]}" > SHA256SUMS.tmp
mv SHA256SUMS.tmp SHA256SUMS
echo "release-sums.sh: $1/SHA256SUMS lists ${#names[@]} file(s)"
