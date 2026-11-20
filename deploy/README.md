# Two-node devnet: validator in Docker, follower in Cloudflare

```
 your machine                                   Cloudflare
 ┌──────────────────────────┐                  ┌───────────────────────────────┐
 │ stein-a (validator)      │◄── quick tunnel ─┤ Worker ─► Container           │
 │ 127.0.0.1:8645           │   (HTTPS, PQ     │          stein follower       │
 │ blocks in volume stein-a │    key exchange) │          (re-verifies blocks) │
 └──────────────────────────┘                  └───────────────────────────────┘
          ▲                                               ▲
          └─────── stein wallet ──────────────────────────┘
```

**Validator A** runs in local Docker, because a validator's block log must be
durable. **Follower B** runs in Cloudflare Containers. Its disk is not
durable, which is fine: on every start it pins the genesis hash, re-syncs from
A and re-verifies every block. B forwards transactions to A and applies A's
blocks only after checking every signature and the state root itself.

The follower-to-tunnel connection uses Stein's TLS policy (`stein-tls`): TLS
1.3, with X25519MLKEM768 key exchange, which Cloudflare's edge supports.

## Validator on a VM (current setup)

The devnet's validator runs on a Linux VM, not on a laptop, so the chain keeps
producing blocks when nobody's machine is on. The VM runs the same compose
file as below: validator A, the named tunnel that publishes it at
**https://rpc.steinchain.com**, and the traffic generator. The validator's
port stays on the VM's loopback; only the tunnel reaches it.

```sh
echo 'STEIN_VM=user@host'            >  deploy/vm/.env     # gitignored
echo 'STEIN_VM_KEY=~/.ssh/<key>'     >> deploy/vm/.env
deploy/vm/ship.sh                    # ship HEAD, rebuild, restart
deploy/vm/ship.sh --reset            # the same, and start a NEW chain
```

`ship.sh` sends a `git archive` of the commit to `~/stein-chain` on the VM,
builds the image there and restarts the services. `~/stein-chain/DEPLOYED_COMMIT`
records what runs. The validator's key and the faucet key are created on the
VM, in the `stein-a` volume, and never leave it.

