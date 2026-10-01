# Stein Chain — Smart Contracts (WASM)

*Design, version 0.2, 2 October 2026. Companion to [SPEC.md](SPEC.md)
and [SHARD-READY.md](SHARD-READY.md). Status: **accepted** (decisions in §15);
nothing here is built yet.*

**Decision:** smart contracts run as **WebAssembly**, in a deterministic,
fuel-metered sandbox, and are written primarily in **Rust** against a Stein
SDK. Calls between contracts are **asynchronous messages**, so the design stays
shard-ready. The rest of this document is how.

---

## 1. Goals and non-goals

**Goals:**
1. **Post-quantum native.** Contracts can verify ML-DSA (and, on the test
   network, FN-DSA) signatures cheaply, so post-quantum multisig, custody and
   account abstraction are first-class features, not afterthoughts.
2. **Deterministic.** Every validator gets the same result, gas and state root
   for every call, on any CPU and operating system. A divergence is a
   consensus failure.
3. **Bounded.** Gas bounds CPU per block, and storage deposits bound state
   growth, so validator hardware stays within SPEC §10 (8 cores, 32 GB, 1 TB).
4. **Shard-ready.** The design keeps every invariant in SHARD-READY.md.
   Contract storage is a subtree under the contract's address, and calls
   between contracts are messages.
5. **Auditable.** Source code can be verified against deployed code, and
   steinscan shows a contract's events, calls and storage use.

**Non-goals for the first release:**
- EVM compatibility;
- synchronous composability across contracts (see §6.4);
- on-chain randomness beyond what is documented in §7.4;
- floating-point arithmetic;
- contracts that pay their own fees (that's §11's account abstraction, later).

---

## 2. Virtual machine

| Choice | Decision | Why |
|---|---|---|
| Engine | **wasmtime** (Cranelift compiler), pinned to an exact version | Mature, heavily fuzzed, fast, and a Bytecode Alliance project with a security process. |
| Second engine | **wasmi** (interpreter), in CI only | Differential testing: both engines must agree on results and gas for every test and fuzz case (§12). |
| Metering | wasmtime **fuel**: deterministic instruction counting | Gas can't depend on timing, which epoch interruption would. |
| Floats | **Rejected at deploy** | Removes the main source of cross-platform non-determinism. NaN canonicalisation is also enabled as a second line of defence. |
| WASM features | MVP plus bulk memory, sign extension, multi-value and mutable globals. **No** threads, relaxed SIMD, memory64, GC, exceptions or tail calls in the first release. | Each feature enabled is consensus surface to test. |
| Memory | Up to **64 MiB** linear memory per instance; stack, table, globals and function counts capped at deploy | Bounded RAM per call. |
| Compilation | Once per code hash, at first use; the native artefact is cached on disk and keyed by `(code hash, engine version)` | Compiling is far slower than running. The cache isn't consensus state: losing it only costs time. |
| Instantiation | Pooling allocator: pre-reserved slots, one fresh instance per call | Instances share nothing; a call can't see a previous call's memory. |

**Engine upgrades are consensus upgrades.** The wasmtime version is part of
the protocol version. Upgrading it means a coordinated network upgrade at a
set height, after the differential and determinism suites pass on the new
version.

---

## 3. Accounts and state

### 3.1 Two kinds of account
Today an account is `{ balance, nonce, public_key? }`. Contracts add a second
kind:

```
Account = Key      { balance, nonce, public_key? }               // today's accounts
        | Contract { balance, code_hash, storage_root, storage_bytes, upgrade_authority? }
```

- **Contract address** = `BLAKE3-derive("stein contract address v1", deployer ‖ deployer_nonce)`.
  It is deterministic and known before deployment, and it can never collide
  with a key-derived address, because the derive-key contexts differ.
- A contract has **no key**, so it can never be the `sender` of a
  transaction. It acts only by sending messages (§6).
- **Code** is stored once per `code_hash` (`BLAKE3-derive("stein code v1", wasm)`)
  in a code store, so a thousand token contracts with the same code store it
  once.

### 3.2 Storage layout (shard-ready, I-4)
Contract storage is a key-value map inside the state tree, placed **under the
contract's address**:

```
state key = contract_address ‖ BLAKE3-derive("stein storage key v1", user_key)
```

In the Jellyfish Merkle Tree keyed by address nibbles (SPEC §5.4), each
contract's storage is a **subtree of its own address**. A shard then moves a
contract with its storage in one piece, and a light client can prove a single
storage value. `storage_root` in the account leaf commits to that subtree.

Limits: keys up to **256 bytes**, values up to **64 KiB**.

