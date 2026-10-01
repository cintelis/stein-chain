#!/usr/bin/env bash
# Deploys the Cloudflare follower against a running validator.
#
#   deploy/cloudflare/deploy.sh <leader-url>
#
# <leader-url> is the validator's public URL, e.g. the quick-tunnel address
# printed by `docker compose -f deploy/docker/compose.yml logs tunnel`. The
# genesis hash is read from the LOCAL validator (http://127.0.0.1:8645), not
# from the URL: the follower must pin a genesis it got out of band, never one
# the leader vouches for itself.
set -euo pipefail
cd "$(dirname "$0")"

leader=${1:?usage: deploy.sh <leader-url>}
local_rpc=${STEIN_LOCAL_RPC:-http://127.0.0.1:8645}
genesis=$(curl -fsS "$local_rpc/status" | tr ',{}' '\n\n\n' | grep '"genesis_hash":' | cut -d: -f2 | tr -d '"')
[[ "$genesis" =~ ^[0-9a-f]{64}$ ]] || { echo "could not read the genesis hash from $local_rpc" >&2; exit 1; }

# The tunnel must lead to the same validator.
remote=$(curl -fsS "$leader/status" | tr ',{}' '\n\n\n' | grep '"genesis_hash":' | cut -d: -f2 | tr -d '"')
[ "$remote" = "$genesis" ] || { echo "$leader is not the local validator (genesis $remote)" >&2; exit 1; }

echo "deploying follower: leader $leader, genesis $genesis"
npx wrangler deploy --var "STEIN_LEADER:$leader" --var "STEIN_GENESIS_HASH:$genesis"
