# Stein Chain — Protocol Specification

*Version 0.3 (draft), 21 November 2026. Native token: **STEIN**.*

Changes in 0.3:
- tokens anyone can issue (§4.6) and liquidity pools that trade them (§4.7),
  as native transaction types
- the state root commits to tokens, holdings and pools (§5.4)

Changes in 0.2:
- algorithm-agile keys and signatures (§3.4)
- ML-DSA-44 as the devnet default
- signature verification cache (§5.3)
- 6-hour witness retention and archive offload (§5.5)
- compact block propagation (§7)
- measured size-reduction options (§2.2)
- revised hardware with role-based horizontal scaling (§10)

Status markers: **[built]** = implemented and tested in this repository;
**[planned]** = specified here, not yet implemented; **[decision]** = open
question for the project owner.

---

## 1. Goals

1. **Post-quantum from genesis.** No signature or key exchange that a
   cryptographically relevant quantum computer breaks: no ECDSA, EdDSA, BLS or
   classical-only Diffie–Hellman anywhere in the protocol.
2. **Security before throughput.** Every validator verifies every signature
   before voting. Nothing a block proposer claims is taken on trust.
3. **High throughput within the bandwidth budget.** Post-quantum signatures are
   ~40× larger than Ed25519 signatures. The design minimises bytes per
   transaction, and the number of times each byte is sent and stored, and is
   honest about what that costs.
4. **Modest validator hardware.** Scale by splitting roles across machines,
   not by requiring one large machine (§10.3).
5. **Fast deterministic finality.** BFT proof of stake: a finalized block is
   never reverted.

Non-goals for v1: smart contracts (§12), privacy features, cross-chain bridges.
Tokens and liquidity pools are built in as transaction types (§4.6, §4.7);
they do not wait for contracts.

---

## 2. Measured baseline

Measured on the development machine (22 hardware threads, Windows 11, AWS-LC
1.18 / aws-lc-sys 0.45, release build). A laptop's boost clocks make results
vary by about ±25% between runs, so ranges are given. Reproduce with:

```
cargo run --release -p stein-crypto --example sigbench
cargo run --release -p stein-state  --example throughput
```

### 2.1 Signatures and state machine

| Measurement | ML-DSA-44 (default) | ML-DSA-65 |
|---|---|---|
| Sign, 1 core | 2,300–2,800 /s | 1,600–2,000 /s |
| Verify, 1 core | 8,800–11,500 /s | 5,600–7,700 /s |
| Verify, all cores | 78,000–106,000 /s | 54,000–72,000 /s |
| Transfer size, first (registers key) | 3,856 B | 5,385 B |
| **Transfer size, thereafter** | **2,543 B** | 3,432 B |
| Leader block build (cold cache) | ~31,000 tx/s | ~21,000 tx/s |
| Follower import, cold cache | ~30,500 tx/s | ~21,000 tx/s |
| Mempool admission (signature check) | ~54,000 tx/s | ~33,000 tx/s |
| **Follower import, warm cache** (block critical path) | **~69,000 tx/s** | ~62,000 tx/s |

These figures **exclude networking, consensus rounds and disk**. They are the
ceiling of one node's CPU, not network throughput (see §10).

### 2.2 Ways to shrink a transaction (measured)

| Option | Steady transfer size | Result |
|---|---|---|
| ML-DSA-65 | 3,432 B | baseline in v0.1 |
| **ML-DSA-44** | **2,543 B (−26%)** | **adopted** in v0.2; the default until FN-DSA-512 (below) |
| zstd level 19 on whole transactions | −2.7% | **rejected**: signatures are random and compress to 99.9% |
| FN-DSA-512 (Falcon), draft FIPS 206, `fn-dsa` 0.4.0 | **789 B (−69% vs ML-DSA-44)**; verify ~66,000 /s per core; block import ~75,600 tx/s cold, ~111,300 warm | **adopted for accounts, opt-in per account** (§3.5), ahead of the final FIPS 206. **Default for new account keys.** |
| STARK aggregation of all signatures in a block | ~120 B + amortised proof | research (§12) |

---

## 3. Cryptography [built]

| Use | Primitive | Notes |
|---|---|---|
| Transaction signatures | **FN-DSA-512** (draft FIPS 206, NIST cat. 1) by default for new account keys (§3.5); ML-DSA-44 (FIPS 204, cat. 2) and ML-DSA-65 (cat. 3) also accepted | 44: pk 1,312 B, sig 2,420 B. 65: pk 1,952 B, sig 3,309 B. FN-DSA-512: pk 897 B, sig 666 B |
| Validator proposals and votes | **ML-DSA-65** (FIPS 204, NIST cat. 3) by default; ML-DSA-44 also accepted. Never a draft scheme | [decision: require ML-DSA-65 for mainnet validators] |
| Hashing | **BLAKE3**, derive-key mode | Every use has its own context string |
| P2P key exchange | **X25519MLKEM768** hybrid, or pure ML-KEM-768/1024 | via rustls on AWS-LC [planned] |
| Implementation | **AWS-LC** (`aws-lc-rs`) for ML-DSA and TLS; `fn-dsa` 0.4.0 for FN-DSA | AWS-LC: FIPS 140-3 validation track; same stack as Sigilant. `fn-dsa`: outside any validation (§3.5) |

