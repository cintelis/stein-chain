# steinchain.com

The Stein Chain home page: static files in `public/`, served by a
Cloudflare Worker at https://steinchain.com (and `www`), plus an anonymous,
encrypted message inbox.

- `index.html`, `style.css`: the page. No framework. `404.html`: what an
  unknown address gets.
- `app.js`: the hero's live figures and latest blocks, from the devnet's
  public RPC (`/status`, `/blocks`), the follower first and the validator as
  a fallback; and the page's motion (below).
- `vendor/anime.esm.min.js`: anime.js 4.5.0 (MIT, licence beside it), the
  animation library `app.js` imports. Vendored so the page loads only its own
  files. sha256, with LF line endings as committed: `5b578df3a20e3e2ac9ce8cf880dbdfe442f5f9812b8f8c7638cac6f628a2af7e`.
- `og.png`: the 1200 x 630 link-preview card (Open Graph and X). Its source is
  `og/card.html`; after changing it, run `node og/render.cjs` (Playwright with
  Chromium) and commit the new PNG.
- `_headers`: the security headers. Scripts and styles come from this origin
  only, and the page may connect only to itself and the two devnet nodes.
- `message.js`, `inbox.js`, `stein_wallet_core.wasm`: the message form with
  wallet registration, the inbox page (`inbox.html`, at `/inbox`), and the
  wallet core they check signatures with. Built from `src/` and `vendor/` by
  `npm run build` (`scripts/build.mjs`; wrangler runs it before `dev` and
  `deploy`); not committed.

## Motion

All of it is in `app.js`, and all of it is decoration: the page reads the
same without it.

- **Hero.** A triangular lattice (a nod to the lattice problems ML-DSA and
  FN-DSA rest on) draws in from the centre, its points flickering. Three
  violet qubits, drawn as Bloch spheres, spread interference rings and every
  few seconds send a wave at a lattice point; the lattice absorbs it with a
  green ring and a ripple. The headline rises word by word, and the figures
  count up.
- **Real blocks.** When the devnet's height grows, each new block sends a
  blue ripple and a short path across the lattice and slides into the "Latest
  blocks" row. The devnet makes a block only when there are transactions, so
  this is occasional.
- **Roadmap.** The items slide in once, and the line on the left
  fills with the scroll position; an item lights up when the line reaches its
  marker.

With `prefers-reduced-motion`, everything is shown in its end state and
nothing loops. The elements that animate in start hidden only when scripts
run and motion is allowed, and a CSS fallback shows them after a few seconds
if the script fails. SVG parts are animated through attributes (`r`,
`transform`), not CSS transforms, which would turn them about the corner of
the view box.

## The message inbox

Anyone can send a message from the page without an account. What makes it
anonymous is what is never collected:

- **Sealed in the browser.** `src/envelope.js` encrypts the text to the
  inbox's public key (`src/inbox-key.js`) with ML-KEM-768 (FIPS 203), HKDF and
  AES-256-GCM. Every envelope is 7,261 bytes whatever the text and whether or
  not a wallet signed it (`src/format.js`), so neither shows from outside.
- **Stored blind.** `src/worker.js` takes `POST /api/message` and writes the
  envelope and the UTC day to the D1 database `steinchain-inbox`. It stores no
  address, header or cookie, logs nothing (`observability` is off), and holds
  no key or other secret.
- **Limited.** Three messages a minute from one address (counted in memory at
  the edge, not stored), 500 a day in all, and only from this site's origin.
- **Not on the chain.** Nothing here touches Stein Chain.

What it cannot hide: the sender's network sees a visit to steinchain.com, and
Cloudflare sees the sender's address as it does for any site it carries. The
page says so and points to Tor Browser.

The deploy workflow's token has no rights over the database, so CI can
neither read nor delete messages. A new file in `migrations/` is therefore
applied by hand, before the push that needs it:

```sh
npx wrangler d1 migrations apply steinchain-inbox --remote
```

## Wallets: signed messages and registration

A visitor with a Stein wallet can do two more things, both by signing a
**statement** at https://wallet.steinchain.com/sign (`src/statement.js`):

- **Sign a message.** The form sends them to the wallet with the text; the
  wallet shows it, signs, and sends them back; the page checks the signature,
  then seals the text together with the signer's public key and signature.
  The inbox checks the signature again and shows the signer's address. The
  site still stores only sealed bytes: it cannot tell who signed, or that
  anyone did.
- **Register a wallet.** The wallet signs its own address; `POST
  /api/register` verifies that and stores the address with its proof in the
  table `wallets`. The list is shown in the inbox (and by `node inbox/read.mjs
  --wallets`). Three a minute from one address, 500 new ones a day.

A statement is not a transaction. The wallet core signs it over
`"STEIN-STATEMENT-v1" ‖ kind ‖ payload`, which no transaction's signed bytes
(`"STEIN-SIG-v1" ‖ …`) can equal, so it cannot be replayed on the chain, and
it is tied to its purpose: a message's signature covers this inbox's public
key and the text, a registration's covers the account's own address. The
wallet answers only to steinchain.com, whatever link brought the visitor.

Signatures are FN-DSA-512 (draft FIPS 206), made and checked by the wallet
core: `vendor/stein-wallet-core/` holds a build of it (see its README). It is
the chain's one FN-DSA implementation, `fn-dsa` 0.4.0, compiled to
WebAssembly from the stein-wallet repository; nothing here links a second
one. The Worker, the pages and the command-line reader all use that file.

To try the whole loop locally, run the wallet (`stein-wallet`, built with
`NEXT_PUBLIC_SITE_ORIGIN=http://127.0.0.1:8787`) and this site with
`WALLET_ORIGIN=<the wallet's address> npm run dev`.

### Reading it

In a browser, at https://steinchain.com/inbox. The page takes the key file
once and keeps the key in that browser only, encrypted under a passphrase
(PBKDF2-SHA-256, 600,000 rounds, AES-256-GCM); the passphrase unlocks it from
then on. Neither is ever sent. The Worker has no password to check, so the
page proves it holds the key instead (`src/proof.js`): the Worker
encapsulates a fresh secret to the inbox's public key, and hands over the
sealed messages only for the proof derived from that secret, once, within two
minutes. Without the secret key nobody gets even the sealed bytes. The page
can also delete the messages it has shown, and lists the registered wallets.

The page is as trustworthy as the deployment that serves it: whoever can
deploy this site could serve a page that takes the key. To read without
trusting the served page, use the command line, with the secret key on your
machine:

```sh
node inbox/read.mjs            # print every message
node inbox/read.mjs --purge    # print them, then delete them from the database
node inbox/read.mjs --wallets  # list the registered wallets
```

It needs wrangler to reach the account (`npx wrangler login`, or
`CLOUDFLARE_API_TOKEN` with D1 edit rights) and the secret key at
`~/.stein/inbox.key`. That file is the only copy: without it the stored
messages cannot be read by anyone. `node inbox/keygen.mjs` makes a new pair
(it never overwrites a key file); read and purge the inbox first, because
messages sealed to the old key need the old key.

What the page says about the protocol follows `docs/SPEC.md`. When a figure
there changes (transfer sizes, measured rates, the roadmap), change it here.

```sh
npm ci
npm test          # the envelope, signed messages, registration, ML-KEM against Node's
npx wrangler d1 migrations apply steinchain-inbox --local   # once, for the local inbox
npm run dev       # http://localhost:8787
npm run deploy    # by hand; pushing to main deploys it (Deploy steinchain.com)
```
