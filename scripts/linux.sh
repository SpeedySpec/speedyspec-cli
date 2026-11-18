#!/usr/bin/env bash
# Runs a command in a Linux musl container with this repository mounted.
#
#   scripts/linux.sh scripts/build-php.sh
#   scripts/linux.sh cargo test -p speedyspec-php --test php
#
# The container sees the repository at /work. Its PHP build and cargo target
# live in .php-build-linux/ and target-linux/ on the host, mounted over
# /work/.php-build and /work/target, so they never mix with the host's builds
# and every path inside the container is the default one.
#
# The sibling checkout ../speedyspec-cli-contracts, when present, is mounted
# read-only at /contracts (see SPEEDYSPEC_CONTRACTS_DIR in crates/speedyspec-cli/build.rs).
#
# SPEEDYSPEC_LINUX_PLATFORM picks the container platform (default linux/amd64).
# The container is thrown away after each run, so build-php.sh may install
# missing tools in it (SPEEDYSPEC_ALLOW_HOST_INSTALL=1).
set -euo pipefail

if [ "$#" -eq 0 ]; then
    echo "usage: scripts/linux.sh <command...>" >&2
    exit 2
fi

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
platform="${SPEEDYSPEC_LINUX_PLATFORM:-linux/amd64}"
suffix=""
if [ "$platform" != "linux/amd64" ]; then
    suffix="-${platform##*/}"
fi
php_build_dir="$repo/.php-build-linux$suffix"
target_dir="$repo/target-linux$suffix"
image="speedyspec-cli-linux:1.99-${platform##*/}"

case "$platform" in
    linux/amd64) rust_target="x86_64-unknown-linux-musl" ;;
    linux/arm64) rust_target="aarch64-unknown-linux-musl" ;;
    *) echo "scripts/linux.sh: unsupported platform $platform" >&2; exit 2 ;;
esac

# Build tools for static-php-cli (it would otherwise `apk add` them on every
# run, since each container starts fresh) and libclang for ext-php-rs's bindgen.
#
# Host artifacts (build scripts, proc macros) must link musl dynamically:
# bindgen dlopen()s libclang, and a static musl binary cannot dlopen. Cargo has
# no stable way to give host artifacts their own flags, so the container sets
# CARGO_BUILD_TARGET (target artifacts get `--target`, host ones do not) and a
# rustc wrapper turns crt-static off only for invocations without `--target`.
# Target artifacts, including every test binary, stay fully static.
docker build --quiet --platform "$platform" --tag "$image" - >/dev/null <<'EOF'
FROM rust:1.99-alpine
RUN apk add --no-cache \
    autoconf automake bash binutils bison build-base bzip2 ca-certificates \
    clang-dev cmake curl file flex g++ gettext-dev git gzip libtool \
    linux-headers m4 make patch perl pkgconf re2c tar unzip wget xz \
    && rustup component add rustfmt
RUN printf '%s\n' \
    '#!/bin/sh' \
    'rustc="$1"; shift' \
    'for arg in "$@"; do [ "$arg" = "--target" ] && exec "$rustc" "$@"; done' \
    'exec "$rustc" "$@" -C target-feature=-crt-static' \
    > /usr/local/bin/rustc-host-dynamic \
    && chmod +x /usr/local/bin/rustc-host-dynamic
ENV RUSTC_WRAPPER=/usr/local/bin/rustc-host-dynamic
EOF

mkdir -p "$php_build_dir" "$target_dir"

# speedyspec-cli embeds the PHP contracts from a sibling checkout, which is
# outside the repository mounted at /work, so mount it separately.
contracts_flags=()
contracts_dir="$repo/../speedyspec-cli-contracts"
if [ -d "$contracts_dir" ]; then
    contracts_dir="$(cd "$contracts_dir" && pwd)"
    contracts_flags=(--volume "$contracts_dir:/contracts:ro" --env SPEEDYSPEC_CONTRACTS_DIR=/contracts)
fi

tty_flag=()
if [ -t 0 ] && [ -t 1 ]; then
    tty_flag=(--tty --interactive)
fi

exec docker run --rm ${tty_flag[@]+"${tty_flag[@]}"} \
    --platform "$platform" \
    --volume "$repo:/work" \
    --volume "$php_build_dir:/work/.php-build" \
    --volume "$target_dir:/work/target" \
    --volume "speedyspec-cargo-registry-${platform##*/}:/usr/local/cargo/registry" \
    ${contracts_flags[@]+"${contracts_flags[@]}"} \
    --env CARGO_BUILD_TARGET="$rust_target" \
    --env SPEEDYSPEC_ALLOW_HOST_INSTALL=1 \
    --workdir /work \
    "$image" "$@"