### 3.1 Domain separation
- **Signatures:** the signed message is `"STEIN-SIG-v1" ‖ domain ‖ payload`.
  `domain` is 1 = transaction, 2 = proposal, 3 = vote. A signature made for one
  purpose cannot verify as another. (AWS-LC exposes pure ML-DSA with the empty
  FIPS 204 context string, so separation is done in the message.)
- **Hashes:** BLAKE3 derive-key mode with a context per use
  (`"stein address v2"`, `"stein txid v1"`, `"stein merkle leaf v1"`, …).
  There is no context-free hash function in the codebase.

### 3.2 Signing
- Signing is **hedged** (FIPS 204 default): fresh randomness is mixed into
  every signature. This resists fault and side-channel attacks. As a result,
  the same body has many valid signatures, so **no on-chain identifier may
  cover a signature** (§4.2).
- **FN-DSA (Falcon) is accepted for user accounts**, ahead of the final
  FIPS 206 (§3.5). It gives about 3× smaller transactions than ML-DSA-44 and
  6–7× faster verification (§2.2). Its signing uses Gaussian sampling, which is
  hard to make constant-time, so **validators stay on ML-DSA** and any
  account may choose ML-DSA instead.

### 3.3 Why BLAKE3 and not SHA-256
Grover's algorithm affects all 256-bit hashes equally (~128-bit preimage
security, which is adequate). BLAKE3 is chosen for **speed** (SIMD, tree-parallel),
not because SHA-256 is quantum-weak.

### 3.4 Algorithm agility [built]
Every public key and signature carries a one-byte **scheme tag**:

| Tag | Scheme | Status |
|---|---|---|
| 1 | ML-DSA-44 | accepted |
| 2 | ML-DSA-65 | accepted; default for validator keys |
| 3 | FN-DSA-512, draft FIPS 206 (`fn-dsa-512-draft`) | accepted for accounts where genesis lists it (the default), and the default for new account keys; never for validators (§3.5). Means this draft for good |
| 4–255 | — | unassigned. The final FIPS 206 will take a new tag |

- Wire form: `tag ‖ bytes`, with the byte length fixed by the tag. There is no
  length prefix, so each value has exactly one encoding.
- Which schemes are accepted is a **chain parameter**: `tx_schemes` for
  accounts and `validator_schemes` for validators, both in genesis. Adding
  a scheme later (the final FIPS 206) is a parameter change and a software
  upgrade, not a format change.
- A key only verifies signatures of its own scheme. The address commits to the
  tag (§4.1), so one key can never control accounts under two schemes.
- Accounts on different schemes interoperate freely (test:
  `accounts_on_different_schemes_share_a_block`).
- **Why ML-DSA-44 is acceptable:** category 2 is at least as hard to break as
  finding a SHA-256 collision, classically or quantumly. NIST approves it for
  general use. Category 3 (ML-DSA-65) remains available for accounts holding
  large balances.

### 3.5 FN-DSA-512 accounts, ahead of the final standard [built]
Accounts may use **FN-DSA-512 per the draft FIPS 206** (scheme tag 3,
`fn-dsa-512-draft`). The decision: the gain is worth taking now and should not
wait for a publication date nobody controls. Transfers are 789 bytes instead of
2,543 (1,687 with key registration), verification is ~6.5× faster than
ML-DSA-44, and block import about 2× faster (§2.2).

**It is the default for new account keys:** `stein keygen` creates an FN-DSA
key, and `stein-node init` creates an FN-DSA faucet (and an ML-DSA-65
validator). An account that prefers a
final standard passes `--scheme ml-dsa-44` or `--scheme ml-dsa-65`. Accounts on
different schemes transfer freely.

**Which networks accept it:** those whose genesis lists tag 3 in `tx_schemes`.
`stein-node init` lists it by default; `init --ml-dsa-only` leaves it out. A
network that started without it (a genesis written before this change) does not
accept it: `tx_schemes` is fixed at genesis.

**What adopting a draft costs, and the rule that contains each cost:**

| Cost | Rule |
|---|---|
| FIPS 206 is unpublished and may still change. The `fn-dsa` crate warns that later versions may reject keys and signatures made now. | **Tag 3 means this draft for good.** `fn-dsa` is pinned to exactly 0.4.0 and is never upgraded in place, so the chain can always replay its own history. The final standard gets a **new tag** beside this one. |
| The final standard may differ, leaving tag 3 accounts on a scheme no other software speaks. | Tag 3 accounts keep working. To move, an account transfers its balance to an account under another scheme; `RotateKey` (§4.3, planned) will make that in-place. |
| `fn-dsa` is a second signature implementation, outside any FIPS validation, and its Gaussian sampling is hard to make constant-time. | **Validators never use FN-DSA.** Genesis validation rejects a draft scheme in `validator_schemes` (`DraftValidatorScheme`). A flaw in FN-DSA could cost the accounts that chose it, never consensus. |
| Two draft implementations could disagree about which signatures verify: a consensus split. | **One implementation.** `scripts/check-crypto-policy.sh` requires `fn-dsa` at exactly 0.4.0, used by `stein-crypto` alone, and refuses any other FN-DSA or Falcon crate. |
| FN-DSA-512 is NIST category 1, below ML-DSA-44 (category 2). | Each account chooses its scheme. Accounts holding large balances should prefer ML-DSA-65. |

