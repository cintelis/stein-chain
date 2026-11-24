#!/usr/bin/env bash
# Checks Stein's FN-DSA-512 against a second implementation (SPEC §3.5).
#
# The chain has one FN-DSA implementation, the `fn-dsa` crate at 0.4.0, and
# what it accepts is what the chain accepts. This script asks whether a second
# implementation of the same draft agrees with it: the C code by the same
# author. Both are given the test vectors that stein-crypto makes from fixed
# seeds (crates/stein-crypto/src/fn_dsa_draft/vectors.rs), and the C code must
#
#   - make the same key pair from each seed;
#   - make the same signature from each seed;
#   - accept each signature Stein accepts, and reject each one Stein rejects:
#     another key, a changed message, a changed bit, a wrong length.
#
# The C code is built twice: as it comes, using the processor's floating point,
# and with that turned off, so that signing uses integers only. Both must make
# the same signatures as Stein, which shows that the two kinds of arithmetic
# round alike.
#
# The C code is a test fixture. It is fetched here at one fixed commit, into
# target/, and is never part of a Stein build: check-crypto-policy.sh still
# holds the build to one implementation.
#
# Needs git, a C compiler (CC, default cc) and a network. CI runs it on Linux
# and macOS.
#
# Usage: scripts/cross-check-fn-dsa.sh

set -euo pipefail
cd "$(dirname "$0")/.."

repository=https://github.com/pornin/c-fn-dsa.git
# 2026-09-21. The draft it implements is the one fn-dsa 0.4.0 does: both were
# brought to it on 2026-07-22.
commit=1b858880dcac091b5e5f84f0e4d8617c35ba1392

work=target/cross-check-fn-dsa
src=$work/c-fn-dsa
cc=${CC:-cc}

# --- 1. The C code, at exactly that commit ---------------------------------------
mkdir -p "$work"
if [ ! -d "$src/.git" ] || [ "$(git -C "$src" rev-parse HEAD 2>/dev/null)" != "$commit" ]; then
  rm -rf "$src"
  git init -q "$src"
  git -C "$src" fetch -q --depth 1 "$repository" "$commit"
  git -C "$src" -c advice.detachedHead=false checkout -q FETCH_HEAD
fi
if [ "$(git -C "$src" rev-parse HEAD)" != "$commit" ] || [ -n "$(git -C "$src" status --porcelain)" ]; then
  echo "cross-check-fn-dsa: $src is not a clean checkout of $commit" >&2
  exit 1
fi

# --- 2. Stein's vectors ----------------------------------------------------------
vectors=$PWD/$work/vectors.txt
# A Windows program does not know a path that starts /c/.
command -v cygpath >/dev/null 2>&1 && vectors=$(cygpath -m "$vectors")
rm -f "$vectors"
STEIN_FN_DSA_VECTORS=$vectors cargo test --locked -p stein-crypto --lib \
  fn_dsa_draft::vectors >/dev/null
if [ ! -s "$vectors" ]; then
  echo "cross-check-fn-dsa: the test wrote no vectors" >&2
  exit 1
fi

# --- 3. The same vectors, through the C code -------------------------------------
parts='codec mq sha3 sysrng util
       kgen kgen_fxp kgen_gauss kgen_mp31 kgen_ntru kgen_poly kgen_zint31
       sign sign_core sign_fpoly sign_fpr sign_sampler
       vrfy'
sources=()
for part in $parts; do
  sources+=("$src/$part.c")
done

run() {
  local name=$1
  shift
  echo "cross-check-fn-dsa: $name"
  "$cc" -O2 "$@" -I "$src" -o "$work/check-$name" \
    scripts/cross-check-fn-dsa/check.c "${sources[@]}"
  "$work/check-$name" "$vectors"
}

run hardware-floating-point
run integers-only -DFNDSA_AVX2=0 -DFNDSA_SSE2=0 -DFNDSA_NEON=0 -DFNDSA_RV64D=0

echo "cross-check-fn-dsa: ok"
