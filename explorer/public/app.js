// steinscan: a block explorer for Stein Chain nodes.
//
// Reads a node's public RPC and follows its live event stream (GET /events,
// server-sent events). Everything a node returns is untrusted input: it is
// only ever placed in the page as text (textContent), never as HTML.
"use strict";

// ---------------------------------------------------------------------------
// Networks

const NETWORKS = [
  { id: "devnet", name: "Devnet (Cloudflare follower)", url: "https://stein-follower.nick-598.workers.dev" },
  { id: "local", name: "Local validator :8645", url: "http://127.0.0.1:8645" },
  { id: "local-follower", name: "Local follower :8646", url: "http://127.0.0.1:8646" },
  { id: "fn-dsa", name: "FN-DSA test network :8655", url: "http://127.0.0.1:8655" },
];
const MAX_ROWS = 25;

const state = {
  node: null,          // base URL of the node being read
  status: null,        // last GET /status
  blocks: [],          // newest first (BlockView)
  txs: [],             // newest first (TxView), pending ones included
  events: null,        // EventSource
  statusTimer: null,
};

function storageGet(key) {
  try { return localStorage.getItem(key); } catch { return null; }
}
function storageSet(key, value) {
  try { localStorage.setItem(key, value); } catch { /* private mode */ }
}

function chooseNode() {
  const fromQuery = new URLSearchParams(location.search).get("node");
  return (fromQuery || storageGet("steinscan.node") || NETWORKS[0].url).replace(/\/+$/, "");
}

// ---------------------------------------------------------------------------
// DOM helpers (text only)

function h(tag, attrs, ...children) {
  const el = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs || {})) {
    if (v === undefined || v === null || v === false) continue;
    if (k === "class") el.className = v;
    else if (k === "text") el.textContent = v;
    else if (k.startsWith("on")) el.addEventListener(k.slice(2), v);
    else el.setAttribute(k, v === true ? "" : String(v));
  }
  for (const child of children.flat()) {
    if (child === null || child === undefined || child === false) continue;
    el.append(child instanceof Node ? child : document.createTextNode(String(child)));
  }
  return el;
}

const $ = (id) => document.getElementById(id);

function short(hash, head = 10, tail = 6) {
  if (!hash) return "";
  return hash.length <= head + tail + 1 ? hash : `${hash.slice(0, head)}…${hash.slice(-tail)}`;
}

function ago(ms) {
  if (!ms) return "";
  const s = Math.max(0, Math.round((Date.now() - ms) / 1000));
  if (s < 60) return `${s}s ago`;
  if (s < 3600) return `${Math.floor(s / 60)}m ago`;
  if (s < 86400) return `${Math.floor(s / 3600)}h ago`;
  return new Date(ms).toLocaleString();
}

function bytes(n) {
  if (n === null || n === undefined) return "—";
  return n >= 1024 ? `${(n / 1024).toFixed(1)} KB` : `${n} B`;
}

function linkTx(id, text) { return h("a", { href: `#/tx/${id}`, class: "mono" }, text || short(id)); }
function linkBlock(height) { return h("a", { href: `#/block/${height}` }, `#${height}`); }
function linkAddr(addr, text) { return h("a", { href: `#/address/${addr}`, class: "mono" }, text || short(addr, 8, 6)); }
function agoEl(ms) { return h("span", { class: "ago muted small", "data-ts": ms }, ago(ms)); }

function schemePill(scheme) {
  if (!scheme) return h("span", { class: "pill" }, "pruned");
  const experimental = scheme.includes("draft");
  return h("span", { class: experimental ? "pill warn" : "pill", title: experimental ? "Experimental: draft FIPS 206" : "FIPS 204" }, scheme);
}
function statusPill(tx) {
  return tx.height ? h("span", { class: "pill ok" }, "confirmed") : h("span", { class: "pill warn" }, "pending");
}

// ---------------------------------------------------------------------------
// RPC