Also:
- The name **`-draft`** appears in key files, JSON and the CLI, so no one
  mistakes these keys for final-standard FN-DSA.
- **Keys from seeds:** `fn-dsa` has no seed-to-key function, so a key is
  generated deterministically from the 32-byte seed through a BLAKE3 XOF.
  These seed files are Stein-specific, not portable to other FN-DSA wallets.

Tests: `fn_dsa_draft_keys_sign_and_verify`, `fn_dsa_and_ml_dsa_never_cross`,
`fn_dsa_accounts_work_where_genesis_lists_the_scheme`,
`scripts/smoke-fn-dsa.sh` (in CI on Linux, Windows and macOS).

---

## 4. Accounts and transactions [built]

### 4.1 Addresses
`address = BLAKE3-derive("stein address v2", scheme_tag ‖ public_key)`: 32 bytes.

Text form: `st` + 64 hex characters + 8 hex characters of checksum
(`BLAKE3-derive("stein address checksum v1", address)[0..4]`). The checksum
catches typos before a transfer is signed and never appears on chain.
[decision: switch to Bech32m before mainnet for better error detection]

### 4.2 Transaction format
```
TxBody      { chain_id: u64, sender: Address, nonce: u64, fee: u128,
              expires_at: u64, action: Action }                         121 B
Witness     { public_key: Option<PublicKey>, signature: Signature }
              ML-DSA-44: 2,422 B, or 3,735 B with key (each item = tag + bytes)
Transaction { body: TxBody, witness: Witness }
```
- **Encoding:** Borsh. It has exactly one encoding per value, which is required
  for anything hashed or signed. JSON is used only at the RPC edge, where keys
  and signatures appear as `"ml-dsa-44:<hex>"`.
- **Transaction id** = `BLAKE3-derive("stein txid v1", body)`. It covers the
  body only, so it is immune to witness malleability.
- **`chain_id`** is in every body, so transactions cannot be replayed across
  networks.
- **`nonce`** must equal the account's next nonce, so transactions cannot be
  replayed within a network.
- **`expires_at`** bounds how long a signed transaction stays valid. It may be
  at most `max_tx_ttl_blocks` ahead of the current height.

### 4.3 Key registration (bandwidth saving)
An account's **first** transaction carries its public key. The chain checks
`address(public_key) == sender` and stores the key. Later transactions must
**not** carry it; doing so is rejected (`KeyAlreadyRegistered`). This saves
1,313 bytes (34%) per transaction after the first with ML-DSA-44, and 1,953
bytes (36%) with ML-DSA-65. Validators keep parsed keys of recently active
accounts in a bounded cache (200,000 keys), so memory does not grow with the
total number of accounts.

Related decisions:
- The mempool accepts follow-up transactions that verify against a key whose
  registering transaction is queued but not yet in a block, so wallets can send
  several transactions in a row.
- Key rotation is [planned]: a `RotateKey` action signed by the old key,
  possibly to a different scheme (e.g. ML-DSA-44 → FN-DSA-512).

### 4.4 Fees
`fee ≥ min_fee_per_byte × encoded_len`. Bytes, not CPU, are the scarce
resource, so fees are charged per byte, and smaller schemes are cheaper to use.
Devnet: 10 base units/byte (a 2.5 KB ML-DSA-44 transfer ≈ 0.000025 STEIN).
**Fees go to the fee pool**, a system account, and are paid out to validators by
voting power every `epoch_blocks` (devnet: 3,600). No transaction credits the
proposer directly, which keeps transactions shard-local (SHARD-READY I-6). [decision: mainnet fee market.
Recommended: an EIP-1559-style base fee that is burned, plus a priority tip to
the proposer]

### 4.5 Units
1 STEIN = 10⁹ base units. Amounts are `u128`. All arithmetic is checked.
Overflow rejects the transaction; it never wraps.

### 4.6 Tokens [built]
Anyone can issue a token. A token is a transaction type, not a contract: it
costs a fee per byte like a transfer, and runs no code.

```
IssueToken    { symbol, decimals, supply }
TransferToken { token, to, amount }
```

- **Address.** A token lives at
  `BLAKE3-derive("stein token address v1", issuer ‖ nonce)`, where `nonce` is
  the nonce of the transaction that issues it. The address is known before
  the token exists, is unique because each nonce is spent once, needs no
  global counter (SHARD-READY I-3), and is controlled by no key.
- **Fixed supply.** `IssueToken` credits the whole `supply` to the issuer.
  There is no mint, burn, freeze or owner afterwards: nobody, the issuer
  included, can change a token once it exists.
- **Supply below 2¹²⁷ base units**, like STEIN's, so a token credit can never
  overflow (SHARD-READY I-1).
- **Symbol:** 1 to 12 capital letters and digits, starting with a letter, and
  not `STEIN`. **Symbols are not unique.** Two tokens may both call
  themselves `USD`; only the address identifies a token, and wallets and
  steinscan show it.
