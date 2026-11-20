# steinchain.com

The Stein Chain home page: static files in `public/`, served by a
Cloudflare Worker at https://steinchain.com (and `www`), plus an anonymous,
encrypted message inbox.

- `index.html`, `style.css`: the page. No framework.
- `app.js`: fills the hero's live figures from the devnet's public RPC
  (`/status`), the follower first and the validator as a fallback.
- `og.png`: the 1200 x 630 link-preview card (Open Graph and X). Its source is
  `og/card.html`; after changing it, run `node og/render.cjs` (Playwright with
  Chromium) and commit the new PNG.
- `_headers`: the security headers. Scripts and styles come from this origin
  only, and the page may connect only to itself and the two devnet nodes.
- `message.js`: the message form. Built from `src/message.js` by `npm run
  build` (wrangler runs it before `dev` and `deploy`); not committed.

## The message inbox

Anyone can send a message from the page without an account. What makes it
anonymous is what is never collected:

- **Sealed in the browser.** `src/envelope.js` encrypts the text to the
  inbox's public key (`src/inbox-key.js`) with ML-KEM-768 (FIPS 203), HKDF and
  AES-256-GCM. Every envelope is 5,213 bytes whatever the text (`src/format.js`),
  so its size says nothing.
- **Stored blind.** `src/worker.js` takes `POST /api/message` and writes the
  envelope and the UTC day to the D1 database `steinchain-inbox`. It stores no
  address, header or cookie, logs nothing (`observability` is off), and holds
  no key. It has no endpoint that reads the inbox.
- **Limited.** Three messages a minute from one address (counted in memory at
  the edge, not stored), 500 a day in all, and only from this site's origin.
- **Not on the chain.** Nothing here touches Stein Chain.

What it cannot hide: the sender's network sees a visit to steinchain.com, and
Cloudflare sees the sender's address as it does for any site it carries. The
page says so and points to Tor Browser.

Reading it happens on your machine, with the secret key:

```sh
node inbox/read.mjs            # print every message
node inbox/read.mjs --purge    # print them, then delete them from the database
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
npm test          # the envelope: round trip, fixed size, tampering, ML-KEM against Node's
npx wrangler d1 migrations apply steinchain-inbox --local   # once, for the local inbox
npm run dev       # http://localhost:8787
npm run deploy    # by hand; pushing to main deploys it (Deploy steinchain.com)
```