async function api(path) {
  const response = await fetch(state.node + path, { headers: { accept: "application/json" } });
  if (!response.ok) {
    let message = `${response.status} ${response.statusText}`;
    try { message = (await response.json()).error || message; } catch { /* not JSON */ }
    throw new Error(message);
  }
  return response.json();
}

function banner(text, bad) {
  const el = $("banner");
  el.hidden = !text;
  el.className = bad ? "banner bad" : "banner";
  el.textContent = text || "";
}

async function refreshStatus() {
  try {
    state.status = await api("/status");
    const s = state.status;
    const experimental = (s.tx_schemes || []).some((x) => x.includes("draft"));
    if (s.sync_error) banner(`This follower stopped following: ${s.sync_error}`, true);
    else if (experimental) banner("FN-DSA TEST NETWORK: experimental draft-standard signatures. Never mainnet.");
    else banner("");
    $("node-info").textContent = `chain ${s.chain_id} · ${s.role} · genesis ${short(s.genesis_hash)}`;
    if (route().page === "home") renderStats();
  } catch (e) {
    banner(`Cannot reach ${state.node}: ${e.message}`, true);
  }
}

// ---------------------------------------------------------------------------
// Live stream

function setLive(mode, text) {
  const el = $("live");
  el.className = `live ${mode}`;
  $("live-text").textContent = text;
}

function connect() {
  if (state.events) state.events.close();
  setLive("", "connecting");
  const es = new EventSource(state.node + "/events");
  state.events = es;
  es.onopen = () => setLive("on", "live");
  es.onerror = () => setLive("off", "reconnecting");
  es.addEventListener("block", (msg) => {
    const event = parse(msg.data);
    if (event && event.type === "block") onBlock(event);
  });
  es.addEventListener("pending", (msg) => {
    const event = parse(msg.data);
    if (event && event.type === "pending") onPending(event);
  });
}

function parse(data) {
  try { return JSON.parse(data); } catch { return null; }
}

function onBlock(block) {
  if (state.blocks.some((b) => b.height === block.height)) return;
  state.blocks.unshift(block);
  state.blocks.length = Math.min(state.blocks.length, MAX_ROWS);
  for (const tx of block.txs) {
    const i = state.txs.findIndex((t) => t.id === tx.id);
    if (i >= 0) state.txs[i] = tx;
    else state.txs.unshift(tx);
  }
  state.txs.length = Math.min(state.txs.length, MAX_ROWS);
  if (state.status) {
    state.status.height = Math.max(state.status.height, block.height);
    state.status.mempool = Math.max(0, (state.status.mempool || 0) - block.tx_count);
  }
  const r = route();
  if (r.page === "home") {
    renderStats();
    renderBlocks(block.height);
    renderTxs(new Set(block.txs.map((t) => t.id)));
  } else if (r.page === "tx" && block.txs.some((t) => t.id === r.arg)) {
    render();
  } else if (r.page === "address" && block.txs.some((t) => t.sender === r.arg || t.to === r.arg)) {
    render();
  }
}

function onPending(tx) {
  if (state.txs.some((t) => t.id === tx.id)) return;
  state.txs.unshift(tx);
  state.txs.length = Math.min(state.txs.length, MAX_ROWS);
  if (state.status) state.status.mempool = (state.status.mempool || 0) + 1;
  if (route().page === "home") {
    renderStats();
    renderTxs(new Set([tx.id]));
  }
}

// ---------------------------------------------------------------------------
// Routing

function route() {
  const [, page, arg] = (location.hash || "#/").split("/");
  if (page === "block" || page === "tx" || page === "address") return { page, arg: decodeURIComponent(arg || "") };
  return { page: "home" };
}

async function render() {
  const app = $("app");
  const r = route();
  try {
    if (r.page === "home") return renderHome(app);
    app.replaceChildren(h("p", { class: "muted" }, "Loading…"));
    if (r.page === "block") return await renderBlockPage(app, r.arg);
    if (r.page === "tx") return await renderTxPage(app, r.arg);
    if (r.page === "address") return await renderAddressPage(app, r.arg);
  } catch (e) {
    app.replaceChildren(h("div", { class: "error" }, `Could not load: ${e.message}`));
  }
}