- **Decimals:** 0 to 18, for display only.
- **Holdings** are kept per holder: `(holder, token) → amount`, under the
  holder's address (SHARD-READY I-4). A holder of none has no entry, and an
  account can hold tokens without ever having held STEIN.
- `TransferToken` debits the sender and credits the recipient. It is valid
  exactly when the sender holds the amount and the fee.
- The mempool admits a transaction only if its sender already holds what it
  sends (§9.7). A token must therefore be in a block before it can be sent or
  deposited: a wallet waits for the issue before the next step.

Tests: `issuing_a_token_credits_its_whole_supply_to_the_issuer`,
`tokens_move_between_accounts_and_cannot_be_overspent`,
`malformed_asset_actions_are_refused_before_anything_else`.

### 4.7 Liquidity pools [built]
A pool holds two assets and trades one for the other at the ratio of its
reserves (a constant-product market maker, as in Uniswap v2). An *asset* is
STEIN or a token. Every pair of assets has exactly one pool, at
`BLAKE3-derive("stein pool address v1", asset_a ‖ asset_b)` with the pair in
canonical order: STEIN first, then tokens by address. No key controls a pool.

```
AddLiquidity    { asset_a, amount_a, asset_b, amount_b, min_shares }
RemoveLiquidity { pool, shares, min_a, min_b }
Swap            { asset_in, amount_in, asset_out, min_out }
```

**Reserves are balances.** A pool's reserves are the balances of its own
address, so there is no second ledger to keep in step. Anything sent straight
to a pool's address joins its reserves; a credit cannot be refused (I-1).

**Shares.** A pool's shares are held like a token, under the pool's address,
and can be sent with `TransferToken`.
- The first deposit creates the pool and sets its price. It mints
  `⌊√(amount_a × amount_b)⌋` shares, of which **1,000 are locked for good**,
  held by the pool itself. A pool can therefore never be emptied back to zero
  shares, and inflating one share's value to round a later depositor down to
  nothing is made costly.
- Later deposits are taken **at the pool's ratio**. The scarcer side sets
  the shares (rounded down); each amount taken is rounded up, never above
  what was offered; the rest is returned.
- Shares redeem for the same fraction of each reserve, rounded down.

**Swaps.** `out = ⌊net × reserve_out ÷ (reserve_in + net)⌋`, where
`net = amount_in − ⌈amount_in × 3 ÷ 1000⌉`. The whole payment joins the
reserves, so the **0.3% fee stays in the pool** and each share is worth more.
The fee is a protocol constant. Every rounding favours the pool: the product
of the reserves never falls in a swap.

**Arithmetic.** Products of two 128-bit amounts are carried in 256 bits and
divided back down (`stein-types` `math`): exact, with no floating point. The
module is tested against vectors from arbitrary-precision arithmetic.

**Two halves, and refunds.** A pool operation has a sender's half and a
pool's half (SHARD-READY I-2):
1. *Sender's half.* The nonce is checked, and the fee and everything the
   transaction sends are debited. **This alone decides whether the
   transaction is valid.**
2. *Pool's half.* The pool computes the result. If it cannot carry the
   operation out (no pool for the pair, less than `min_out` / `min_shares` /
   `min_a` / `min_b`, an amount too small to give anything back), **it
   returns what was sent**, as credits.

A swap the pool cannot fill is therefore still a valid transaction: it is in
the block, its nonce is spent and its fee is paid. What a pool does is never
a condition of validity, so a transaction can be judged on its sender's state
alone, and trying a trade against a moving price is not free. Today both
halves run back to back in the same block; with sharding the pool's half
becomes a receipt on the pool's shard (SHARD-READY §3.1).

**Outcomes.** What a pool operation did (what it paid, or why it returned
the payment) is not in the block: every node derives it by executing the
block, and serves it with the transaction (`outcome` in `GET /tx/{id}`). It
is committed through the state root. [planned: a receipts root in the block
header, so a light client can be shown an outcome with a proof.]

**What is not here:** no price oracle, no pool for more than two assets, no
concentrated liquidity, and no multi-pool route in one transaction. Trading
A for C through B is two transactions. As on any such market, the order of
transactions in a block decides who gets which price; `min_out` is the
trader's protection.

Tests: `the_first_deposit_creates_the_pool_and_locks_the_minimum`,
`a_swap_trades_at_the_pool_price_and_the_fee_stays_in_the_pool`,
`a_swap_the_pool_cannot_fill_is_still_valid_and_returns_what_was_paid`,
`later_deposits_take_the_pool_ratio_and_return_the_rest`,
`shares_redeem_for_their_part_of_both_reserves`,
`everything_is_conserved_across_a_long_mixed_history`, and
`scripts/smoke-devnet.sh`.

### 4.8 How these reached a running network
There is no activation height yet (§12). The new transaction types were added
so that a chain's earlier history is untouched:
- `Transfer` keeps its encoding; the new actions are new variants after it.
- The state root is unchanged while no token or pool exists (§5.4).

A node with this software replays any earlier history to the same roots
(test: `state_roots_from_before_tokens_are_unchanged`). A node without it
stops at the first block that carries a new action, because it cannot decode
it. **Every node must therefore be upgraded before the first such transaction
is sent.** That is acceptable on a devnet run by one operator; a network with
independent validators needs the activation rule first.

