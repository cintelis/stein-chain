# steinchain.com

The Stein Chain home page: static files in `public/`, served by a
Cloudflare Worker at https://steinchain.com (and `www`).

- `index.html`, `style.css`: the page. No build step, no framework.
- `app.js`: fills the hero's live figures from the devnet's public RPC
  (`/status`), the follower first and the validator as a fallback.
- `og.png`: the 1200 x 630 link-preview card (Open Graph and X). Its source is
  `og/card.html`; after changing it, run `node og/render.cjs` (Playwright with
  Chromium) and commit the new PNG.
- `_headers`: the security headers. Scripts and styles come from this origin
  only, and the page may connect only to the two devnet nodes.

What the page says about the protocol follows `docs/SPEC.md`. When a figure
there changes (transfer sizes, measured rates, the roadmap), change it here.

```sh
npm ci
npm run dev       # http://localhost:8787
npm run deploy    # by hand; pushing to main deploys it (Deploy steinchain.com)
```