// ---------------------------------------------------------------------------
// Home

function renderHome(app) {
  app.replaceChildren(
    h("section", { id: "stats", class: "stats" }),
    h("div", { class: "cols" },
      h("section", { class: "panel" },
        h("h2", {}, "Latest blocks", h("span", { class: "hint" }, "live")),
        h("ul", { id: "blocks", class: "rows" })),
      h("section", { class: "panel" },
        h("h2", {}, "Latest transactions", h("span", { class: "hint" }, "pending → confirmed, live")),
        h("ul", { id: "txs", class: "rows" }))),
  );
  renderStats();
  renderBlocks();
  renderTxs();
}

// Transactions per second over the last minute of wall-clock time. Blocks are
// only produced when there is something to include, so averaging over blocks
// alone would freeze at the last burst.
const TPS_WINDOW_MS = 60_000;
function tps() {
  const since = Date.now() - TPS_WINDOW_MS;
  const count = state.blocks.filter((b) => b.timestamp_ms >= since).reduce((n, b) => n + b.tx_count, 0);
  return count / (TPS_WINDOW_MS / 1000);
}

function stat(label, value, sub) {
  return h("div", { class: "stat" },
    h("div", { class: "label" }, label),
    h("div", { class: "value" }, value),
    sub ? h("div", { class: "sub" }, sub) : null);
}

function renderStats() {
  const el = $("stats");
  if (!el) return;
  const s = state.status;
  if (!s) { el.replaceChildren(stat("Network", "…")); return; }
  const rate = tps();
  const sized = state.txs.filter((t) => t.size);
  const avg = sized.length ? Math.round(sized.reduce((n, t) => n + t.size, 0) / sized.length) : null;
  el.replaceChildren(
    stat("Block height", s.height.toLocaleString(), state.blocks[0] ? ago(state.blocks[0].timestamp_ms) : "no blocks yet"),
    stat("Transactions / s", rate.toFixed(2), "last 60 seconds"),
    stat("Mempool", String(s.mempool), "waiting for a block"),
    stat("Avg. tx size", avg === null ? "—" : bytes(avg), "recent transactions"),
    stat("Chain", String(s.chain_id), `${s.role}${s.leader ? " of " + new URL(s.leader).host : ""}`),
    stat("Signature schemes", (s.tx_schemes || []).length ? String(s.tx_schemes.length) : "—", (s.tx_schemes || []).join(", ")),
  );
}

function renderBlocks(flashHeight) {
  const el = $("blocks");
  if (!el) return;
  if (!state.blocks.length) { el.replaceChildren(h("li", { class: "empty" }, "No blocks yet. They appear here as they are produced.")); return; }
  el.replaceChildren(...state.blocks.map((b) =>
    h("li", { class: b.height === flashHeight ? "row flash" : "row" },
      h("div", { class: "badge-icon" }, "Bk"),
      h("div", { class: "main" },
        h("div", {}, linkBlock(b.height), " ", agoEl(b.timestamp_ms)),
        h("div", { class: "small muted" }, "proposer ", linkAddr(b.proposer))),
      h("div", { class: "right" },
        h("div", {}, `${b.tx_count} tx${b.tx_count === 1 ? "" : "s"}`),
        h("div", { class: "small muted" }, bytes(b.size))))));
}

function renderTxs(flashIds) {
  const el = $("txs");
  if (!el) return;
  if (!state.txs.length) { el.replaceChildren(h("li", { class: "empty" }, "No transactions yet. Send one with the stein wallet.")); return; }
  el.replaceChildren(...state.txs.map((t) =>
    h("li", { class: flashIds && flashIds.has(t.id) ? "row flash" : "row" },
      h("div", { class: "badge-icon" }, "Tx"),
      h("div", { class: "main" },
        h("div", {}, linkTx(t.id), " ", statusPill(t)),
        h("div", { class: "small muted" }, "from ", linkAddr(t.sender), " to ", linkAddr(t.to))),
      h("div", { class: "right" },
        h("div", {}, `${t.amount_stein} STEIN`),
        h("div", { class: "small" }, schemePill(t.scheme), " ", h("span", { class: "muted" }, bytes(t.size)))))));
}