---

## 5. Blocks, execution and storage

### 5.1 Block format [built]
```
BlockHeader { chain_id, height, parent, timestamp_ms, proposer,
              tx_count, tx_root, witness_root, state_root }
Block       { header, proposer_signature, bodies[], witnesses[]? }
```
The header commits to bodies (`tx_root`) and witnesses (`witness_root`)
**separately**, using RFC 6962-style Merkle trees with leaf/node domain
separation and no leaf duplication (this avoids CVE-2012-2459-style
ambiguity).

### 5.2 The witness rule — the critical security property [built]
**A validator never votes on, or applies, a block whose witnesses it has not
received and verified.** `import_block` rejects a block that arrives without
witnesses (`MissingWitnesses`). It also verifies every transaction signature
(from its cache or afresh), and rejects the whole block if one fails.

The original research notes proposed stripping signatures before consensus and
gossiping them later on a slower channel. That would let a malicious leader
include forged transfers that nobody checks before finality. This
specification **forbids** that design. Signatures may be *pruned* only after
finality (§5.5).

Tests: `followers_reject_a_block_with_a_forged_transaction` and
`followers_reject_blocks_without_witnesses_or_with_wrong_roots`.

### 5.3 Execution [built: sequential; planned: parallel]
1. Stateless checks (chain id, expiry, fee, scheme accepted, key/address
   binding): sequential and cheap.
2. Signature verification: **parallel across all cores** (rayon), through the
   verification cache.
3. Execution: sequential, all-or-nothing per transaction for the proposer and
   all-or-nothing per block for followers. A rejected block leaves no trace
   (undo log).

**Verification cache [built].** A signature verified at mempool admission is
not verified again when the block is built or imported. Without the cache,
every transaction is verified twice per validator, so the cache halves
signature CPU. It also takes verification off the block's critical path:
import is measured at ~69,000 tx/s warm vs ~30,500 tx/s cold (§2.1). Rules:
- A cache entry is `BLAKE3(key fingerprint ‖ tx id ‖ witness hash)`, so it
  vouches only for the exact bytes verified, under the exact key.
- Entries are created only by a successful verification on this node, so a
  peer cannot inject one.
- The cache is bounded (two generations, 200,000 entries). A miss falls back
  to full verification, and the cache never changes what is valid.

Tests: `a_warm_signature_cache_never_vouches_for_different_bytes` and
`signatures_checked_at_admission_are_not_checked_again`.

[planned] Parallel execution of transfers with disjoint account sets
(Sealevel/Block-STM style). Execution is not the bottleneck today.

### 5.4 State commitment
- [built] A Merkle root over all accounts in address order, recomputed per
  block. This is O(n) and fine for the devnet.
- [built] From the first token on, the root is
  `BLAKE3-derive("stein state root v2", accounts ‖ tokens ‖ holdings ‖ pools)`:
  the roots of four such trees. Until then it is the accounts root alone, as
  it has been since genesis (§4.8).
- [planned] A Jellyfish Merkle Tree (as in Aptos/Diem) updated incrementally,
  O(k log n) per block, with proofs for light clients.

### 5.5 Storage, witness retention and archive offload
- [built] An append-only, fsynced block log. On restart the node **re-verifies
  every block** from genesis; a tampered log is refused. In memory, witnesses
  are pruned after `witness_retention_blocks`. A follower syncing older blocks
  is served them from the log, witnesses included, so a new node can still
  verify the whole history.
- [planned] RocksDB with separate column families for headers, bodies,
  witnesses and state. Account state moves to disk, with only hot accounts in
  RAM.
- **Pruning rule:** once a block is finalized and older than
  `witness_retention_blocks` (**21,600 blocks ≈ 6 hours**), a non-archive node
  may delete its witnesses. `tx_root` still proves which bodies executed.
- [planned] **Archive offload:** archive nodes stream finalized blocks,
  witnesses included, to object storage (Cloudflare R2 or S3) in immutable
  per-epoch bundles addressed by block hash. History stops needing local NVMe
  and anyone can re-verify it. R2 has no egress fees, which suits public
  history downloads.
- **Consequence (weak subjectivity):** a new node cannot re-verify signatures
  older than the retention window from peers. It syncs from a recent finalized
  checkpoint, signed by more than ⅔ of stake, plus state sync, and can audit
  history from the archive bundles. This is normal for proof of stake and must
  be explained to users.

---

## 6. Consensus [planned]

**BFT proof of stake, Tendermint-family**, with ML-DSA validator keys.

- **Engine:** Malachite (Rust; generic over the signing scheme). Confirm
  maintenance status before adopting. Fallback: implement HotStuff-2 directly.
  Off-the-shelf CometBFT and Substrate (BABE/GRANDPA) are **not** acceptable,
  because their validator signatures are Ed25519/sr25519.
- **Finality:** a block is final once precommits from more than ⅔ of voting
  power are collected. Target 1–2 s.
- **Validator set:** capped at **100 active validators** (top by stake), with
  delegation for everyone else. Reason: lattice signatures cannot be
  aggregated like BLS. Vote traffic: two rounds × 100 × 2.4 KB ≈ 480 KB per
  block with ML-DSA-44, or ≈ 660 KB with ML-DSA-65.
