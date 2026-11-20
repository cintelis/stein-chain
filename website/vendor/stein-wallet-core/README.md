# The wallet core, vendored

A build of the Stein wallet core, the Rust crate in the
[stein-wallet](https://github.com/cintelis/stein-wallet) repository
(`wasm/`), and its JavaScript caller:

- `stein_wallet_core.wasm`: built from stein-wallet `55813c1` with its pinned
  toolchain (`npm run build:wasm` there). sha256
  `d7b6e7c56f771c0a3352d1d3cf3fa78846b66a40fd1e16e675a6b93d25696480`.
- `core.mjs`: `lib/stein/core.mjs` from the same commit.

This site uses two of its functions, to check what a wallet signed for it
(`src/statement.js`): verifying a statement's signature and deriving an
address from a public key. It never signs here: no seed reaches this site.

Why a vendored build and not a crate in this workspace: the chain's rule is
one FN-DSA implementation, `fn-dsa` 0.4.0, linked by stein-crypto alone
(`AGENTS.md`). The wallet core is that same implementation at that same
version, compiled for the browser, which stein-crypto cannot be (its ML-DSA
is AWS-LC). Vendoring the wallet's build adds no second implementation and
nothing to this workspace's dependency graph.

To update: build the core in stein-wallet, copy both files here, and change
the commit and hash above. The deploy checks that the served file is this one.