// ---------------------------------------------------------------------------
// Detail pages

function kv(rows) {
  return h("div", { class: "panel" },
    h("div", { class: "kv" }, rows.flatMap(([k, v]) => [h("div", { class: "k" }, k), h("div", {}, v)])));
}

function txTable(txs, { showBlock = true } = {}) {
  if (!txs.length) return h("div", { class: "empty" }, "No transactions.");
  return h("div", { class: "table-scroll" },
    h("table", {},
      h("thead", {}, h("tr", {}, ["Transaction", showBlock ? "Block" : null, "From", "To", "Amount", "Fee", "Scheme", "Size"].filter(Boolean).map((t) => h("th", {}, t)))),
      h("tbody", {}, txs.map((t) => h("tr", {},
        h("td", {}, linkTx(t.id)),
        showBlock ? h("td", {}, t.height ? linkBlock(t.height) : statusPill(t)) : null,
        h("td", {}, linkAddr(t.sender)),
        h("td", {}, linkAddr(t.to)),
        h("td", {}, `${t.amount_stein} STEIN`),
        h("td", { class: "muted" }, t.fee_stein),
        h("td", {}, schemePill(t.scheme)),
        h("td", { class: "muted" }, bytes(t.size)))))));
}

async function renderBlockPage(app, arg) {
  const height = Number(arg);
  if (!Number.isInteger(height) || height < 1) throw new Error("not a block height");
  const [block] = await api(`/blocks?before=${height}&limit=1`);
  if (!block || block.height !== height) throw new Error(`block ${height} does not exist yet`);
  app.replaceChildren(
    h("h1", { class: "page-title" }, `Block #${block.height}`),
    kv([
      ["Block hash", h("span", { class: "mono" }, block.hash)],
      ["Timestamp", `${new Date(block.timestamp_ms).toLocaleString()} (${ago(block.timestamp_ms)})`],
      ["Proposer", linkAddr(block.proposer, block.proposer)],
      ["Transactions", String(block.tx_count)],
      ["Size", bytes(block.size)],
      ["Parent", block.height > 1 ? h("a", { href: `#/block/${block.height - 1}`, class: "mono" }, block.parent) : h("span", { class: "mono" }, block.parent + " (genesis)")],
      ["State root", h("span", { class: "mono" }, block.state_root)],
      ["Signatures", block.witnesses_available ? h("span", { class: "pill ok" }, "retained") : h("span", { class: "pill" }, "pruned after finality (SPEC §5.5)")],
    ]),
    h("section", { class: "panel section" }, h("h2", {}, "Transactions"), txTable(block.txs, { showBlock: false })),
    h("p", { class: "small" },
      block.height > 1 ? h("a", { href: `#/block/${block.height - 1}` }, "← previous") : null, "  ",
      h("a", { href: `#/block/${block.height + 1}` }, "next →")),
  );
}

async function renderTxPage(app, id) {
  if (!/^[0-9a-f]{64}$/.test(id)) throw new Error("not a transaction id");
  const result = await api(`/tx/${id}`);
  if (result.status === "unknown") throw new Error("this node does not know that transaction");
  const t = result.tx;
  app.replaceChildren(
    h("h1", { class: "page-title" }, "Transaction", statusPill(t), h("span", { class: "mono" }, t.id)),
    kv([
      ["Status", t.height ? h("span", {}, "Confirmed in block ", linkBlock(t.height), ` at position ${t.index}`) : "Pending: waiting for a block (this page updates live)"],
      ["From", linkAddr(t.sender, t.sender)],
      ["To", linkAddr(t.to, t.to)],
      ["Amount", `${t.amount_stein} STEIN`],
      ["Fee", `${t.fee_stein} STEIN`],
      ["Nonce", String(t.nonce)],
      ["Signature scheme", schemePill(t.scheme)],
      ["Registers sender's key", t.registers_key === null ? "— (signature pruned)" : t.registers_key ? "yes (first transaction from this account)" : "no"],
      ["Size", bytes(t.size)],
      ["Valid until block", String(t.expires_at)],
    ]),
  );
}