- **Proposer selection:** stake-weighted round robin. [built: unweighted round
  robin; a validator produces only at the heights it is scheduled for]
- **Epochs:** validator-set changes take effect at epoch boundaries
  (devnet proposal: 1 hour).
- **Slashing:** double-signing (two conflicting signed votes or proposals) is
  provable on chain → [decision: 5%] slash and permanent removal. Downtime
  leads to jailing, not slashing.
- **Unbonding:** 21 days. This must exceed the weak-subjectivity checkpoint
  interval to rule out long-range attacks.
- **Key rotation:** validators can rotate consensus keys at epoch boundaries.
- **Remote signer:** validators sign through an HSM or remote-signer process
  with double-sign protection (a high-water mark of height/round). The seed
  never lives on the networked host.

PoW was considered and rejected. Probabilistic finality makes witness pruning
unsafe, and orphaned fat blocks waste bandwidth.

---

## 7. Networking [planned]

| Path | Transport | Security |
|---|---|---|
| Validator ↔ validator | **QUIC** (`quinn`) | TLS 1.3, key exchange **X25519MLKEM768** or pure **ML-KEM-1024**. Peer identity = ML-DSA validator key, carried in a signed handshake extension. No classical-only fallback. |
| Full node ↔ full node | QUIC | Same; peer identity by node key |
| Wallets, explorers → RPC | **HTTPS** (TLS 1.3, PQ hybrid key exchange) | JSON-RPC over HTTP; **WebSocket (`wss://`)** subscriptions for new heads, transactions and account changes |

- TLS policy reuses Sigilant's `sigilant-tls` approach: TLS 1.2 is absent from
  the binary, and only post-quantum or hybrid groups are offered.
- **Compact block propagation.** Transactions are gossiped once through the
  mempool, and each validator verifies them on arrival (warming its cache).
  Blocks then carry only the header plus 8-byte short transaction ids, salted
  per block to resist collision attacks (as in BIP 152). Validators rebuild
  the block from their mempool and request only the transactions they are
  missing. Each transaction's ~2.5 KB therefore crosses each link about once
  instead of twice, roughly halving validator bandwidth. The witness rule
  (§5.2) still holds: every validator has every witness before it votes; they
  just arrive earlier, through the mempool.
- **Block propagation:** erasure-coded shreds (Turbine-style) over QUIC for
  the compact block and any missing transactions.
- **Mempool gossip:** transactions are forwarded to validators and the next
  few scheduled leaders (Gulf-Stream-style) instead of being flooded to every
  node.
- **WebSocket is not used for P2P**: it adds TCP head-of-line blocking that QUIC
  avoids.

---

## 8. Token [decision]

These are placeholders. The project owner should set them, with legal advice.

| Item | Devnet value | To decide |
|---|---|---|
| Ticker / decimals | STEIN / 9 | — |
| Genesis supply | 1,000,000,000 (faucet) | Total supply, allocation, vesting |
| Issuance | none | Staking rewards (e.g. tapering inflation) vs fixed supply |
| Fee policy | 100% to proposer | Burn base fee + tip |
| Min. validator stake | — | — |

