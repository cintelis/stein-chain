# Stein Chain

A post-quantum layer-1 blockchain. Native token: **STEIN**.

- **ML-DSA** (FIPS 204) signatures via AWS-LC: **ML-DSA-44** by default
  (2,543-byte transfers), ML-DSA-65 accepted. Keys and signatures carry a scheme
  tag, so FN-DSA can be added when FIPS 206 is final.
- **FN-DSA test network** (opt-in, experimental): a separate chain where accounts
  may use FN-DSA-512 per the draft FIPS 206, giving 789-byte transfers. It needs
  an explicit build feature and `init --enable-fn-dsa`; see SPEC §3.5.
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
| `stein-crypto` | Scheme-tagged ML-DSA-44/65 keys and signatures, parallel batch verification, BLAKE3, Merkle trees, addresses, seed files |
| `stein-types` | Transactions, blocks, genesis, chain parameters, amounts |
| `stein-state` | Account state, key registry, signature and key caches, block building, full block verification and import |
| `stein-node` | Mempool, block producer, fsynced block log with re-verification on restart, JSON RPC |
| `stein-cli` | `stein` wallet: keygen, balance, send |

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
./target/release/stein keygen --out bob.seed            # ML-DSA-44; prints bob's address
./target/release/stein keygen --scheme ml-dsa-65 --out carol.seed
./target/release/stein send --key .stein-devnet/faucet.seed --to <bob-address> --amount 25.5
./target/release/stein balance <bob-address>
./target/release/stein status
```

FN-DSA test network (experimental, never mainnet):

```sh
cargo build --release --features stein-crypto/fn-dsa-experimental
./target/release/stein-node --data-dir .stein-fn init --enable-fn-dsa --scheme fn-dsa-512-draft
./target/release/stein-node --data-dir .stein-fn run --rpc 127.0.0.1:8655
./target/release/stein keygen --scheme fn-dsa-512-draft --out frank.seed
```

RPC: `GET /status`, `GET /account/{address}`, `GET /account/{address}/txs`,
`POST /tx {"raw": "<borsh hex>"}`, `GET /tx/{id}`, `GET /blocks`, `GET /mempool`,
`GET /block/{height}[?witnesses=true]`, and `GET /events`, a live stream of
blocks and transactions (server-sent events).

**steinscan**, the block explorer, watches it all in real time: see
[explorer/README.md](explorer/README.md).

## Tests and benchmarks

```sh
cargo test
cargo clippy --all-targets
cargo run --release -p stein-crypto --example sigbench     # ML-DSA-44/65 sign/verify rates
cargo run --release -p stein-state  --example throughput   # state-machine TPS, cold vs warm cache
```

CI (`.github/workflows/ci.yml`, adapted from Sigilant) runs on every push and
pull request:
- text-file, toolchain-pin and **crypto-policy** checks (`scripts/`): no
  classical or second ML-DSA implementation in the build graph;
- `fmt`, `clippy -D warnings` and all tests;
- an end-to-end devnet smoke test (`scripts/smoke-devnet.sh`) on Linux,
  Windows and macOS;
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

## Provenance

Key handling (`stein-crypto/src/keys.rs`, `seed_file.rs`) is adapted from
Sigilant's `sigilant-token` crate. The Windows build configuration and the
post-quantum TLS policy (to be used in phase 3) follow Sigilant as well.