### 3.3 Storage deposit (state stays bounded)
Each stored byte, including the code it deploys, locks **storage deposit**
from the contract's own balance: `storage_bytes × deposit_per_byte`.
- A write that would exceed what the balance covers fails, and the call
  reverts.
- Deleting data releases its deposit back to the balance.
- Nobody can grow the state without paying for it, so validator disks stay
  within SPEC §10.

Ongoing rent was considered and rejected: contracts silently disappearing when
rent runs out surprises users. A deposit is visible up front and refundable.
[decision: initial `deposit_per_byte`]

---

## 4. Transactions

Two new actions join `Transfer`:

```rust
enum Action {
    Transfer { to, amount },                                   // unchanged; never runs code (§6.3)
    Deploy   { code: Vec<u8>, init: Option<Call>, upgrade_authority: Option<Address> },
    Call     { contract: Address, method: String, args: Vec<u8>, deposit: Amount, gas_limit: u64 },
}
```

- Both are signed by the sender's ML-DSA key like any transaction. Witness
  rules, key registration and verification caching apply unchanged.
- **Deploy** limits: code at most **512 KiB**, which is paid for by the byte
  fee and then the storage deposit. The module is validated at deploy (§7.1);
  an invalid module is rejected and nothing is stored.
- **Call** arguments are Borsh-encoded bytes; the SDK generates the encoding
  (§9).
- Method names starting with `__` are reserved.

### 4.1 Fees
```
fee = byte_fee (existing: min_fee_per_byte × encoded size)
    + gas_used × gas_price
```
- The sender sets `fee` as the **maximum** they'll pay, and `gas_limit` per
  call.
- The byte fee and `gas_limit × gas_price` are reserved up front, so a call
  can never run past what the sender can pay.
- Unused gas is refunded **as a credit** (I-1: credits never fail).
- `gas_price` starts as a chain parameter. It moves to an EIP-1559-style base
  fee together with the transfer fee market (SPEC §4.4).

---

## 5. Gas schedule

Gas is measured on the **reference validator** (SPEC §10.4: 8 cores, ML-DSA-44
configuration), with **1 gas ≈ 1 ns** of single-core work. Every price below
comes from a benchmark checked into the repository, not from guesswork.

| Item | Basis (initial, to be benchmarked) |
|---|---|
| WASM instructions | wasmtime fuel, 1 fuel per instruction, scaled by a calibrated factor |
| Memory growth | per 64 KiB page added |
| `storage_read` | base + per byte read |
| `storage_write` | base + per byte written (plus deposit, §3.3) |
| `blake3` / `sha3_256` | base + per byte hashed |
| `ml_dsa_verify` (44 / 65) | measured: ~100 µs / ~150 µs per verify (SPEC §2) |
| `fn_dsa_verify` (test network only) | measured: ~15 µs per verify |
| Sending a message (§6) | base + per byte of arguments |
| Emitting an event | base + per byte |

- **Block gas limit:** set so that a block full of worst-case calls executes
  in at most **300 ms** on the reference validator, which keeps 1-second blocks
  safe. [decision: exact value after benchmarks]
- **Per-call gas cap:** a single call can use at most ¼ of the block gas
  limit.

---

## 6. Execution model

### 6.1 Calls are receipts
A `Call` transaction does two things:
1. In the sender's transaction: check the nonce, reserve the fee and attach
   the deposit (debited from the sender). This touches the **sender only**
   (I-2).
2. It creates a **receipt**, `Receipt { id, from, to: contract, method, args,
   deposit, gas, origin_tx }`, which the contract executes.

A contract that calls another contract, or transfers STEIN, does so by
emitting **outgoing receipts**. These execute after the current call
finishes, never inside it.

### 6.2 Ordering within a block (single shard, today)
```
block:
  1. transactions, in order        (sender-side: nonces, fees, deposits)
  2. receipts, in FIFO order       (new receipts join the back of the queue)
  3. stop when the block's gas is used up; unexecuted receipts carry over to
     the next block, at the front, in order
```
- Every validator runs the same queue in the same order, so the result is
  deterministic.
- In a sharded future, step 2 becomes "receipts for my shard, from any shard's
  previous chunk", with the same semantics.

### 6.3 Credits never fail (I-1)
- A plain `Transfer` to a contract **credits its balance and runs no code**.
  There is no "receive hook" that could reject funds.
- When a call fails, its attached deposit returns to the caller **as a refund
  receipt, which is a credit** and so cannot fail.
- The same holds for unused gas.

