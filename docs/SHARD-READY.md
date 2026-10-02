# Shard-Ready Design Note

*Status: accepted design constraint. 1 October 2026. Companion to
[SPEC.md](SPEC.md) v0.2.*

## Decision

**Stein will not shard for v1.** Bandwidth and hardware are addressed by
smaller signatures, compact blocks and role separation (SPEC §10). Sharding
would trade one problem (bytes per validator) for harder ones: smaller,
easier-to-attack validator groups, and transfers between shards that take two
steps and can each fail.

**Stein will stay shardable.** Today's choices must not rule sharding out.
This note fixes:
1. the sharding model Stein would use, so today's design can be checked
   against it;
2. the rules every protocol change must follow until then;
3. where the current code already complies, and the gaps to close before
   mainnet.

Sharding should only be revisited if **both** of these hold:
- sustained demand nears the single-chain ceiling (about 5,000 TPS after
  FN-DSA and compact blocks);
- **validity proofs** (STARKs, SPEC §12) exist for Stein's state transition.

Without validity proofs, a shard's small validator group could finalize an
invalid state, and the rest of the network could only detect it after the
fact. That is the security failure sharding must never have. Validity proofs
are a hard prerequisite, not an optimisation.

---

## 1. The target model (for checking compatibility, not building)

**One consensus, many execution shards** (the NEAR Nightshade / Ethereum
data-sharding family), not independent shard chains:

```
                 one BFT consensus, one finality
 block h:  [ header | shard 0 chunk | shard 1 chunk | … | shard N-1 chunk ]
                         │                │
               validity proof 0   validity proof 1     (all verified by everyone)
                         │                │
           outgoing receipts ──────► included as incoming receipts in block h+1
```

- **One chain, one finality.** Every block contains one *chunk* per shard. All
  shards finalize together, so there is no inter-shard reorg and no
  "finalized on shard A, pending on shard B".
- **Chunk producers** execute their shard and publish a validity proof. Every
  validator verifies every proof, which is cheap: proof size and verify time do
  not grow with the number of transactions. So security comes from the
  **whole** validator set, not from a shard committee.
- **Data availability:** chunks are erasure-coded and sampled. Validators do
  not download every chunk; they check that each one is retrievable.
- **Shard assignment by address prefix.** `shard(address)` = the address's top
  bits, under a **shard map** of prefix ranges. Resharding splits or merges
  ranges at epoch boundaries. Addresses are uniform BLAKE3 hashes, so shards
  balance on their own.
- **Cross-shard transfers are asynchronous receipts:**
  1. Block *h*, sender's shard: check nonce and balance, debit amount + fee,
     emit `Receipt { id, to, amount }` into the chunk's `outgoing_receipts_root`.
  2. Block *h+1*, recipient's shard: include the receipt and credit `to`.
     Receipts are applied in origin order, and each receipt id is consumed
     exactly once.
  3. The transfer is final when block *h* is final. The credit is guaranteed
     to happen, and that guarantee is the purpose of invariant I-1 below.

---

## 2. Invariants every change must keep

| # | Invariant | Why sharding needs it |
|---|---|---|
| **I-1** | **A credit can never fail.** Adding funds to an account has no precondition and cannot overflow. | A cross-shard credit lands one block after the debit is final. If it could fail, every transfer would need a refund path and a lock in between. |
| **I-2** | **A transaction reads and conditions only on its sender's state.** Everything else it does is a blind credit or an outgoing message. | The sender's shard can then fully validate the transaction. No synchronous reads across shards. |
| **I-3** | **All per-account sequencing is per account:** nonces, key registration, key rotation. There is no global counter in consensus logic. | Global counters serialise all shards. |
| **I-4** | **State is keyed and committed by address.** Each prefix range of addresses is a subtree with its own root. | A shard's state root is a subtree root, and resharding moves subtrees without re-hashing the world. |
| **I-5** | **Transaction identity and validity do not depend on block position.** The id is over the body only, and the witness rule is per transaction. | A transaction must keep its identity when routed to a shard chunk instead of a monolithic block. |
| **I-6** | **Fees never require a cross-shard credit on the hot path.** | Today every transaction credits the proposer, which may live on another shard. That would turn every transaction into a cross-shard one. |
| **I-7** | **Wire formats are versioned.** Headers, transactions and receipts carry a version, so adding `shard_id` and receipt roots is an upgrade, not a breaking redesign. | Sharding adds header fields. |
| **I-8** | **Clients route by address, never assume one global mempool or one block stream.** Wallets and steinscan model `shard_id` (always 0 until sharding). | Avoids an ecosystem-wide migration later. |