async function renderAddressPage(app, address) {
  if (!/^st[0-9a-f]{72}$/.test(address)) throw new Error("not a Stein address");
  const [account, txs] = await Promise.all([api(`/account/${address}`), api(`/account/${address}/txs?limit=50`)]);
  const scheme = txs.find((t) => t.sender === address && t.scheme);
  app.replaceChildren(
    h("h1", { class: "page-title" }, "Address", h("span", { class: "mono" }, address)),
    kv([
      ["Balance", h("strong", {}, `${account.balance_stein} STEIN`)],
      ["Nonce (transactions sent)", String(account.nonce)],
      ["Public key on chain", account.key_registered ? h("span", { class: "pill ok" }, "registered") : h("span", { class: "pill" }, "not yet: registered by its first transaction")],
      ["Signature scheme", scheme ? schemePill(scheme.scheme) : h("span", { class: "muted" }, "unknown until it sends")],
    ]),
    h("section", { class: "panel section" }, h("h2", {}, "Transactions", h("span", { class: "hint" }, "newest first")), txTable(txs)),
  );
}

// ---------------------------------------------------------------------------
// Search, network switch, start-up

function search(event) {
  event.preventDefault();
  const q = $("search-input").value.trim().toLowerCase();
  if (!q) return;
  if (/^\d+$/.test(q)) location.hash = `#/block/${q}`;
  else if (/^st[0-9a-f]{72}$/.test(q)) location.hash = `#/address/${q}`;
  else if (/^(0x)?[0-9a-f]{64}$/.test(q)) location.hash = `#/tx/${q.replace(/^0x/, "")}`;
  else $("app").prepend(h("div", { class: "error" }, "Enter a block height, a transaction id (64 hex) or an address (st…)."));
}

function setupNetworks() {
  const select = $("node-select");
  const known = NETWORKS.some((n) => n.url === state.node);
  const options = NETWORKS.map((n) => h("option", { value: n.url, selected: n.url === state.node }, n.name));
  if (!known) options.push(h("option", { value: state.node, selected: true }, state.node));
  options.push(h("option", { value: "custom" }, "Custom node URL…"));
  select.replaceChildren(...options);
  select.addEventListener("change", () => {
    let url = select.value;
    if (url === "custom") {
      url = (prompt("Node RPC URL (https://… or http://127.0.0.1:port)") || "").trim();
      if (!/^https?:\/\/[^\s]+$/.test(url)) { select.value = state.node; return; }
    }
    switchNode(url.replace(/\/+$/, ""));
  });
}

async function switchNode(url) {
  state.node = url;
  state.blocks = [];
  state.txs = [];
  state.status = null;
  storageSet("steinscan.node", url);
  setupNetworks();
  connect();
  await loadInitial();
  render();
}

async function loadInitial() {
  await refreshStatus();
  try {
    state.blocks = await api(`/blocks?limit=${MAX_ROWS}`);
    const pending = await api(`/mempool?limit=${MAX_ROWS}`);
    state.txs = [...pending, ...state.blocks.flatMap((b) => b.txs)].slice(0, MAX_ROWS);
  } catch { /* banner already shows the problem */ }
}

setInterval(() => {
  for (const el of document.querySelectorAll(".ago")) el.textContent = ago(Number(el.dataset.ts));
}, 1000);
// The per-minute rate decays with no new blocks: refresh it.
setInterval(() => { if (route().page === "home") renderStats(); }, 5000);

$("search").addEventListener("submit", search);
window.addEventListener("hashchange", render);
state.node = chooseNode();
setupNetworks();
connect();
state.statusTimer = setInterval(refreshStatus, 10000);
loadInitial().then(render);
