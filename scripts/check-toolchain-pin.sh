#!/usr/bin/env bash
# The Rust compiler version is pinned (as in Sigilant). It is stated in three
# places that must agree:
#
#   rust-toolchain.toml          what developers and CI build with
#   deploy/docker/Dockerfile     what node images (validators, Cloudflare
#                                followers) are built with
#   Cargo.toml  rust-version     the oldest compiler the workspace claims to support
#
# A node image built with a different compiler from the one the tests ran
# under is exactly the kind of drift a pin exists to prevent. Upgrading the
# compiler means changing all three in one commit; this check fails otherwise.

set -euo pipefail
cd "$(dirname "$0")/.."

pinned=$(sed -nE 's/^channel *= *"([0-9]+\.[0-9]+\.[0-9]+)"$/\1/p' rust-toolchain.toml)
if [ -z "$pinned" ]; then
  echo "toolchain-pin: rust-toolchain.toml must pin an exact version, e.g. channel = \"1.98.1\" (not \"stable\")" >&2
  exit 1
fi

fail=0
# The image may also be pinned by digest: rust:1.98.1-bookworm@sha256:… AS build
image=$(sed -nE 's/^FROM rust:([^ @]+)(@sha256:[0-9a-f]+)? AS build$/\1/p' deploy/docker/Dockerfile)
if [ "$image" != "$pinned-bookworm" ]; then
  echo "toolchain-pin: Dockerfile builds with rust:$image, but rust-toolchain.toml pins $pinned (expected rust:$pinned-bookworm)" >&2
  fail=1
fi

msrv=$(sed -nE 's/^rust-version *= *"([0-9]+\.[0-9]+)"$/\1/p' Cargo.toml)
if [ "$msrv" != "${pinned%.*}" ]; then
  echo "toolchain-pin: Cargo.toml rust-version is $msrv, but only ${pinned%.*} is ever built or tested" >&2
  fail=1
fi

[ "$fail" -eq 0 ] || exit 1
echo "toolchain-pin: ok (Rust $pinned everywhere)"