Any new transaction type is reviewed against I-1 and I-2 first. A contract VM
(SPEC §12) is where these are most at risk. Contracts must call each other
through asynchronous messages, not synchronous calls into other accounts' state.

---

## 3. Audit of the current code

| Invariant | Status | Where | Action |
|---|---|---|---|
| I-1 | **Gap (small)** | `stein-state` `apply`: credits use `checked_add` and can return `Overflow` | Enforce **total supply < 2¹²⁷** at genesis and in any future issuance. Then no balance plus credit can overflow, and the credit path cannot fail. Keep `checked_add` as a defensive assertion. |
| I-2 | Compliant | `Action::Transfer` reads sender only; recipient credit is blind | Keep for every new action |
| I-3 | Compliant | Nonce and key registry are per account. `proposer_for(height)` is consensus-wide, which is correct with one consensus. | — |
| I-4 | **Gap (planned anyway)** | State root is an RFC 6962 tree split by **count**, so address-prefix ranges are not subtrees | Phase 2 Jellyfish Merkle Tree **must be keyed by address nibbles** (radix-16), making each prefix range a subtree. Recorded as a hard requirement on the phase 2 work. |
| I-5 | Compliant | `TxBody::id` covers the body only; witnesses are verified per transaction | — |
| I-6 | **Gap (design)** | Every transaction credits the proposer's account | Before mainnet: fees go to a per-block fee pool (burned base fee plus tips), distributed to validators once per epoch. This also fits the planned EIP-1559-style burn (SPEC §4.4). |
| I-7 | **Gap (small)** | `BlockHeader` and `Transaction` have no version field | Add `version: u8` to the header and transaction before testnet genesis. |
| I-8 | n/a yet | RPC and CLI | RPC responses include `"shard_id": 0` once the format is next touched; steinscan models it from the start. |

None of the gaps is costly. Closing them before **testnet genesis** costs a few
hours. Closing them after mainnet would take hard forks.

---

## 4. What this costs and saves

- **Cost now:** about a day of work for the four gaps above, mostly the
  fee-pool change.
- **Cost of not doing it:** the main trap is a mainnet with per-transaction
  proposer credits and an address-unaware state tree. Retrofitting sharding
  onto that means migrating every account's state commitment and changing
  fee economics at the same time.
- **What is deliberately not done now:**
  - shard maps, chunk formats or receipt types in code;
  - data-availability sampling;
  - cross-shard mempool routing.

  All of these would be speculative until validity proofs exist.

## 5. Checklist before testnet genesis

- [x] Genesis rejects total supply ≥ 2¹²⁷ (`GenesisError::SupplyTooLarge`).
      Any future issuance must keep below it (I-1)
- [ ] Jellyfish Merkle Tree keyed by address nibbles (I-4, phase 2)
- [x] Fee pool (system account) with per-epoch payout by voting power
      replaces per-transaction proposer credit (I-6)
- [x] `version` field in block header and transaction body; unknown versions
      are refused (I-7)
- [x] RPC exposes `shard_id: 0` in `/status` and block views (I-8)
- [ ] New transaction types reviewed against I-1 and I-2 (standing rule)
