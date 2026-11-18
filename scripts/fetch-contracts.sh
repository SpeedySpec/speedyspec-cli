#!/usr/bin/env bash
# Fetches the PHP contracts release this CLI is pinned to (./contracts-version)
# into .contracts/<version>/, which crates/speedyspec-cli/build.rs embeds:
#   scripts/fetch-contracts.sh
# The archive is checked against the pinned SHA-256 before it is unpacked, and
# a version already fetched with that hash is not downloaded again.
# SPEEDYSPEC_CONTRACTS_PIN names another pin file (the tests use it).
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
pin="${SPEEDYSPEC_CONTRACTS_PIN:-$repo/contracts-version}"

value() {
    sed -n "s/^$1=//p" "$pin" | head -1
}
version="$(value version)"
url="$(value url)"
sha256="$(value sha256)"
if [ -z "$version" ] || [ -z "$url" ] || [ -z "$sha256" ]; then
    echo "fetch-contracts.sh: $pin must have version=, url= and sha256= lines" >&2
    exit 1
fi

sha256_of() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | cut -d' ' -f1
    else
        shasum -a 256 "$1" | cut -d' ' -f1
    fi
}

contracts="$repo/.contracts"
dest="$contracts/$version"
if [ -f "$dest/.sha256" ] && [ "$(cat "$dest/.sha256")" = "$sha256" ]; then
    echo "fetch-contracts.sh: .contracts/$version is up to date"
    exit 0
fi

mkdir -p "$contracts"
work="$(mktemp -d "$contracts/.fetch.XXXXXX")"
trap 'rm -rf "$work"' EXIT

curl -fsSL -o "$work/archive.tar.gz" "$url"
actual="$(sha256_of "$work/archive.tar.gz")"
if [ "$actual" != "$sha256" ]; then
    echo "fetch-contracts.sh: checksum mismatch for $url" >&2
    echo "  expected $sha256" >&2
    echo "  actual   $actual" >&2
    exit 1
fi

mkdir "$work/unpacked"
tar -xzf "$work/archive.tar.gz" -C "$work/unpacked" --strip-components=1
printf '%s\n' "$sha256" > "$work/unpacked/.sha256"
rm -rf "$dest"
mv "$work/unpacked" "$dest"
echo "fetch-contracts.sh: fetched contracts $version into .contracts/$version"