### 6.4 Asynchronous, not synchronous, composability
The cost is real. **Contract A cannot call contract B and use the answer
within the same call.** A instead sends B a message with a **callback**: B
runs, and then A's callback runs with B's result (`Promise` in the SDK).
Consequences:
- **No flash loans** and no single-transaction atomic arbitrage across
  contracts.
- **Synchronous re-entrancy is impossible**: B can never re-enter A
  mid-call. But callbacks run after other receipts may have changed A's
  state. The SDK makes that explicit: callbacks re-read state, and patterns
  like "lock, then resolve in the callback" are documented and supported by
  helpers.
- Token standards are designed for it (§10), following NEAR's proven
  `transfer_call` pattern.

Synchronous calls between contracts on the same shard were considered and
rejected for v1. They make shard placement visible to contract authors, so
code that works today could break when sharding arrives (SHARD-READY.md).
[decision: revisit only together with a sharding design]

### 6.5 Failure
- A trap (panic, out of gas, memory limit, invalid argument) **reverts that
  receipt's storage writes and messages**. Gas used is still charged, and the
  deposit is refunded (§6.3).
- Receipts that already completed stay completed: there is no global rollback
  across receipts. That's the price of asynchrony, made explicit in the SDK.

---

## 7. Determinism and safety

### 7.1 Deploy-time validation
A module is rejected at deploy if:
- it isn't valid WASM;
- it uses a feature outside §2's allow-list;
- it contains **any** floating-point instruction or type;
- it imports anything outside the `stein` host namespace;
- it exceeds a limit (memory, tables, functions, locals or exports);
- it lacks the required exports (`memory`, at least one method);
- it has a `start` function: initialisation runs only through the `init` call,
  which is metered.

### 7.2 No ambient non-determinism
No clock (only the block timestamp), no randomness, no I/O, no threads, and
no access to other contracts' memory. Host functions are pure functions of
consensus state.

### 7.3 Resource limits
Each call runs in a fresh instance under the gas cap and 64 MiB of memory.
Recursion depth is bounded by the stack limit, and hitting it traps
deterministically.

### 7.4 Randomness
- Contracts can read `block_hash(height)` for **past** heights only.
- It is documented as **biasable by the proposer**, and unsuitable for
  lotteries or anything of value.
- A verifiable random function from validators, built on post-quantum
  primitives, is later work.

### 7.5 Upgrades
- Code is **immutable by default**.
- A contract deployed with an `upgrade_authority` (an account or multisig
  contract) can be replaced through an `Upgrade` call from that authority. The
  storage stays; the code hash changes.
- steinscan shows the authority and the full upgrade history.
- The authority can be renounced, permanently, by the authority itself.

---

## 8. Host API (`stein` namespace)

| Group | Functions |
|---|---|
| Context | `input()`, `caller()`, `current_contract()`, `attached_deposit()`, `gas_left()`, `block_height()`, `block_timestamp_ms()`, `chain_id()` |
| Storage | `storage_read(key)`, `storage_write(key, value)`, `storage_remove(key)`, `storage_has(key)` |
| Balance | `balance()`, `storage_bytes()`, `deposit_per_byte()` |
| Messages | `promise_call(contract, method, args, deposit, gas)`, `promise_transfer(to, amount)`, `promise_then(promise, method, args, gas)`, `promise_result(i)` |
| Output | `return_value(bytes)`, `panic(msg)`, `emit(topic, data)` (events, indexed by steinscan) |
| Crypto | `blake3(data)`, `sha3_256(data)`, `ml_dsa_verify(scheme, public_key, message, signature)`, `address_of(public_key)` |

**Signature verification inside contracts** uses its own domain, so a
signature made for a contract can never be replayed as a transaction (or the
reverse):

```
signed message = "STEIN-SIG-v1" ‖ domain 4 (contract) ‖ contract_address ‖ chain_id ‖ message
```

`ml_dsa_verify` accepts exactly the schemes in the chain's `tx_schemes`
parameter, so FN-DSA works in contracts only on the FN-DSA test network.

---

## 9. SDK and toolchain

- **`stein-sdk`** (Rust, `no_std` plus `alloc`, target
  `wasm32-unknown-unknown`):
  - `#[contract]`, `#[init]`, `#[view]`, `#[payable]` and `#[callback]`
    macros;
  - typed storage collections (`Map`, `Vector`, `LazyCell`);
  - `Promise` for messages;
  - Borsh argument encoding.
- **ABI:** the build emits a JSON ABI (methods, arguments, events), which
  wallets and steinscan use to encode calls and decode events.
- **`stein-sdk-test`:** runs contracts natively against an in-memory chain,
  with the same VM, gas metering and receipt queue, so tests measure real gas.