Once per VM, copy the tunnel credentials (section 2) to
`~/.cloudflared/stein-devnet.json` and make them readable by the tunnel
container's user alone: `sudo chown 65532 ~/.cloudflared/stein-devnet.json &&
sudo chmod 600 ~/.cloudflared/stein-devnet.json`.

After `--reset` the chain has a new genesis hash (printed by `ship.sh`): set
the repository variable `STEIN_GENESIS_HASH` to it and re-run *Deploy
Cloudflare follower*.

- **Only one validator may run.** Two nodes signing with one key would fork
  the chain, and two with different keys are two chains behind one address.
  Do not start `stein-a` with the named tunnel anywhere else.
- **Deploys are by hand.** This repository is public, and a self-hosted
  runner on a public repository lets anyone who opens a pull request run code
  on the VM.
- **To use the faucet**, run the wallet inside the validator's container:
  `docker compose -f compose.yml exec stein-a stein send --key /data/faucet.seed --to <address> --amount 100`
  (in `~/stein-chain/deploy/docker` on the VM).

The sections below describe the same pieces on one machine, for a local devnet.

## 1. Validator A (local Docker)

```sh
docker compose -f deploy/docker/compose.yml up -d --build stein-a
curl -s http://127.0.0.1:8645/status          # note genesis_hash
docker compose -f deploy/docker/compose.yml cp stein-a:/data/faucet.seed ./faucet.seed
```

The first start creates a devnet in the `stein-a` volume:
- a validator key;
- a faucet key holding 1,000,000,000 STEIN;
- `genesis.json`.

Keep `faucet.seed` private.

## 2. Tunnel to A: named, stable (current setup)

Validator A is published at **https://rpc.steinchain.com** through the
Cloudflare Tunnel `stein-devnet`. Its configuration is committed in
`deploy/docker/cloudflared/config.yml`. Its credentials are not committed:
they live in `~/.cloudflared/stein-devnet.json` on the operator's machine, and
`deploy/docker/.env` (gitignored) points compose at them.

```sh
docker compose -f deploy/docker/compose.yml --profile named-tunnel up -d named-tunnel
curl -s https://rpc.steinchain.com/status
```

One-time setup on a new machine. In Git Bash, set `MSYS_NO_PATHCONV=1` first,
or the container paths get rewritten:

```sh
CF="docker run --rm -v $HOME/.cloudflared:/home/nonroot/.cloudflared cloudflare/cloudflared:2026.9.3"
$CF tunnel login        # opens a link: authorise steinchain.com
$CF tunnel create --credentials-file /home/nonroot/.cloudflared/stein-devnet.json stein-devnet
$CF tunnel route dns stein-devnet rpc.steinchain.com
echo "STEIN_TUNNEL_CREDENTIALS=$HOME/.cloudflared/stein-devnet.json" > deploy/docker/.env
```

The repository variables `STEIN_LEADER_URL` (https://rpc.steinchain.com) and
`STEIN_GENESIS_HASH` tell CI where the validator is and which chain to pin.
If the validator is re-initialised (a new genesis), update
`STEIN_GENESIS_HASH`, or the follower deploy will refuse to run.

## 2b. Quick tunnel (no account, temporary)

```sh
docker compose -f deploy/docker/compose.yml --profile tunnel up -d
docker compose -f deploy/docker/compose.yml logs tunnel | grep -o 'https://[a-z0-9-]*\.trycloudflare\.com'
```

A Cloudflare **quick tunnel** needs no account or DNS. Its URL changes every
time the tunnel restarts; if it does, redeploy the follower (step 3). For a
stable URL, use a named tunnel on your own domain instead.

> The validator's RPC has no authentication or rate limiting yet (SPEC §9).
> Anyone with the tunnel URL can read the chain and submit transactions
> (which still need valid signatures and fees). Fine for a devnet; stop the
> tunnel when you are not testing:
> `docker compose -f deploy/docker/compose.yml stop tunnel`.

## 3. Follower B (Cloudflare Containers)

**Automatic:** after CI passes on `main`, the *Deploy Cloudflare follower*
workflow deploys the tested commit, if node or deployment code changed since
the last deploy. It can also be run by hand from the Actions tab.

**From your machine:**

Requires the Workers Paid plan (Containers) and Docker running locally:
wrangler builds the image from `deploy/docker/Dockerfile`.

```sh
cd deploy/cloudflare
npm install
./deploy.sh https://<name>.trycloudflare.com
```

`deploy.sh` reads the genesis hash from the **local** validator, never from
the tunnel. It refuses to deploy if the tunnel leads elsewhere. It prints the
Worker URL, e.g. `https://stein-follower.<account>.workers.dev`.

The first request starts the container (cold start of a few seconds), and
then it syncs.

## 4. Node to node

```sh
B=https://stein-follower.<account>.workers.dev
stein keygen --out bob.seed                                  # prints bob's address
stein --rpc "$B" send --key faucet.seed --to <bob> --amount 25   # sent via B
stein --rpc http://127.0.0.1:8645 balance <bob>              # on A
stein --rpc "$B" balance <bob>                               # on B: same
curl -s "$B/status"                                          # role: follower, same state_root as A
```

If B's `/status` shows a `sync_error`, B received data from A that failed
verification and has stopped following. It never applies such data.

## Devnet traffic

The validator only produces a block when transactions are waiting, so an
unused devnet stands still and steinscan shows nothing new. `loadgen` keeps
it moving:

```sh
docker compose -f deploy/docker/compose.yml --profile loadgen up -d loadgen
docker compose -f deploy/docker/compose.yml logs -f loadgen
```

It reads the faucet key from A's volume, gives 20 throwaway accounts 1,000
STEIN each, and sends one transfer every 5 minutes between them (set
`STEIN_LOADGEN_TPS` to change the rate). Every start funds a fresh set of
accounts. Stop it with
`docker compose -f deploy/docker/compose.yml --profile loadgen stop loadgen`.

## Local-only alternative

Run both nodes locally, without Cloudflare:

```sh
STEIN_GENESIS_HASH=<hash from step 1> \
  docker compose -f deploy/docker/compose.yml --profile local-follower up -d
curl -s http://127.0.0.1:8646/status
```

## Tear down

```sh
docker compose -f deploy/docker/compose.yml --profile tunnel --profile local-follower down      # keeps the chain
docker compose -f deploy/docker/compose.yml --profile tunnel --profile local-follower down -v   # deletes it
cd deploy/cloudflare && npx wrangler delete
```
