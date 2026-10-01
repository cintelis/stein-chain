#!/usr/bin/env bash
# Deploys the Cloudflare follower against a running validator.
#
#   deploy/cloudflare/deploy.sh <leader-url>
#
# <leader-url> is the validator's public URL, e.g. the quick-tunnel address
# printed by `docker compose -f deploy/docker/compose.yml logs tunnel`.
#
# The genesis hash the follower pins must come from out of band, never from
# the leader vouching for itself. It is taken from STEIN_GENESIS_HASH if set
# (the CI workflow passes the operator's input), otherwise read from the LOCAL
# validator at $STEIN_LOCAL_RPC (default http://127.0.0.1:8645). Either way,
# the deploy is refused unless the leader is on that same genesis.
set -euo pipefail
cd "$(dirname "$0")"

leader=${1:?usage: deploy.sh <leader-url>}
leader=${leader%/}

status_genesis() { # <rpc> -> genesis hash from its /status
  curl -fsS "$1/status" | tr ',{}' '\n\n\n' | grep '"genesis_hash":' | cut -d: -f2 | tr -d '"'
}

if [ -n "${STEIN_GENESIS_HASH:-}" ]; then
  genesis=$STEIN_GENESIS_HASH
else
  local_rpc=${STEIN_LOCAL_RPC:-http://127.0.0.1:8645}
  genesis=$(status_genesis "$local_rpc") || { echo "could not reach the local validator at $local_rpc" >&2; exit 1; }
fi
[[ "$genesis" =~ ^[0-9a-f]{64}$ ]] || { echo "genesis hash must be 64 hex characters, got '$genesis'" >&2; exit 1; }
[[ "$leader" =~ ^https:// ]] || { echo "the leader URL must be https://" >&2; exit 1; }

remote=$(status_genesis "$leader") || { echo "could not reach the leader at $leader" >&2; exit 1; }
[ "$remote" = "$genesis" ] || { echo "$leader is on genesis $remote, not the pinned $genesis" >&2; exit 1; }

echo "deploying follower: leader $leader, genesis $genesis"
npx wrangler deploy --var "STEIN_LEADER:$leader" --var "STEIN_GENESIS_HASH:$genesis"