- **Reproducible builds:** a pinned Docker builder (the same discipline as the
  node image). The same source always produces the same WASM, which is what
  makes source verification on steinscan possible.
- AssemblyScript or other languages: later, if there's demand.

---

## 10. Standards (first contracts)

| Standard | What | Note |
|---|---|---|
| **STN-1 Fungible token** | balances, transfers, `transfer_call` with a callback (NEP-141 style), metadata | Designed for async calls; no "approve then transferFrom" race. |
| **STN-2 Non-fungible token** | ownership, transfers, metadata | |
| **STN-3 PQ multisig** | M-of-N **ML-DSA** signers, proposals, time locks | The flagship. Quantum-safe custody for treasuries and the future foundation. |
| **STN-4 Events** | event naming and encoding conventions | For indexers and steinscan. |

---

## 11. Later: smart accounts (account abstraction)
Because Stein accounts already carry a **scheme tag** (SPEC §3.4), the natural
next step is accounts whose authorisation logic is a contract: M-of-N keys,
social recovery, spending limits, key rotation, or a mix of ML-DSA and FN-DSA
keys. A smart account validates a transaction through a metered `validate`
call before it executes. This needs care, because validation must stay cheap
and bounded to protect mempool admission. It's a phase after contracts are
stable.

---

## 12. Testing and assurance

1. **Differential execution:** wasmtime and wasmi run every SDK test, every
   standard contract and every fuzz case. Results, gas, storage writes and
   events must be identical.
2. **Cross-platform determinism:** the CI matrix (Linux x86-64 and ARM64,
   Windows, macOS) runs the same contract suite and compares state roots.
3. **Fuzzing:**
   - module validation (malformed and adversarial WASM);
   - the host API (random arguments, limits);
   - the receipt queue (random interleavings of calls, callbacks and
     failures).
4. **Gas benchmarks** in CI: pricing regressions fail the build when an
   operation's measured cost drifts more than 20% from its gas price.
5. **Property tests:**
   - supply conservation across arbitrary call graphs;
   - credits never fail;
   - a failed receipt leaves no state change except gas and the refund.
6. **Audit:** the VM integration, host API and gas schedule get their own
   external audit, separate from the protocol audit (SPEC §9).

---

## 13. Explorer and RPC

- **RPC:**
  - `POST /contract/{address}/view/{method}`: read-only, gas-capped, not on
    chain;
  - `POST /simulate`: estimates gas and shows the receipts a call would
    create;
  - `GET /contract/{address}`: code hash, ABI (if published), storage bytes,
    upgrade authority.
- **steinscan:**
  - contract pages: code hash, verified source, ABI, a *Read contract* tab;
  - an events tab;
  - the receipt tree of each transaction (Etherscan's "internal
    transactions");
  - storage deposit, and upgrade history.

---

## 14. Roadmap

| Phase | Deliverable |
|---|---|
| **C0** | This design; benchmark harness for gas pricing |
| **C1** | VM integration: deploy validation, `Deploy` and `Call`, fuel metering, storage under the JMT layout (needs SPEC phase 2), storage deposit, the receipt queue in a single shard |
| **C2** | `stein-sdk`, `stein-sdk-test`, ABI, reproducible builds; STN-1 and STN-3; RPC view and simulate; steinscan contract pages |
| **C3** | Async calls between contracts and callbacks, fully exercised; STN-2; fuzzing and differential suites at full strength |
| **C4** | External VM audit; contracts on the public testnet |
| **later** | Smart accounts (§11), VRF randomness, more languages |

C1 depends on SPEC phase 2: the JMT keyed by address nibbles, and state on
disk.

---

## 15. Decisions (accepted 2 October 2026)

| # | Decision | Accepted |
|---|---|---|
| 1 | Composability | **Asynchronous only** (§6.4). Flash-loan-style DeFi is out of scope by design. |
| 2 | State growth | **Storage deposit**, refundable, no rent (§3.3). The initial price per byte is set from the disk budget when C1 is benchmarked. |
| 3 | Upgradeability | **Immutable by default, with an opt-in upgrade authority**, visible on steinscan and renounceable (§7.5). |
| 4 | Code size | **512 KiB** maximum per contract (§4). |
| 5 | First standards | **STN-1 fungible token** and **STN-3 post-quantum multisig** ship in C2; STN-2 NFTs follow in C3 (§10). |

Still open, set during C1 from benchmarks: `deposit_per_byte`, `gas_price`,
the block gas limit, and the calibration factor between fuel and gas (§5).
