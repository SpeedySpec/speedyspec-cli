#!/usr/bin/env bash
# Checks a built speedyspec binary before it is published:
#   scripts/smoke.sh <binary>
# - `--version` names the version in version.txt and a contracts version;
# - it runs a PHP file;
# - on macOS, neither the binary nor the static PHP in it demands a macOS newer
#   than the deployment target in .cargo/config.toml.
set -euo pipefail

if [ "$#" -ne 1 ]; then
    echo "usage: scripts/smoke.sh <binary>" >&2
    exit 2
fi
repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
binary="$1"
fail() {
    echo "smoke.sh: $*" >&2
    exit 1
}

version="$(tr -d '[:space:]' < "$repo/version.txt")"
output="$("$binary" --no-plugins --version)" || fail "$binary --version failed"
first="$(printf '%s\n' "$output" | sed -n 1p)"
[ "$first" = "speedyspec $version" ] || fail "--version says \"$first\", not \"speedyspec $version\""
printf '%s\n' "$output" | grep -q '^contracts [0-9]' || fail "--version names no contracts version"

"$binary" --no-plugins php "$repo/examples/install-wordpress/plugin.php" >/dev/null \
    || fail "$binary could not run a PHP file"

if [ "$(uname -s)" = "Darwin" ]; then
    target="$(sed -n 's/^MACOSX_DEPLOYMENT_TARGET = "\(.*\)"$/\1/p' "$repo/.cargo/config.toml")"
    [ -n "$target" ] || fail "no MACOSX_DEPLOYMENT_TARGET in .cargo/config.toml"
    minos="$(otool -l "$binary" | awk '/LC_BUILD_VERSION/ { seen = 1 } seen && $1 == "minos" { print $2; exit }')"
    [ "$minos" = "$target" ] || fail "$binary needs macOS $minos, not $target"
    php_lib="${SPEEDYSPEC_PHP_LIB:-$repo/.php-build/buildroot/lib/libphp.a}"
    if [ -f "$php_lib" ]; then
        newest="$( (otool -l "$php_lib" | awk '$1 == "minos" { print $2 }'; echo "$target") | sort -V | tail -1)"
        [ "$newest" = "$target" ] || fail "$php_lib holds objects for macOS $newest, newer than $target"
    fi
fi
echo "smoke.sh: $binary is speedyspec $version and runs"
