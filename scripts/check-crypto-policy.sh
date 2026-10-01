#!/usr/bin/env bash
# Enforces Stein's post-quantum-only cryptographic policy (SPEC §3), adapted
# from Sigilant, at the two places it could quietly erode:
#
#   1. Dependencies. AWS-LC (aws-lc-rs / aws-lc-sys) is the only signature and
#      key-exchange implementation allowed in the BUILD GRAPH, and BLAKE3 the only
#      hash. Not the lockfile: Cargo locks optional dependencies that are never
#      compiled, so the lockfile would cry wolf. Refused by name:
#        - classical-signature and classical key-exchange crates;
#        - other crypto backends (ring, OpenSSL, BoringSSL, native-tls);
#        - other ML-DSA implementations: one audited code path, not two that
#          could disagree about what verifies — a consensus split;
#        - FN-DSA / Falcon implementations in any standard build. The one
#          recorded exception (SPEC §3.5): the `fn-dsa` crate at exactly 0.4.0,
#          reachable ONLY through stein-crypto's `fn-dsa-experimental`
#          feature, for the FN-DSA test network. Checked in section 1b.
#
#   2. Source. AWS-LC necessarily *contains* classical algorithms; what matters is
#      that nothing in this workspace selects one. Library and binary code
#      (crates/*/src) must not name a classical signature or key-exchange
#      algorithm. Comments may discuss them.
#
# Run from anywhere; CI runs it in the Rust job.

set -euo pipefail
cd "$(dirname "$0")/.."

fail=0

# --- 1. Build graph --------------------------------------------------------------
forbidden_packages='
ring
openssl openssl-sys boringssl boring boring-sys native-tls
rsa ecdsa dsa
ed25519 ed25519-dalek ed25519-compact ed448-goldilocks
p224 p256 p384 p521 k256 secp256k1
x25519-dalek curve25519-dalek
bls12_381 blst
ml-dsa fips204 pqcrypto-dilithium pqcrypto-mldsa pqcrypto-traits oqs oqs-sys
fn-dsa fn-dsa-sign fn-dsa-vrfy fn-dsa-kgen fn-dsa-comm falcon-rs pqcrypto-falcon
'
built=$(cargo tree --workspace --locked --target all --edges normal,build,dev --prefix none --format '{p}' \
  | awk '{print $1}' | sort -u)
if [ -z "$built" ]; then
  echo "crypto-policy: could not read the build graph" >&2
  exit 1
fi
for package in $forbidden_packages; do
  if grep -qx "$package" <<<"$built"; then
    echo "crypto-policy: forbidden package in the build graph: $package" >&2
    echo "               find what pulled it in with: cargo tree --target all -i $package" >&2
    fail=1
  fi
done

# --- 1b. The FN-DSA exception, held to exactly its size --------------------------
# With the experimental feature on, fn-dsa may appear, at exactly 0.4.0 (a
# different draft implementation could disagree about which signatures verify,
# a consensus split), and nothing but stein-crypto may depend on it.
experimental=$(cargo tree --workspace --locked --target all --edges normal,build \
  --features stein-crypto/fn-dsa-experimental --prefix none --format '{p}' | awk '{print $1, $2}' | sort -u)
fn_dsa=$(grep -E '^fn-dsa ' <<<"$experimental" || true)
if [ "$fn_dsa" != "fn-dsa v0.4.0" ]; then
  echo "crypto-policy: the experimental build must contain fn-dsa v0.4.0 exactly, found: ${fn_dsa:-none}" >&2
  fail=1
fi
for other in falcon-rs pqcrypto-falcon pqcrypto-mldsa ml-dsa ring; do
  if grep -qE "^$other " <<<"$experimental"; then
    echo "crypto-policy: forbidden package in the experimental build: $other" >&2
    fail=1
  fi
done
dependents=$(cargo tree --workspace --locked --target all --edges normal,build \
    --features stein-crypto/fn-dsa-experimental -i fn-dsa@0.4.0 --depth 1 --prefix none --format '{p}' \
  | awk '{print $1}' | sort -u | grep -vx fn-dsa || true)
if [ "$dependents" != "stein-crypto" ]; then
  echo "crypto-policy: fn-dsa may be used by stein-crypto only, but is used by:" >&2
  echo "$dependents" | sed 's/^/               /' >&2
  fail=1
fi

# --- 2. Source -------------------------------------------------------------------
# Identifiers from aws-lc-rs and rustls. Post-quantum hybrids (X25519MLKEM768)
# are allowed and do not match: \b requires the name to end where the pattern
# does.
classical='\b(ECDSA_P[0-9]+\w*|ED25519\w*|RSA_PKCS1\w*|RSA_PSS\w*|ECDH_P[0-9]+|X25519|SECP256R1|SECP384R1|HMAC_SHA1\w*|TLS12)\b'
hits=$(grep -rEn --include='*.rs' "$classical" crates/*/src | grep -Ev '^[^:]+:[0-9]+:[[:space:]]*//' || true)
if [ -n "$hits" ]; then
  echo "crypto-policy: classical algorithm selected in library or binary source:" >&2
  echo "$hits" >&2
  fail=1
fi

if [ "$fail" -ne 0 ]; then
  echo "crypto-policy: post-quantum only — see docs/SPEC.md §3" >&2
  exit 1
fi
echo "crypto-policy: ok ($(wc -l <<<"$built" | tr -d ' ') packages in the build graph, no classical cryptography selected)"
