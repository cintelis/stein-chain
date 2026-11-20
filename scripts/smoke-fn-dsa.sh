#!/usr/bin/env bash
# Smoke test of FN-DSA accounts (draft FIPS 206, SPEC §3.5) with the real
# binaries:
#
#   - `init` creates a network whose faucet is an FN-DSA account by default
#     and whose validator stays on ML-DSA; `keygen` creates FN-DSA keys by
#     default;
#   - FN-DSA and ML-DSA accounts transfer to each other, at the measured sizes
#     (1,688 bytes with key registration, 790 bytes after) and exact fees;
#   - a follower re-verifies the FN-DSA signatures and agrees on the state root;
#   - a network created with --ml-dsa-only refuses FN-DSA keys.
#
# Usage: scripts/smoke-fn-dsa.sh [target-dir]   (default: target/debug)

set -euo pipefail
cd "$(dirname "$0")/.."

bin=${1:-target/debug}
node="$bin/stein-node"
wallet="$bin/stein"
port=${SMOKE_PORT:-18720}
rpc="http://127.0.0.1:$port"
follower_rpc="http://127.0.0.1:$((port + 1))"
dir=$(mktemp -d)
pids=()

cleanup() {
  for p in "${pids[@]}"; do kill "$p" 2>/dev/null || true; done
  if [ "${status:-1}" -ne 0 ]; then
    for log in "$dir"/*.log; do
      [ -f "$log" ] && { echo "--- $log ---" >&2; cat "$log" >&2; }
    done
  fi
  rm -rf "$dir"
}
trap 'status=$?; cleanup' EXIT

field() { # <rpc> <field> -> value of that field in /status
  curl -fsS "$1/status" | tr ',{}' '\n\n\n' | grep "\"$2\":" | cut -d: -f2- | tr -d '"'
}

balance() { # <address> [rpc] -> STEIN amount
  "$wallet" --rpc "${2:-$rpc}" balance "$1" | awk '{print $1}'
}

expect() { # <label> <expected> <actual>
  if [ "$2" != "$3" ]; then
    echo "smoke-fn-dsa: $1: expected $2, got $3" >&2
    exit 1
  fi
  echo "smoke-fn-dsa: $1 = $3"
}

wait_up() { # <rpc>
  for _ in $(seq 60); do
    curl -fsS "$1/status" >/dev/null 2>&1 && return 0
    sleep 0.5
  done
  echo "smoke-fn-dsa: $1 did not start" >&2
  return 1
}

wait_height() { # <rpc> <height>
  for _ in $(seq 60); do
    [ "$(field "$1" height 2>/dev/null || true)" = "$2" ] && return 0
    sleep 0.5
  done
}

# A validator never gets a draft-standard key.
if "$node" --data-dir "$dir/draft" init --validator-scheme fn-dsa-512-draft >/dev/null 2>&1; then
  echo "smoke-fn-dsa: a validator was given an FN-DSA key" >&2
  exit 1
fi

# An ML-DSA-only network refuses an FN-DSA faucet.
if "$node" --data-dir "$dir/standard" init --ml-dsa-only --scheme fn-dsa-512-draft >/dev/null 2>&1; then
  echo "smoke-fn-dsa: an ML-DSA-only network accepted an FN-DSA faucet" >&2
  exit 1
fi

"$node" --data-dir "$dir/chain" init | tee "$dir/init.log"
grep -q "accounts     : ml-dsa-44, ml-dsa-65, fn-dsa-512-draft" "$dir/init.log"
grep -q "key schemes  : validator ml-dsa-65, faucet fn-dsa-512-draft" "$dir/init.log"
"$node" --data-dir "$dir/chain" run --rpc "127.0.0.1:$port" >>"$dir/node.log" 2>&1 &
pids+=($!)
wait_up "$rpc"
expect "chain id" "31337" "$(field "$rpc" chain_id)"

frank=$("$wallet" keygen --out "$dir/frank.seed" 2>/dev/null)
grep -q '^fn-dsa-512-draft:' "$dir/frank.seed"
mary=$("$wallet" keygen --scheme ml-dsa-44 --out "$dir/mary.seed" 2>/dev/null)

send() { # <key> <to> <amount> [rpc] -> prints the encoded size
  "$wallet" --rpc "${4:-$rpc}" send --key "$1" --to "$2" --amount "$3" | tee -a "$dir/sends.log" \
    | sed -nE 's/.*\(([0-9]+) bytes.*/\1/p'
}

expect "FN-DSA transfer, registering the key" "1688" "$(send "$dir/chain/faucet.seed" "$frank" 100)"
expect "FN-DSA transfer, key registered" "790" "$(send "$dir/chain/faucet.seed" "$frank" 1)"
expect "FN-DSA account to ML-DSA account" "1688" "$(send "$dir/frank.seed" "$mary" 30)"
expect "ML-DSA account to FN-DSA account" "3857" "$(send "$dir/mary.seed" "$frank" 5)"
# Fees at 10 base units per byte: frank paid 1,688 B; mary paid 3,857 B.
expect "frank" "75.99998312" "$(balance "$frank")"
expect "mary" "24.99996143" "$(balance "$mary")"

# A follower re-verifies every FN-DSA signature before applying a block.
"$node" --data-dir "$dir/follower" follow --leader "$rpc" --genesis-hash "$(field "$rpc" genesis_hash)" \
  --rpc "127.0.0.1:$((port + 1))" >>"$dir/follower.log" 2>&1 &
pids+=($!)
wait_up "$follower_rpc"
wait_height "$follower_rpc" "$(field "$rpc" height)"
expect "follower: frank" "75.99998312" "$(balance "$frank" "$follower_rpc")"
expect "state roots agree" "$(field "$rpc" state_root)" "$(field "$follower_rpc" state_root)"

echo "smoke-fn-dsa: ok"
