# steinscan

Block explorer for Stein Chain. Watch blocks and transactions arrive in real
time, and look up any block, transaction or address.

- **Live:** follows a node's `GET /events` stream (server-sent events). Blocks
  appear the moment the validator produces them or a follower has verified
  them. Transactions appear as *pending* when a node accepts them, then turn
  *confirmed*.
- **No backend, no build step, no third-party code:** three static files that
  read a node's public RPC from the browser.
- **Safe with untrusted nodes:** everything a node returns is shown as text
  (`textContent`), never parsed as HTML, under a strict Content-Security-Policy.

## Run locally

```sh
cd explorer/public && python -m http.server 8090
# open http://127.0.0.1:8090/?node=http://127.0.0.1:8645
```

## Deploy (Cloudflare)

```sh
cd explorer && npm install && npx wrangler deploy
```

Pick the network in the header, or pass `?node=<rpc-url>`. The choice is
remembered in the browser.

## Node API it uses

| Endpoint | For |
|---|---|
| `GET /events` | live `block` and `pending` events |
| `GET /status` | header stats, network banner |
| `GET /blocks?before=&limit=` | recent blocks, block pages |
| `GET /mempool` | pending transactions on load |
| `GET /tx/{id}` | transaction page (pending or included) |
| `GET /account/{address}`, `GET /account/{address}/txs` | address page |

## Next (SPEC §11.1)

This reads a node directly, so history is limited to what that node holds in
memory. For mainnet-scale history and full-text search, add an indexer that
follows `/events` into Postgres, with steinscan reading from it.
