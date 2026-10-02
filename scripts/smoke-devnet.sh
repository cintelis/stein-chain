#!/usr/bin/env bash
# End-to-end smoke test of the real binaries: initialise a devnet, run the node,
# send STEIN from an ML-DSA-44 account to an ML-DSA-65 account and back, kill
# the node, restart it (the block log is re-verified), and check that balances
# survived. Then node to node: start a follower pinned to the validator's
# genesis, send a transfer through the follower, and check that both nodes
# agree on balances and state root. What the test suites, which drive the
# libraries, do not cover.
#
# Usage: scripts/smoke-devnet.sh [target-dir]   (default: target/debug)

set -euo pipefail
cd "$(dirname "$0")/.."

bin=${1:-target/debug}
node="$bin/stein-node"
wallet="$bin/stein"
port=${SMOKE_PORT:-18700}
rpc="http://127.0.0.1:$port"
follower_rpc="http://127.0.0.1:$((port + 1))"
dir=$(mktemp -d)
pid=""
follower_pid=""

cleanup() {
  [ -n "$pid" ] && kill "$pid" 2>/dev/null || true
  [ -n "$follower_pid" ] && kill "$follower_pid" 2>/dev/null || true
  if [ "${status:-1}" -ne 0 ] && [ -f "$dir/node.log" ]; then
    echo "--- node log ---" >&2
    cat "$dir/node.log" >&2
    if [ -f "$dir/follower.log" ]; then
      echo "--- follower log ---" >&2
      cat "$dir/follower.log" >&2
    fi
  fi
  rm -rf "$dir"
}
trap 'status=$?; cleanup' EXIT

start_node() {
  "$node" --data-dir "$dir/chain" run --rpc "127.0.0.1:$port" >>"$dir/node.log" 2>&1 &
  pid=$!
  for _ in $(seq 60); do
    curl -fsS "$rpc/status" >/dev/null 2>&1 && return 0
    sleep 0.5
  done
  echo "smoke: node did not start" >&2
  return 1
}

stop_node() {
  kill "$pid"
  wait "$pid" 2>/dev/null || true
  pid=""
}

balance() { # <address> [rpc] -> STEIN amount
  "$wallet" --rpc "${2:-$rpc}" balance "$1" | awk '{print $1}'
}

field() { # <rpc> <field> -> value of that field in /status (string or number)
  curl -fsS "$1/status" | tr ',{}' '\n\n\n' | grep "\"$2\":" | cut -d: -f2- | tr -d '"'
}

expect() { # <label> <expected> <actual>
  if [ "$2" != "$3" ]; then
    echo "smoke: $1: expected $2, got $3" >&2
    exit 1
  fi
  echo "smoke: $1 = $3"
}

wait_height() { # <rpc> <height>
  for _ in $(seq 60); do
    [ "$(field "$1" height 2>/dev/null || true)" = "$2" ] && return 0
    sleep 0.5
  done
}

"$node" --data-dir "$dir/chain" init >/dev/null
start_node

bob=$("$wallet" keygen --out "$dir/bob.seed" 2>/dev/null)
carol=$("$wallet" keygen --scheme ml-dsa-65 --out "$dir/carol.seed" 2>/dev/null)

"$wallet" --rpc "$rpc" send --key "$dir/chain/faucet.seed" --to "$bob" --amount 100
"$wallet" --rpc "$rpc" send --key "$dir/bob.seed" --to "$carol" --amount 40
"$wallet" --rpc "$rpc" send --key "$dir/carol.seed" --to "$bob" --amount 15

# Fees are 10 base units per byte: bob paid one key-registering ML-DSA-44
# transfer (3,857 B), carol one key-registering ML-DSA-65 transfer (5,386 B).
expect "bob before restart" "74.99996143" "$(balance "$bob")"
expect "carol before restart" "24.99994614" "$(balance "$carol")"

# A typo in an address must be caught by its checksum, not sent.
typo="${bob%?}$([ "${bob: -1}" = 0 ] && echo 1 || echo 0)"
if "$wallet" --rpc "$rpc" balance "$typo" >/dev/null 2>&1; then
  echo "smoke: a mistyped address was accepted" >&2
  exit 1
fi

stop_node
start_node
grep -q "replayed and re-verified block log height=3" "$dir/node.log" \
  || { echo "smoke: restart did not replay 3 blocks" >&2; exit 1; }
expect "bob after restart" "74.99996143" "$(balance "$bob")"
expect "carol after restart" "24.99994614" "$(balance "$carol")"

# --- Node to node ------------------------------------------------------------
genesis_hash=$(field "$rpc" genesis_hash)
"$node" --data-dir "$dir/follower" follow --leader "$rpc" --genesis-hash "$genesis_hash" \
  --rpc "127.0.0.1:$((port + 1))" >>"$dir/follower.log" 2>&1 &
follower_pid=$!
wait_height "$follower_rpc" 3
expect "follower synced height" "3" "$(field "$follower_rpc" height)"
expect "follower role" "follower" "$(field "$follower_rpc" role)"

# Sent to the follower: it verifies the transfer, forwards it to the validator,
# then downloads and re-verifies the block that includes it.
"$wallet" --rpc "$follower_rpc" send --key "$dir/bob.seed" --to "$carol" --amount 4
wait_height "$follower_rpc" 4
expect "carol on validator" "28.99994614" "$(balance "$carol")"
expect "carol on follower" "28.99994614" "$(balance "$carol" "$follower_rpc")"
expect "bob on follower" "$(balance "$bob")" "$(balance "$bob" "$follower_rpc")"
expect "state roots agree" "$(field "$rpc" state_root)" "$(field "$follower_rpc" state_root)"

echo "smoke: ok"
