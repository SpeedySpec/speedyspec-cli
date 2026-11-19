#!/usr/bin/env bash
# Checks that every path copied from speedyspec-server still matches the hash recorded in copied-from.json.
#
#   scripts/check-copies.sh [root]          check (the repository root by default)
#   scripts/check-copies.sh --hash <dir>    print a directory's content hash
#
# A path's hash is the SHA-256 of its files' relative names and SHA-256s, in byte order (target/ excluded), so it
# depends only on the files' contents and names. A drifted path is named, and the script fails.
set -euo pipefail

hash_dir() {
    (
        cd "$1"
        find . -type f ! -path './target/*' | LC_ALL=C sort | while IFS= read -r file; do
            printf '%s %s\n' "$(shasum -a 256 < "$file" | cut -d' ' -f1)" "$file"
        done
    ) | shasum -a 256 | cut -d' ' -f1
}

if [ "${1:-}" = "--hash" ]; then
    hash_dir "$2"
    exit 0
fi

root="${1:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
record="$root/copied-from.json"
[ -f "$record" ] || { echo "no copied-from.json at $record" >&2; exit 1; }

drifted=0
# Each recorded path is one line: "path": "hash",
while IFS= read -r line; do
    path=$(printf '%s' "$line" | sed -E 's/^ *"([^"]+)": *"([0-9a-f]{64})".*/\1/')
    want=$(printf '%s' "$line" | sed -E 's/^ *"([^"]+)": *"([0-9a-f]{64})".*/\2/')
    if [ ! -d "$root/$path" ]; then
        echo "copied path $path is missing" >&2
        drifted=1
        continue
    fi
    got=$(hash_dir "$root/$path")
    if [ "$got" != "$want" ]; then
        echo "copied path $path differs from speedyspec-server's (hash $got, recorded $want)" >&2
        drifted=1
    fi
done < <(grep -E '^ *"[^"]+": *"[0-9a-f]{64}"' "$record")

if [ "$drifted" -ne 0 ]; then
    echo "re-copy from speedyspec-server, or record the change there first" >&2
    exit 1
fi
echo "every copied path matches copied-from.json"
