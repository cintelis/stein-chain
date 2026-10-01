#!/usr/bin/env bash
# End-to-end smoke test of the real binaries: initialise a devnet, run the node,
# send STEIN from an ML-DSA-44 account to an ML-DSA-65 account and back, kill
# the node, restart it (the block log is re-verified), and check that balances
# survived. What the test suites, which drive the libraries, do not cover.
#
# Usage: scripts/smoke-devnet.sh [target-dir]   (default: target/debug)

set -euo pipefail
cd "$(dirname "$0")/.."

bin=${1:-target/debug}
node="$bin/stein-node"
wallet="$bin/stein"
port=${SMOKE_PORT:-18700}
rpc="http://127.0.0.1:$port"
dir=$(mktemp -d)
pid=""

cleanup() {
  [ -n "$pid" ] && kill "$pid" 2>/dev/null || true
  if [ "${status:-1}" -ne 0 ] && [ -f "$dir/node.log" ]; then
    echo "--- node log ---" >&2
    cat "$dir/node.log" >&2
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

balance() { # <address> → STEIN amount
  "$wallet" --rpc "$rpc" balance "$1" | awk '{print $1}'
}

expect() { # <label> <expected> <actual>
  if [ "$2" != "$3" ]; then
    echo "smoke: $1: expected $2, got $3" >&2
    exit 1
  fi
  echo "smoke: $1 = $3"
}

"$node" --data-dir "$dir/chain" init >/dev/null
start_node

bob=$("$wallet" keygen --out "$dir/bob.seed" 2>/dev/null)
carol=$("$wallet" keygen --scheme ml-dsa-65 --out "$dir/carol.seed" 2>/dev/null)

"$wallet" --rpc "$rpc" send --key "$dir/chain/faucet.seed" --to "$bob" --amount 100
"$wallet" --rpc "$rpc" send --key "$dir/bob.seed" --to "$carol" --amount 40
"$wallet" --rpc "$rpc" send --key "$dir/carol.seed" --to "$bob" --amount 15

# Fees are 10 base units per byte: bob paid one key-registering ML-DSA-44
# transfer (3,856 B), carol one key-registering ML-DSA-65 transfer (5,385 B).
expect "bob before restart" "74.99996144" "$(balance "$bob")"
expect "carol before restart" "24.99994615" "$(balance "$carol")"

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
expect "bob after restart" "74.99996144" "$(balance "$bob")"
expect "carol after restart" "24.99994615" "$(balance "$carol")"

echo "smoke: ok"
