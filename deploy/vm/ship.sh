#!/usr/bin/env bash
# Ships the committed tree to the validator's VM and (re)starts the devnet
# there: validator A, its named tunnel (https://rpc.steinchain.com) and the
# traffic generator. See deploy/README.md, "Validator on a VM".
#
#   deploy/vm/ship.sh            ship HEAD, rebuild, restart
#   deploy/vm/ship.sh --reset    the same, but first DELETE the chain on the
#                                VM: the validator then creates a new genesis
#
# Where the VM is comes from the environment or from deploy/vm/.env
# (gitignored), never from the repository:
#
#   STEIN_VM=user@host           required
#   STEIN_VM_KEY=~/.ssh/key      optional SSH identity file
#
# The VM needs Docker, and once: the tunnel credentials at
# ~/.cloudflared/stein-devnet.json (see the README). Nothing else is copied.
# The validator's key never leaves the VM.
#
# A public repository must not deploy through a self-hosted runner (anyone
# who opens a pull request could run code on the VM), so this runs by hand.

set -euo pipefail
cd "$(dirname "$0")/../.."

# shellcheck disable=SC1091
[ -f deploy/vm/.env ] && . deploy/vm/.env
: "${STEIN_VM:?set STEIN_VM=user@host (in the environment or deploy/vm/.env)}"

reset=0
case "${1:-}" in
  "") ;;
  --reset) reset=1 ;;
  *) echo "usage: deploy/vm/ship.sh [--reset]" >&2; exit 64 ;;
esac

if ! git diff --quiet HEAD; then
  echo "ship: there are uncommitted changes; only a commit is shipped" >&2
  exit 1
fi
sha=$(git rev-parse HEAD)

remote() {
  ssh ${STEIN_VM_KEY:+-i "$STEIN_VM_KEY"} -o BatchMode=yes "$STEIN_VM" "$@"
}

echo "ship: $sha -> $STEIN_VM"
remote 'rm -rf "$HOME/stein-chain-new" && mkdir "$HOME/stein-chain-new"'
# No line-ending conversion: the tree must arrive as it is committed.
git -c core.autocrlf=false archive --format=tar "$sha" | remote 'tar -x -C "$HOME/stein-chain-new"'

remote SHA="$sha" RESET="$reset" bash -s <<'REMOTE'
set -euo pipefail
new=$HOME/stein-chain-new
cur=$HOME/stein-chain
credentials=$HOME/.cloudflared/stein-devnet.json
compose="docker compose -f compose.yml --profile named-tunnel --profile loadgen"

[ -f "$credentials" ] || { echo "ship: $credentials is missing on the VM (see deploy/README.md)" >&2; exit 1; }
echo "$SHA" > "$new/DEPLOYED_COMMIT"
echo "STEIN_TUNNEL_CREDENTIALS=$credentials" > "$new/deploy/docker/.env"

rm -rf "$HOME/stein-chain-old"
[ -d "$cur" ] && mv "$cur" "$HOME/stein-chain-old"
mv "$new" "$cur"
cd "$cur/deploy/docker"

docker compose -f compose.yml build stein-a
if [ "$RESET" = 1 ]; then
  echo "ship: deleting the chain on the VM"
  $compose down --volumes
fi
$compose up -d

for _ in $(seq 60); do
  if status=$(curl -fsS http://127.0.0.1:8645/status 2>/dev/null); then
    echo "ship: validator is up on $SHA"
    echo "$status"
    exit 0
  fi
  sleep 2
done
echo "ship: the validator did not answer within two minutes" >&2
docker compose -f compose.yml logs --tail 40 stein-a >&2
exit 1
REMOTE
