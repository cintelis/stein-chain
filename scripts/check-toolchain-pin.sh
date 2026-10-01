#!/usr/bin/env bash
# The Rust compiler version is pinned (adapted from Sigilant). It is stated in
# two places that must agree:
#
#   rust-toolchain.toml          what developers and CI build with
#   Cargo.toml  rust-version     the oldest compiler the workspace claims to support
#
# Validators must run binaries built by the compiler the tests ran under.
# Upgrading the compiler means changing both in one commit (and, once release
# images exist, their Dockerfile too); this check fails otherwise.

set -euo pipefail
cd "$(dirname "$0")/.."

pinned=$(sed -nE 's/^channel *= *"([0-9]+\.[0-9]+\.[0-9]+)"$/\1/p' rust-toolchain.toml)
if [ -z "$pinned" ]; then
  echo "toolchain-pin: rust-toolchain.toml must pin an exact version, e.g. channel = \"1.98.1\" (not \"stable\")" >&2
  exit 1
fi

msrv=$(sed -nE 's/^rust-version *= *"([0-9]+\.[0-9]+)"$/\1/p' Cargo.toml)
if [ "$msrv" != "${pinned%.*}" ]; then
  echo "toolchain-pin: Cargo.toml rust-version is $msrv, but only ${pinned%.*} is ever built or tested" >&2
  exit 1
fi
echo "toolchain-pin: ok (Rust $pinned everywhere)"