> Issuing and distributing a token can be a regulated activity (in Australia,
> ASIC's view of crypto-assets as financial products). Get legal advice before
> any sale or public distribution.

---

## 9. Security requirements

1. **[built]** Every transaction signature is verified by every validator before
   state changes. Forged signatures are rejected even when the block proposer
   signed the block, and even when the cache holds a valid signature for the
   same body.
2. **[built]** Sender/public-key binding: `address(scheme ‖ pk) == sender`.
3. **[built]** Replay protection: chain id, nonces, expiry.
4. **[built]** Only schemes listed in the chain parameters are accepted; a key
   never verifies another scheme's signature; unknown tags do not decode.
5. **[built]** Checked arithmetic everywhere, enforced by the clippy
   `arithmetic_side_effects` lint in library code. `unsafe_code = "forbid"`.
6. **[built]** Seed files (`<scheme>:<hex>`) are created exclusively (never
   overwritten) with mode 0600 on Unix, and the seed is zeroized after use.
7. **[built]** Bounded mempool (100k total, 64 per sender), bounded key and
   signature caches; RPC body limit 64 KB; RPC binds to localhost by default.
   Admission requires that the sender's balance covers the transaction and
   that its nonce directly follows the account's and its queued ones, so
   nothing that could never be included occupies the pool.
8. **[planned]** RPC rate limiting per IP, since each submission costs a
   ~100 µs signature verification before any fee is paid. Public RPC goes
   behind a proxy/CDN, on RPC nodes, never on the validator core.
9. **[planned]** Fuzzing (cargo-fuzz) of every decoder: Borsh transaction,
   block, and genesis.
10. **[planned]** Property tests: supply conservation across random transaction
    sequences. [built: deterministic tests, for STEIN, every token and every
    pool's shares across a mixed history of transfers, swaps, deposits and
    withdrawals]
11. **[planned]** Two independent external audits (protocol and cryptography
    integration) before mainnet. A bug bounty from testnet onward.
12. **[planned]** Reproducible builds for validator binaries (Linux, built from
    source, no prebuilt objects).

---

## 10. Performance targets and hardware

### 10.1 Where the limit is
CPU is not the constraint: at 2,000 TPS, ML-DSA-44 verification needs about
**0.2 of one core** (each signature is verified once, thanks to the cache).
**Bandwidth and storage are the limits.** Figures below use ML-DSA-44
steady-state transfers (2,543 B, of which the witness is 2,422 B):

| Target | Raw tx data per validator | Witness storage, 6 h retention | Bodies kept (archive), per day |
|---|---|---|---|
| 1,000 TPS | ~20 Mbit/s | ~52 GB | ~10 GB |
| **2,000 TPS** | **~41 Mbit/s** | **~105 GB** | ~21 GB |
| 5,000 TPS | ~102 Mbit/s | ~262 GB | ~52 GB |

These assume sustained full load. Real chains average a fraction of capacity.
Gossip overhead adds 2–3× to raw bandwidth with compact blocks (§7), or 4–6×
without them.

**Launch target: 2,000 TPS capacity, 1 s blocks, ~2 s finality.** Raise it
only after measuring a real multi-region testnet.

### 10.2 Comparison (approximate public figures)

| | Ethereum L1 | Solana | Stein (target) |
|---|---|---|---|
| Real-world TPS | ~15–30 | ~1,000–4,000 user TPS (plus votes) | 2,000 capacity at launch |
| Block / slot time | 12 s | 0.4 s | 1 s |
| Finality | ~13 min (2 epochs) | ~1–2 s optimistic, ~13 s rooted | ~2 s, deterministic |
| Signature size | 65 B (ECDSA) | 64 B (Ed25519) | 2,420 B (ML-DSA-44); 666 B (FN-DSA-512) |
| Typical transfer size | ~110 B | ~250 B | 2,543 B (ML-DSA-44); 789 B (FN-DSA-512) |
| Quantum-safe signatures | No | No | **Yes** |
| Validators | ~1M (BLS aggregation) | ~1,000–1,500 | ≤100 active + delegation |

### 10.3 Horizontal scaling by role
A validator does not have to be one large machine. Each role scales on its
own, and only the small consensus core needs to be a strong, trusted box:

```
                    ┌─ RPC nodes ×N ── non-voting replicas behind a load balancer
                    │   (Cloudflare in front: cache, rate limit, DDoS)
 validator core ────┼─ sig-verify workers ×N ── optional, same operator, stateless
 (consensus +       │
  execution)        └─ archive ── streams finalized blocks to R2/S3 (§5.5)
```

- **Validator core:** consensus, execution, and recent state and witnesses
  only. It never serves wallets or explorers.
- **RPC nodes:** non-voting full nodes that follow finalized blocks. Add as
  many as user traffic needs; they never slow consensus.
- **Signature-verify workers:** verification is fully parallel and stateless.
  If a validator runs short of CPU, it hands batches to worker machines it
  runs itself, instead of upgrading to a larger CPU.
- **Archive:** object storage, not validator disk.
- **Protocol-level sharding is not planned for v1**, but the protocol is kept
  **shard-ready**: see [SHARD-READY.md](SHARD-READY.md) for the target model,
  the invariants every change must keep, and the gaps to close before testnet.
  The scaling path past ~5,000 TPS is FN-DSA, STARK signature aggregation and
  validity rollups (§12). Validity proofs are a prerequisite for sharding.

### 10.4 Hardware (revised)

| Role | CPU | RAM | Disk | Network |
|---|---|---|---|---|
| Devnet (this repo) | any 4+ cores | 4 GB | 10 GB | localhost |
| **Validator core, ML-DSA-44, 2k TPS** | **8 cores** (AVX2) | **32 GB ECC** | **1 TB NVMe** | 1 Gbit/s port, ~150 Mbit/s sustained, static IP, open UDP |
| Validator core, with FN-DSA (future) | 4 cores | 16 GB ECC | 500 GB NVMe | ~50–100 Mbit/s sustained |
| RPC node (scale out) | 4–8 cores | 16–32 GB | 500 GB NVMe | per user traffic |
| Archive | 4 cores | 16 GB | object storage (R2/S3) | — |

Changes from v0.1 (16 cores, 64 GB, 2× 2 TB):
- CPU halves: verification cache plus ML-DSA-44.
- Disk shrinks by more than 4×: 6-hour retention plus archive offload.
- Bandwidth roughly halves: compact blocks plus smaller transactions.
- RAM halves: bounded key cache, and state moving to disk (phase 2).

The validator-core figures are estimates from §2 and §10.1. Confirm them on
the multi-node testnet.

ECC memory is still recommended for validators: a flipped bit can make a
validator sign conflicting votes and be slashed.

Plus an **HSM or remote signer** for validator keys once vendors support
ML-DSA (still landing in 2026).

### 10.5 Cloudflare Containers: not for validators
Cloudflare Containers cap instances at **4 vCPU, 12 GiB RAM and 20 GB disk**
(Cloudflare docs, limits page). They are designed as request-driven instances
behind Workers: disk is not durable state, instances sleep when idle, and
inbound traffic arrives as HTTP through a Worker rather than as raw UDP/QUIC
from peers. A validator needs always-on operation, durable NVMe and inbound
QUIC, so it **cannot** run there. Full nodes can't either.

Cloudflare **does** fit the edges:
- **Block explorer front end** (steinscan) on Workers/Pages.
- **Public RPC gateway**: Workers in front of your own RPC nodes, with caching,
  rate limiting and DDoS protection.
- **Faucet** for the testnet.
- **Archive storage** on R2 (§5.5).

---

## 11. Roadmap

| Phase | Deliverable | Status |
|---|---|---|
| 0 | Spec (this document) | done |
| 1 | Single-node devnet: crypto, transactions, state, block log, RPC, CLI wallet | **done** |
| 1.1 | Algorithm-agile keys, ML-DSA-44 default, verification cache, 6 h retention, bounded key cache | **done** |
| 1.2 | Follower nodes over HTTPS with PQ TLS; Docker validator plus Cloudflare follower | **done** |
| 1.3 | FN-DSA test network (experimental) | **done**, superseded by 1.4 |
| 1.4 | FN-DSA-512 accounts ahead of the final FIPS 206 (§3.5) | **done** |
| 1.5 | Tokens and liquidity pools as native transaction types (§4.6, §4.7); `stein token` and `stein pool`; steinscan shows them | **done** |
| 2 | Hardening: fuzzing, property tests, RocksDB storage (state on disk), JMT state **keyed by address nibbles**, RPC rate limits, archive offload to R2 | next |
| 2.1 | Shard-readiness gaps ([SHARD-READY.md](SHARD-READY.md) §5): supply < 2¹²⁷, fee pool, `version` fields, `shard_id: 0` in RPC | **done** |
| 3 | P2P: QUIC + PQ TLS, transaction gossip, **compact block propagation**, RPC-node role | |
| 4 | Consensus: Malachite + ML-DSA votes, staking, slashing, epochs, checkpoints. **Formal model** of the consensus protocol (Quint/TLA+, model-checked for safety). **Deterministic simulation testing**: the whole network in one process with injected crashes, partitions, delays and Byzantine validators, run millions of times per change | |
| 1.4 | **steinscan** live explorer; node `GET /events` (server-sent events), `/blocks`, `/mempool`, account history, CORS | **done** |
| 5 | Public testnet, faucet, steinscan indexer (Postgres), wallet SDK (TypeScript/Rust) | |
| 6 | Audits ×2, bug bounty, multi-region load tests, mainnet genesis ceremony | |

**Reliability targets.** Safety (never finalizing conflicting blocks, never
creating or losing funds) is **absolute**: established by the formal model and
tested by simulation, never budgeted. Liveness (finality keeps advancing) is
targeted at **99.999%**, about 5 minutes of stalled finality per year. That
can only be demonstrated by running a decentralised testnet and then mainnet
over time, not by testing alone.
| C0–C4 | WASM smart contracts: design, VM and receipts, SDK and standards, audit (see [CONTRACTS.md](CONTRACTS.md) §14) | design written |
| later | Final FIPS 206 as a new scheme tag, `RotateKey`, STARK signature aggregation, light clients | |

### 11.1 steinscan (block explorer) [built: live explorer; planned: indexer]
**Built:** `explorer/`, a static app that reads a node's RPC and follows its
live event stream (`GET /events`). It has a live block and transaction feed
(pending, then confirmed), network stats, search, and block, transaction and
address pages that show each signature scheme and key registration. It is
deployable to Cloudflare.

**Planned for mainnet scale:**
An Etherscan-style explorer: blocks, transactions, accounts, validators,
supply and network stats.
- **Indexer** (Rust): follows an RPC node over WebSocket and writes to
  Postgres.
- **API**: read-only REST/GraphQL over Postgres.
- **Front end**: Next.js on Cloudflare Pages or Vercel.
- PQ-specific views: each account's scheme and whether its key is registered,
  witness availability and pruning status per block, and validator
  key-rotation history.

The node RPC already serves blocks, transaction lookup and accounts, which is
the data an indexer starts from.

---

## 12. Future work
- **Final FIPS 206:** when it is published and an implementation reaches 1.0,
  add it as a new scheme tag beside tag 3 (§3.5) and let accounts rotate to
  it. Adding a scheme to a running network needs an upgrade rule (an
  activation height), which does not exist yet: `tx_schemes` is fixed at
  genesis.
- **STARK signature aggregation:** a block proposer proves "all N signatures
  verify" in one hash-based (post-quantum) proof of ~100–200 KB. Individual
  signatures then never enter blocks, so validators and archives stop paying
  for them. This is the path past ~5,000 TPS, and the basis for validity
  rollups.
- **An activation rule for upgrades:** a height at which new rules start, agreed
  in advance, so validators run by different operators can upgrade without
  stopping at each other's blocks (§4.8).
- **A receipts root** in the block header, committing to what each
  transaction did (§4.7).
- **Smart contracts:** WebAssembly, decided. Design in
  [CONTRACTS.md](CONTRACTS.md): wasmtime with deterministic fuel metering, a
  Rust SDK, asynchronous calls between contracts (shard-ready), storage
  deposits, and ML-DSA verification inside contracts.
- **Light clients:** JMT proofs plus finalized header chains signed by
  validators.
