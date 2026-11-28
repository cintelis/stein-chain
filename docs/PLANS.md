# Stein Chain: what is planned

*Draft, 28 November 2026.*

This document describes directions the project intends to take that are not
in the protocol specification yet. **Nothing here is built.** Each part says
what it would do, what it cannot do, and what is still undecided. When a part
is designed in detail it moves into [SPEC.md](SPEC.md) or
[CONTRACTS.md](CONTRACTS.md), and when it is built it is marked **[built]**
there.

What exists today is in the specification: a single-validator devnet with
post-quantum accounts, native tokens and liquidity pools, a browser wallet
and an explorer.

Status markers, as in the specification: **[planned]** = intended, not yet
implemented; **[decision]** = open question for the project owner.

---

## 1. Classic assets: Bitcoin, Ether and others [planned]

Bitcoin and Ethereum secure ownership with ECDSA (and, for Bitcoin, Schnorr)
signatures, which a large enough quantum computer could break. Stein Chain
intends to give holders of those assets two things.

### 1.1 What Stein Chain can and cannot protect

A separate chain cannot change how another chain checks signatures. Coins
that stay on Bitcoin or Ethereum are protected by those chains' own rules and
nothing Stein Chain does alters that. What Stein Chain can protect is an
asset, or a claim on an asset, that is **held on Stein Chain**: every
transfer of it then needs a post-quantum signature.

### 1.2 Bridged assets [planned]

A holder locks BTC or ETH on its own chain and receives the same amount as a
token on Stein Chain. On Stein Chain that token moves, trades in pools and is
held under ML-DSA or FN-DSA keys like any other. Redeeming burns the token
and releases the original.

- **What this protects:** who owns the bridged token, and every movement of
  it, on Stein Chain.
- **What it does not protect on its own:** the coins locked on the original
  chain. They sit there under that chain's classical signatures, held by the
  bridge. If those can be forged, the lock can be emptied whatever Stein
  Chain records. Measures that reduce this (never reusing a lock address, so
  its public key is not revealed until it is spent; moving the lock to
  post-quantum keys as soon as the original chain offers them) are part of
  the design work, not a solved problem.
- **[decision]** Who operates the bridge, and how many parties must agree to
  release funds. Bridges have been the largest single source of losses in
  this industry; this part will be designed, reviewed and audited on its own
  before it carries real value.

### 1.3 Quantum-safe custody [planned]

Accounts governed by several post-quantum keys, for treasuries, funds and
individuals who want more than one key between an attacker and their assets:
M-of-N ML-DSA signers, proposals that others approve, and time locks before
large movements. This is the STN-3 multisig standard in
[CONTRACTS.md](CONTRACTS.md) §10. It applies to anything held on Stein Chain,
bridged assets included.

---

## 2. Agents with accounts of their own [planned]

AI agents that hold and move assets need limits that do not depend on the
agent behaving. The plan is for the chain itself to enforce them.

- An owner's account authorises an agent's key to act for it, within rules
  the owner sets: how much in one transaction, how much in a day, which
  recipients or assets, and until when.
- Validators check those rules on every transaction the agent signs. A
  transaction outside them is refused like any invalid transaction.
- The owner can change or revoke the authorisation at any time, and the
  agent's key can never do more than the rules allow.

This builds on smart accounts ([CONTRACTS.md](CONTRACTS.md) §11). The
[stein-agent](https://github.com/steinchain/stein-agent) prototype shows the
idea today with limits the agent's own software enforces; the plan is to move
that enforcement into the chain.

**[decision]** Whether agent authorisations are a native transaction type,
as tokens and pools are, or a standard contract.

---

## 3. Asset management and DeFi [planned]

**Built today:** fixed-supply tokens anyone can issue, and constant-product
liquidity pools that trade them, as native transaction types (SPEC §4.6,
§4.7).

**Planned:** WebAssembly smart contracts ([CONTRACTS.md](CONTRACTS.md)), and
on them the standards for tokens with rules of their own, non-fungible
tokens and multisig custody. Lending, vaults and other applications are for
contracts to provide once that layer exists; none is designed yet.

---

## 4. Token sale [decision]

A sale of STEIN is intended, timed with the launch of mainnet. **No sale is
open today, and no terms have been published.** Total supply, allocation,
price, eligibility and dates are undecided, and will be published on
steinchain.com when they are.

Until then:

- STEIN on the devnet has no value and cannot be bought.
- Anyone offering STEIN for sale, or a place in a sale, is not Stein Chain.
- Official announcements appear only on [steinchain.com](https://steinchain.com),
  [@steinscan](https://x.com/steinscan) on X and
  [t.me/steinchain](https://t.me/steinchain). Nobody from the project will
  message you first or ask for a key, a passphrase or a payment.

Issuing and selling a token can be a regulated activity. The terms of any
sale will follow legal advice in the places it is offered.
