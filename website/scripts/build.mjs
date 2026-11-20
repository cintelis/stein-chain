// Builds what the pages load and git does not hold: the two scripts
// (public/message.js, public/inbox.js) and a copy of the wallet core they
// verify signatures with. wrangler runs this before `dev` and `deploy`.
//
// For local development against a local wallet, point the form at it:
//   WALLET_ORIGIN=http://localhost:3000 npm run build
import { copyFileSync } from "node:fs";
import { build } from "esbuild";

const wallet = process.env.WALLET_ORIGIN;
await build({
  entryPoints: ["src/message.js", "src/inbox.js"],
  bundle: true,
  format: "iife",
  target: "es2022",
  minify: true,
  legalComments: "none",
  outdir: "public",
  define: wallet ? { WALLET_ORIGIN: JSON.stringify(wallet) } : {},
  logLevel: "info",
});
copyFileSync("vendor/stein-wallet-core/stein_wallet_core.wasm", "public/stein_wallet_core.wasm");
