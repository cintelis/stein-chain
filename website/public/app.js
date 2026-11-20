// steinchain.com: the live figures in the hero, read from the devnet's public
// RPC. The follower first, the validator if the follower does not answer.
"use strict";

const NODES = ["https://stein-follower.nick-598.workers.dev", "https://rpc.steinchain.com"];
const REFRESH_MS = 10000;

const $ = (id) => document.getElementById(id);
const number = (n) => Number(n).toLocaleString("en-US");

async function status() {
  let last = new Error("no node");
  for (const node of NODES) {
    try {
      const response = await fetch(node + "/status", {
        headers: { accept: "application/json" },
        signal: AbortSignal.timeout(8000),
      });
      if (!response.ok) throw new Error(`HTTP ${response.status}`);
      const s = await response.json();
      // A follower that has stopped following shows a stale chain.
      if (s.sync_error) throw new Error("follower out of sync");
      return s;
    } catch (e) {
      last = e;
    }
  }
  throw last;
}

async function refresh() {
  const state = $("live-state");
  try {
    const s = await status();
    $("s-height").textContent = number(s.height);
    $("s-txs").textContent = number(s.tx_total);
    $("s-accounts").textContent = number(s.accounts);
    $("s-chain").textContent = String(s.chain_id);
    $("s-schemes").textContent = (s.tx_schemes || []).join(" · ");
    state.textContent = "● online";
    state.className = "state ok";
  } catch {
    state.textContent = "unreachable";
    state.className = "state bad";
  }
}

refresh();
setInterval(() => {
  if (document.visibilityState === "visible") refresh();
}, REFRESH_MS);
document.addEventListener("visibilitychange", () => {
  if (document.visibilityState === "visible") refresh();
});
