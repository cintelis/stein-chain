# Stein Chain

A post-quantum layer-1 blockchain. Native token: **STEIN**. Every signature on
chain is ML-DSA (FIPS 204) or FN-DSA (draft FIPS 206); every block is
re-verified by every node.

- **ML-DSA** (FIPS 204) signatures via AWS-LC: ML-DSA-44
  (2,543-byte transfers) and ML-DSA-65. Keys and signatures carry a scheme
  tag, so schemes can be added without a format change.
- **FN-DSA-512** (draft FIPS 206) is the default for new account keys:
  789-byte transfers. Adopted ahead of the final standard; validators stay on
  ML-DSA (ML-DSA-65 by default), and any account may choose ML-DSA instead.
  See SPEC §3.5.
- **BLAKE3** hashing with a domain-separation context for every use.
- Account model with **one-time public-key registration** (−34% bytes per transfer).
- **Signature verification cache**: each signature is verified once per node,
  at mempool admission, not again at block import.
- Witness/body split: every validator verifies every signature *before* voting;
  signatures are pruned only after finality.
- BFT proof of stake (planned), QUIC with post-quantum TLS 1.3 (planned).

Protocol design, security rules, measured performance and hardware: **[docs/SPEC.md](docs/SPEC.md)**.
Constraints that keep the protocol shardable later: **[docs/SHARD-READY.md](docs/SHARD-READY.md)**.
Smart contract design (WebAssembly): **[docs/CONTRACTS.md](docs/CONTRACTS.md)**.

## Status

Phase 1.1 is complete: a **single-validator devnet**. Peer-to-peer networking and
consensus are not built yet (SPEC §11). Do not use this for real value.

| Crate | What it does |
|---|---|
| `stein-crypto` | Scheme-tagged ML-DSA-44/65 and FN-DSA-512 keys and signatures, parallel batch verification, BLAKE3, Merkle trees, addresses, seed files |
| `stein-types` | Transactions, blocks, genesis, chain parameters, amounts, tokens, pool pricing |
| `stein-state` | Account state, tokens and liquidity pools, key registry, signature and key caches, block building, full block verification and import |
| `stein-node` | Mempool, block producer, fsynced block log with re-verification on restart, JSON RPC |
| `stein-cli` | `stein` wallet: keygen, balance, send, tokens, pools |

## Quick start

Requires Rust 1.98.1 (pinned in `rust-toolchain.toml`) and a C compiler. On
Windows, use the MSVC toolchain. `.cargo/config.toml` uses AWS-LC's prebuilt NASM
objects, so no NASM install is needed. Linux and macOS build everything from source.

```sh
cargo build --release

# Create a devnet: validator key, faucet key (1e9 STEIN), genesis.json
./target/release/stein-node init

# Run it (RPC on 127.0.0.1:8645, 1 s blocks)
./target/release/stein-node run

# In another terminal
./target/release/stein keygen --out bob.seed            # FN-DSA-512; prints bob's address
./target/release/stein keygen --scheme ml-dsa-65 --out carol.seed
./target/release/stein send --key .stein-devnet/faucet.seed --to <bob-address> --amount 25.5
./target/release/stein balance <bob-address>
./target/release/stein status
```

New keys are FN-DSA-512 (draft FIPS 206, SPEC §3.5). For an ML-DSA account:

```sh
./target/release/stein keygen --scheme ml-dsa-65 --out carol.seed
```

### Tokens and pools

Anyone can issue a token with a fixed supply, and fund a pool that trades it
(SPEC §4.6, §4.7). An asset is `stein` or a token's address.

```sh
# Issue 1,000,000 GOLD. Prints the token's address.
./target/release/stein token issue --key bob.seed --symbol GOLD --supply 1000000

# Fund the STEIN/GOLD pool. The first deposit sets the price: 4,000 GOLD per STEIN.
./target/release/stein pool add --key bob.seed --a stein --amount-a 100 --b <gold> --amount-b 400000

# Trade through it, send the token, and take the liquidity back out.
./target/release/stein pool swap --key carol.seed --sell stein --amount 10 --buy <gold>
./target/release/stein token send --key carol.seed --token <gold> --to <bob-address> --amount 500
./target/release/stein pool remove --key bob.seed --pool <pool-address> --shares all

./target/release/stein token list
./target/release/stein pool list
```

A swap names the least it will accept (the quote less `--slippage-bps`, 0.5%
by default). If the price has moved past that when the block is built, the
pool returns the payment: the transaction is still in the block and its fee
is spent.

RPC: `GET /status`, `GET /account/{address}`, `GET /account/{address}/txs`,
`POST /tx {"raw": "<borsh hex>"}`, `GET /tx/{id}`, `GET /blocks`, `GET /mempool`,
`GET /block/{height}[?witnesses=true]`, `GET /tokens`, `GET /token/{address}`,
`GET /pools`, `GET /pool/{address}`, and `GET /events`, a live stream of
blocks and transactions (server-sent events). A node holds each client to a
rate and answers `429` with `Retry-After` beyond it; requests from the
node's own machine are not limited (see [deploy/README.md](deploy/README.md)).

**steinscan**, the block explorer, watches it all in real time: see
[explorer/README.md](explorer/README.md).

Two more parts of the project have repositories of their own:
[stein-wallet](https://github.com/steinchain/stein-wallet), the browser wallet
at https://wallet.steinchain.com, and
[stein-site](https://github.com/steinchain/stein-site), the home page at
https://steinchain.com.

## Tests and benchmarks

```sh
cargo test
cargo clippy --all-targets
PROPTEST_CASES=2000 cargo test -p stein-state prop_tests   # more random histories than the default 48
scripts/fuzz.sh 600                                        # every fuzz target for 10 minutes (Linux, macOS)
cargo run --release -p stein-crypto --example sigbench     # ML-DSA-44/65 sign/verify rates
cargo run --release -p stein-state  --example throughput   # state-machine TPS, cold vs warm cache
```

Property tests (`crates/stein-state/src/prop_tests.rs`,
`crates/stein-types/tests/decoders.rs`) run with `cargo test`. The fuzz
targets in `fuzz/` need a nightly compiler and cargo-fuzz, and are not part
of the workspace: see `scripts/fuzz.sh`.

CI (`.github/workflows/ci.yml`, adapted from Sigilant) runs on every push and
pull request:
- text-file, toolchain-pin and **crypto-policy** checks (`scripts/`): no
  classical or second ML-DSA implementation in the build graph;
- `fmt`, `clippy -D warnings` and all tests;
- an end-to-end devnet smoke test (`scripts/smoke-devnet.sh`) on Linux,
  Windows and macOS;
- every fuzz target for a minute;
- RustSec advisories.

Benchmarks run on demand.

Key security tests (in `crates/stein-state/src/tests.rs`):
- `followers_reject_a_block_with_a_forged_transaction`: a malicious leader
  signs a block containing a forged transfer, and every follower rejects it.
- `followers_reject_blocks_without_witnesses_or_with_wrong_roots`
- `a_warm_signature_cache_never_vouches_for_different_bytes`
- `accounts_on_different_schemes_share_a_block`
- `a_rejected_block_restores_new_accounts_and_keys`
- `supply_is_conserved`
- `any_history_conserves_everything_and_every_node_agrees` (in
  `prop_tests.rs`): the same properties over random histories.

## Provenance

Key handling (`stein-crypto/src/keys.rs`, `seed_file.rs`) is adapted from
Sigilant's `sigilant-token` crate. The Windows build configuration and the
post-quantum TLS policy (to be used in phase 3) follow Sigilant as well.
