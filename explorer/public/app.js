// steinscan: a block explorer for Stein Chain nodes.
//
// Reads a node's public RPC and follows its live event stream (GET /events,
// server-sent events). Everything a node returns is untrusted input: it is
// only ever placed in the page as text (textContent), never as HTML.
"use strict";

// ---------------------------------------------------------------------------
// Networks and state

const NETWORKS = [
  { name: "Devnet (Cloudflare follower)", url: "https://stein-follower.nick-598.workers.dev" },
  { name: "Devnet validator (rpc.steinchain.com)", url: "https://rpc.steinchain.com" },
  { name: "Local validator :8645", url: "http://127.0.0.1:8645" },
  { name: "Local follower :8646", url: "http://127.0.0.1:8646" },
  { name: "FN-DSA test network :8655", url: "http://127.0.0.1:8655" },
];
const HOME_ROWS = 6;      // rows in the home page lists
const SPARK_BLOCKS = 50;  // blocks in the overview chart
const PAGE_BLOCKS = 25;   // blocks per page
const PAGE_TXS = 50;      // transactions per page (whole blocks)
const TPS_WINDOW_MS = 60_000;

const state = {
  node: null,
  status: null,
  blocks: [],   // newest first, up to SPARK_BLOCKS
  pending: [],  // pending transactions seen live
  events: null,
};

function storageGet(key) { try { return localStorage.getItem(key); } catch { return null; } }
function storageSet(key, value) { try { localStorage.setItem(key, value); } catch { /* private mode */ } }

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
    else if (k.startsWith("on")) el.addEventListener(k.slice(2), v);
    else el.setAttribute(k, v === true ? "" : String(v));
  }
  for (const child of children.flat()) {
    if (child === null || child === undefined || child === false) continue;
    el.append(child instanceof Node ? child : document.createTextNode(String(child)));
  }
  return el;
}
function svg(tag, attrs) {
  const el = document.createElementNS("http://www.w3.org/2000/svg", tag);
  for (const [k, v] of Object.entries(attrs || {})) el.setAttribute(k, String(v));
  return el;
}
const $ = (id) => document.getElementById(id);

// List icons, drawn here (no icon font: the CSP allows nothing external).
// A cube for blocks, a document for transactions.
const ICONS = {
  block: ["M12 2.6 20.4 7.2v9.6L12 21.4 3.6 16.8V7.2Z", "M3.6 7.2 12 11.8l8.4-4.6", "M12 11.8v9.6"],
  tx: ["M6.2 2.6h8.1l4.5 4.5v14.3H6.2Z", "M14.3 2.6v4.5h4.5", "M9 12.2h6.8", "M9 15.4h6.8", "M9 18.6h4.2"],
};
function icon(kind) {
  const el = svg("svg", { viewBox: "0 0 24 24", width: 20, height: 20, fill: "none", stroke: "currentColor",
    "stroke-width": 1.6, "stroke-linejoin": "round", "stroke-linecap": "round", "aria-hidden": "true" });
  for (const d of ICONS[kind]) el.append(svg("path", { d }));
  return el;
}

function short(hash, head = 10, tail = 8) {
  if (!hash) return "";
  return hash.length <= head + tail + 1 ? hash : `${hash.slice(0, head)}…${hash.slice(-tail)}`;
}
function ago(ms) {
  if (!ms) return "";
  const s = Math.max(0, Math.round((Date.now() - ms) / 1000));
  if (s < 60) return `${s} sec${s === 1 ? "" : "s"} ago`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m} min${m === 1 ? "" : "s"} ago`;
  const hr = Math.floor(m / 60);
  if (hr < 24) return `${hr} hr${hr === 1 ? "" : "s"} ${m % 60} min${m % 60 === 1 ? "" : "s"} ago`;
  const d = Math.floor(hr / 24);
  return `${d} day${d === 1 ? "" : "s"} ago`;
}
function utc(ms) {
  const d = new Date(ms);
  const mon = d.toLocaleString("en-US", { month: "short", timeZone: "UTC" });
  const pad = (n) => String(n).padStart(2, "0");
  return `${mon}-${pad(d.getUTCDate())}-${d.getUTCFullYear()} ${pad(d.getUTCHours())}:${pad(d.getUTCMinutes())}:${pad(d.getUTCSeconds())} UTC`;
}
function bytes(n) {
  if (n === null || n === undefined) return "—";
  return `${n.toLocaleString()} bytes`;
}
function num(n) { return Number(n).toLocaleString(); }

function linkTx(id, full) { return h("a", { href: `#/tx/${id}`, class: "mono" }, full ? id : short(id)); }
function linkBlock(height) { return h("a", { href: `#/block/${height}` }, String(height)); }
function linkAddr(addr, full) { return h("a", { href: `#/address/${addr}`, class: "mono", title: addr }, full ? addr : short(addr, 9, 7)); }
function agoEl(ms) { return h("span", { class: "ago", "data-ts": ms }, ago(ms)); }
function help(text) { return h("span", { class: "help", title: text, "aria-label": text }, "?"); }
function copyBtn(text) {
  return h("button", {
    type: "button", class: "copy", title: "Copy", "aria-label": "Copy",
    onclick: (e) => {
      const btn = e.currentTarget;
      navigator.clipboard?.writeText(text).then(() => {
        btn.classList.add("done"); btn.textContent = "✓";
        setTimeout(() => { btn.classList.remove("done"); btn.textContent = "⧉"; }, 1200);
      });
    },
  }, "⧉");
}
function schemePill(scheme) {
  if (!scheme) return h("span", { class: "pill", title: "Signature pruned after finality" }, "pruned");
  const experimental = scheme.includes("draft");
  return h("span", { class: experimental ? "pill warn" : "pill info", title: experimental ? "Experimental: draft FIPS 206" : "FIPS 204 post-quantum signature" }, scheme);
}
function statusPill(tx) {
  return tx.height ? h("span", { class: "pill ok" }, "✓ Success") : h("span", { class: "pill warn" }, "◷ Pending");
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
    const fee = Number(s.min_fee_per_byte || 0);
    $("strip-info").replaceChildren(
      "STEIN ", h("b", {}, "devnet"), " · Fee: ", h("b", {}, `${fee} base units/byte`),
      " · Block time: ", h("b", {}, `${(s.block_time_ms || 1000) / 1000} s`),
      " · Chain ", h("b", {}, String(s.chain_id)));
    $("foot-node").textContent = state.node;
    $("foot-chain").textContent = `chain ${s.chain_id} · ${s.role} · genesis ${short(s.genesis_hash)}`;
    if (route().page === "home") renderOverview();
  } catch (e) {
    banner(`Cannot reach ${state.node}: ${e.message}`, true);
  }
}

// ---------------------------------------------------------------------------
// Live stream

function setLive(mode, text) { $("live").className = `live ${mode}`; $("live-text").textContent = text; }
function parse(data) { try { return JSON.parse(data); } catch { return null; } }

function connect() {
  if (state.events) state.events.close();
  setLive("", "connecting");
  const es = new EventSource(state.node + "/events");
  state.events = es;
  es.onopen = () => setLive("on", "live");
  es.onerror = () => setLive("off", "reconnecting");
  es.addEventListener("block", (m) => { const e = parse(m.data); if (e && e.type === "block") onBlock(e); });
  es.addEventListener("pending", (m) => { const e = parse(m.data); if (e && e.type === "pending") onPending(e); });
}

function onBlock(block) {
  if (state.blocks.some((b) => b.height === block.height)) return;
  state.blocks.unshift(block);
  state.blocks.length = Math.min(state.blocks.length, SPARK_BLOCKS);
  const included = new Set(block.txs.map((t) => t.id));
  state.pending = state.pending.filter((t) => !included.has(t.id));
  if (state.status) {
    state.status.height = Math.max(state.status.height, block.height);
    if (state.status.tx_total !== undefined) state.status.tx_total += block.tx_count;
  }
  const r = route();
  if (r.page === "home") { renderOverview(); renderLatestBlocks(block.height); renderLatestTxs(included); }
  else if (r.page === "pending") render();
  else if ((r.page === "blocks" || r.page === "txs") && !r.query.get("before")) render();
  else if (r.page === "tx" && included.has(r.arg)) render();
  else if (r.page === "address" && block.txs.some((t) => t.sender === r.arg || t.to === r.arg)) render();
}

function onPending(tx) {
  if (state.pending.some((t) => t.id === tx.id)) return;
  state.pending.unshift(tx);
  state.pending.length = Math.min(state.pending.length, 100);
  const r = route();
  if (r.page === "home") renderLatestTxs(new Set([tx.id]));
  else if (r.page === "pending") render();
}

// ---------------------------------------------------------------------------
// Routing

function route() {
  const raw = (location.hash || "#/").slice(1);
  const [path, qs] = raw.split("?");
  const [, page, arg] = path.split("/");
  const query = new URLSearchParams(qs || "");
  const pages = ["block", "tx", "address", "blocks", "txs", "pending", "accounts", "validators"];
  return pages.includes(page) ? { page, arg: decodeURIComponent(arg || ""), query } : { page: "home", query };
}

async function render() {
  const app = $("app");
  const r = route();
  $("nav-search").hidden = r.page === "home";
  try {
    if (r.page === "home") return renderHome(app);
    if (!app.dataset.page || app.dataset.page !== r.page + r.arg) {
      app.replaceChildren(h("div", { class: "wrap page" }, h("p", { class: "muted" }, "Loading…")));
    }
    app.dataset.page = r.page + r.arg;
    const view = {
      block: renderBlockPage, tx: renderTxPage, address: renderAddressPage, blocks: renderBlocksPage,
      txs: renderTxsPage, pending: renderPendingPage, accounts: renderAccountsPage, validators: renderValidatorsPage,
    }[r.page];
    const content = await view(r.arg, r.query);
    app.replaceChildren(h("div", { class: "wrap page" }, content));
  } catch (e) {
    app.replaceChildren(h("div", { class: "wrap page" }, h("div", { class: "error" }, `Could not load: ${e.message}`)));
  }
}

// ---------------------------------------------------------------------------
// Home

function heroSearch() {
  const select = h("select", { "aria-label": "Search filter" },
    h("option", { value: "all" }, "All Filters"), h("option", { value: "address" }, "Addresses"),
    h("option", { value: "tx" }, "Txn Hash"), h("option", { value: "block" }, "Block"));
  const input = h("input", { type: "search", autocomplete: "off", spellcheck: "false", "aria-label": "Search",
    placeholder: "Search by Address / Txn Hash / Block" });
  return h("form", { class: "hero-search", role: "search", onsubmit: (e) => { e.preventDefault(); go(input.value, select.value); } },
    select, input, h("button", { type: "submit", "aria-label": "Search" }, "⌕"));
}

function renderHome(app) {
  app.dataset.page = "home";
  app.replaceChildren(
    h("section", { class: "hero" }, h("div", { class: "wrap" },
      h("h1", {}, "The Stein Chain Explorer"),
      heroSearch(),
      h("div", { class: "hero-note" }, "Post-quantum: every signature is ",
        h("span", { class: "chip" }, "ML-DSA · FIPS 204"), " and every block is re-verified by every node."))),
    h("div", { class: "wrap overlap" },
      h("section", { id: "overview", class: "card overview" }),
      h("div", { class: "two" },
        h("section", { class: "card" }, h("div", { class: "card-head" }, "Latest Blocks", h("span", { class: "pill ok small" }, "● live")),
          h("div", { id: "latest-blocks" }), h("a", { class: "card-foot", href: "#/blocks" }, "View all blocks →")),
        h("section", { class: "card" }, h("div", { class: "card-head" }, "Latest Transactions", h("span", { class: "pill ok small" }, "● live")),
          h("div", { id: "latest-txs" }), h("a", { class: "card-foot", href: "#/txs" }, "View all transactions →")))),
    h("div", { class: "spacer" }),
  );
  renderOverview();
  renderLatestBlocks();
  renderLatestTxs();
}

function tps() {
  const since = Date.now() - TPS_WINDOW_MS;
  const count = state.blocks.filter((b) => b.timestamp_ms >= since).reduce((n, b) => n + b.tx_count, 0);
  return count / (TPS_WINDOW_MS / 1000);
}

function ovItem(icon, label, ...value) {
  return h("div", { class: "ov-item" }, h("div", { class: "ov-icon", "aria-hidden": "true" }, icon),
    h("div", {}, h("div", { class: "ov-label" }, label), h("div", { class: "ov-value" }, ...value)));
}

function sparkline(blocks) {
  const data = blocks.slice(0, SPARK_BLOCKS).reverse();
  const W = 300, H = 80, n = Math.max(data.length, 1);
  const max = Math.max(1, ...data.map((b) => b.tx_count));
  const bw = W / SPARK_BLOCKS;
  const chart = svg("svg", { class: "spark", viewBox: `0 0 ${W} ${H + 2}`, preserveAspectRatio: "none", role: "img",
    "aria-label": `Transactions per block over the last ${data.length} blocks` });
  chart.append(svg("line", { class: "axis", x1: 0, y1: H + 1, x2: W, y2: H + 1 }));
  data.forEach((b, i) => {
    const hgt = Math.max(1.5, (b.tx_count / max) * H);
    const rect = svg("rect", { x: (SPARK_BLOCKS - n + i) * bw + 1, y: H - hgt, width: Math.max(1, bw - 2), height: hgt, rx: 1 });
    const title = svg("title", {});
    title.textContent = `Block ${b.height}: ${b.tx_count} txns`;
    rect.append(title);
    chart.append(rect);
  });
  return chart;
}

function renderOverview() {
  const el = $("overview");
  if (!el) return;
  const s = state.status;
  if (!s) { el.replaceChildren(h("div", { class: "ov-col muted" }, "Connecting to the node…")); return; }
  const latest = state.blocks[0];
  el.replaceChildren(
    h("div", { class: "ov-col" },
      ovItem("Σ", "STEIN supply", s.total_supply_stein ? `${num(Number(s.total_supply_stein))} STEIN` : "—"),
      ovItem("◎", "Accounts", s.accounts ? num(s.accounts) : "—")),
    h("div", { class: "ov-col" },
      ovItem("⇄", "Transactions", s.tx_total === undefined ? "—" : num(s.tx_total), " ", h("span", { class: "small muted" }, `(${tps().toFixed(2)} TPS)`)),
      ovItem("▣", "Last finalized block", num(s.height), " ", h("span", { class: "small muted" }, latest ? `(${ago(latest.timestamp_ms)})` : ""))),
    h("div", { class: "ov-col" },
      h("div", { class: "spark-title" }, `Transactions per block · last ${Math.min(state.blocks.length, SPARK_BLOCKS)} blocks`),
      state.blocks.length ? sparkline(state.blocks) : h("div", { class: "muted small" }, "No blocks yet.")),
  );
}

function renderLatestBlocks(flash) {
  const el = $("latest-blocks");
  if (!el) return;
  const blocks = state.blocks.slice(0, HOME_ROWS);
  if (!blocks.length) { el.replaceChildren(h("div", { class: "empty" }, "No blocks yet. They appear here as they are produced.")); return; }
  el.replaceChildren(...blocks.map((b, i) => {
    const prev = state.blocks[i + 1];
    const secs = prev ? Math.max(0, Math.round((b.timestamp_ms - prev.timestamp_ms) / 1000)) : null;
    return h("div", { class: b.height === flash ? "list-row flash" : "list-row" },
      h("div", { class: "lr-first" }, h("div", { class: "sq", title: "Block" }, icon("block")),
        h("div", {}, h("div", {}, linkBlock(b.height)), h("div", { class: "small muted" }, agoEl(b.timestamp_ms)))),
      h("div", { class: "lr-mid" },
        h("div", {}, "Validated By ", linkAddr(b.proposer)),
        h("div", { class: "small muted" }, h("a", { href: `#/block/${b.height}` }, `${b.tx_count} txn${b.tx_count === 1 ? "" : "s"}`),
          secs === null ? "" : ` in ${secs} sec${secs === 1 ? "" : "s"}`)),
      h("div", {}, b.fees_stein === undefined ? null : h("span", { class: "amount-box", title: "Fees collected by the proposer" }, `${b.fees_stein} STEIN`)));
  }));
}

function latestTxs() {
  const confirmed = state.blocks.flatMap((b) => b.txs.map((t) => ({ ...t, ts: b.timestamp_ms })));
  return [...state.pending.map((t) => ({ ...t, ts: null })), ...confirmed].slice(0, HOME_ROWS);
}

function renderLatestTxs(flash) {
  const el = $("latest-txs");
  if (!el) return;
  const txs = latestTxs();
  if (!txs.length) { el.replaceChildren(h("div", { class: "empty" }, "No transactions yet. Send one with the stein wallet.")); return; }
  el.replaceChildren(...txs.map((t) =>
    h("div", { class: flash && flash.has(t.id) ? "list-row flash" : "list-row" },
      h("div", { class: "lr-first" }, h("div", { class: "sq", title: "Transaction" }, icon("tx")),
        h("div", {}, h("div", {}, linkTx(t.id)), h("div", { class: "small muted" }, t.ts ? agoEl(t.ts) : statusPill(t)))),
      h("div", { class: "lr-mid" },
        h("div", {}, "From ", linkAddr(t.sender)),
        h("div", {}, "To ", linkAddr(t.to))),
      h("div", {}, h("span", { class: "amount-box" }, `${t.amount_stein} STEIN`)))));
}

// ---------------------------------------------------------------------------
// Tables and pagination

function table(headers, rows) {
  if (!rows.length) return h("div", { class: "empty" }, "Nothing to show yet.");
  return h("div", { class: "table-scroll" }, h("table", {},
    h("thead", {}, h("tr", {}, headers.map((x) => h("th", {}, x)))),
    h("tbody", {}, rows.map((cells) => h("tr", {}, cells.map((c) => h("td", {}, c)))))));
}

function txRows(txs, { block = true } = {}) {
  return txs.map((t) => [
    linkTx(t.id),
    h("span", { class: "method" }, "Transfer"),
    block ? (t.height ? linkBlock(t.height) : statusPill(t)) : null,
    t.ts ? agoEl(t.ts) : h("span", { class: "muted" }, "—"),
    linkAddr(t.sender),
    h("span", { class: "arrow", "aria-label": "to" }, "→"),
    linkAddr(t.to),
    `${t.amount_stein} STEIN`,
    h("span", { class: "muted small" }, t.fee_stein),
    schemePill(t.scheme),
  ].filter((c) => c !== null));
}
const TX_HEADERS = (block = true) => ["Transaction Hash", "Method", block ? "Block" : null, "Age", "From", "", "To", "Amount", "Txn Fee", "Signature"].filter((x) => x !== null);

function pager(links) {
  return h("div", { class: "pager" }, links.map(([label, href]) => href ? h("a", { href }, label) : h("span", {}, label)));
}

// ---------------------------------------------------------------------------
// Pages

async function renderBlocksPage(_, query) {
  const tip = state.status ? state.status.height : (await api("/status")).height;
  const before = Math.min(Number(query.get("before")) || tip, tip);
  const blocks = before > 0 ? await api(`/blocks?before=${before}&limit=${PAGE_BLOCKS}`) : [];
  const pages = Math.max(1, Math.ceil(tip / PAGE_BLOCKS));
  const page = Math.floor((tip - before) / PAGE_BLOCKS) + 1;
  const nav = pager([
    ["First", page > 1 ? "#/blocks" : null],
    ["‹", page > 1 ? `#/blocks?before=${Math.min(tip, before + PAGE_BLOCKS)}` : null],
    [`Page ${page} of ${pages}`, null],
    ["›", before - PAGE_BLOCKS > 0 ? `#/blocks?before=${before - PAGE_BLOCKS}` : null],
    ["Last", page < pages ? `#/blocks?before=${((tip - 1) % PAGE_BLOCKS) + 1}` : null],
  ]);
  return [
    h("h1", { class: "page-title" }, "Blocks"),
    h("section", { class: "card" },
      h("div", { class: "card-head" }, h("span", { class: "muted small" }, `Total of ${num(tip)} blocks`), nav),
      table(["Block", "Age", "Txn", "Validator", "Size", "Fees"], blocks.map((b) => [
        linkBlock(b.height), agoEl(b.timestamp_ms),
        h("a", { href: `#/block/${b.height}` }, String(b.tx_count)),
        linkAddr(b.proposer), b.size === null ? "—" : bytes(b.size), b.fees_stein === undefined ? "—" : `${b.fees_stein} STEIN`])),
      h("div", { class: "card-head" }, h("span", {}), nav)),
  ];
}

async function renderTxsPage(_, query) {
  const tip = state.status ? state.status.height : (await api("/status")).height;
  const before = Math.min(Number(query.get("before")) || tip, tip);
  const txs = [];
  let cursor = before, last = before;
  while (cursor > 0 && txs.length < PAGE_TXS) {
    const blocks = await api(`/blocks?before=${cursor}&limit=100`);
    if (!blocks.length) break;
    for (const b of blocks) {
      if (txs.length >= PAGE_TXS) break;
      txs.push(...b.txs.map((t) => ({ ...t, ts: b.timestamp_ms })));
      last = b.height;
    }
    cursor = blocks[blocks.length - 1].height - 1;
  }
  const next = last - 1;
  const nav = pager([
    ["First", before < tip ? "#/txs" : null],
    ["Older ›", next > 0 && txs.length ? `#/txs?before=${next}` : null],
  ]);
  const total = state.status ? state.status.tx_total : null;
  return [
    h("h1", { class: "page-title" }, "Transactions"),
    h("section", { class: "card" },
      h("div", { class: "card-head" },
        h("span", { class: "muted small" }, total === null ? "" : `${num(total)} transactions found`,
          txs.length ? ` · showing blocks ${num(last)}–${num(before)}` : ""), nav),
      table(TX_HEADERS(), txRows(txs)),
      h("div", { class: "card-head" }, h("span", {}), nav)),
  ];
}

async function renderPendingPage() {
  const fromNode = await api("/mempool?limit=100");
  const seen = new Map([...fromNode, ...state.pending].map((t) => [t.id, t]));
  const txs = [...seen.values()].filter((t) => !t.height);
  return [
    h("h1", { class: "page-title" }, "Pending Transactions"),
    h("section", { class: "card" },
      h("div", { class: "card-head" }, h("span", { class: "muted small" }, `${txs.length} transaction${txs.length === 1 ? "" : "s"} waiting for a block (updates live)`)),
      table(TX_HEADERS(false), txRows(txs, { block: false }))),
  ];
}

async function renderAccountsPage() {
  const [accounts, status] = await Promise.all([api("/accounts/top?limit=100"), state.status ? Promise.resolve(state.status) : api("/status")]);
  const supply = Number(status.total_supply_stein || 0);
  return [
    h("h1", { class: "page-title" }, "Top Accounts by STEIN Balance"),
    h("section", { class: "card" },
      h("div", { class: "card-head" }, h("span", { class: "muted small" }, `${num(status.accounts || accounts.length)} accounts · total supply ${num(supply)} STEIN`)),
      table(["Rank", "Address", "Balance", "Percentage", "Txn Count", "Key"], accounts.map((a, i) => [
        String(i + 1), linkAddr(a.address, true), `${a.balance_stein} STEIN`,
        supply ? `${((Number(a.balance_stein) / supply) * 100).toFixed(4)}%` : "—",
        num(a.nonce), a.key_registered ? h("span", { class: "pill ok" }, "registered") : h("span", { class: "pill" }, "—")]))),
  ];
}

async function renderValidatorsPage() {
  const v = await api("/validators");
  return [
    h("h1", { class: "page-title" }, "Validators"),
    h("section", { class: "card" },
      h("div", { class: "card-head" }, h("span", { class: "muted small" }, `${v.validators.length} active · total voting power ${num(v.total_power)}`)),
      table(["Validator", "Signature scheme", "Voting power", "Share"], v.validators.map((x) => [
        linkAddr(x.address, true), schemePill(x.scheme), num(x.power), `${(x.share * 100).toFixed(2)}%`]))),
  ];
}

function detailRows(rows) {
  return h("div", { class: "rows" }, rows.map((r) => r === "sep" ? h("div", { class: "sep" })
    : h("div", { class: "dr" }, h("div", { class: "k" }, r[2] ? help(r[2]) : null, `${r[0]}:`), h("div", { class: "v" }, r[1]))));
}

async function blockAt(height) {
  const [b] = await api(`/blocks?before=${height}&limit=1`);
  return b && b.height === height ? b : null;
}

async function renderTxPage(id) {
  if (!/^[0-9a-f]{64}$/.test(id)) throw new Error("not a transaction hash");
  const result = await api(`/tx/${id}`);
  if (result.status === "unknown") throw new Error("this node does not know that transaction");
  const t = result.tx;
  const block = t.height ? await blockAt(t.height) : null;
  const tip = state.status ? state.status.height : t.height;
  const confirmations = t.height ? Math.max(1, tip - t.height + 1) : 0;
  return [
    h("h1", { class: "page-title" }, "Transaction Details"),
    h("div", { class: "tabs" }, h("span", { class: "tab" }, "Overview")),
    h("section", { class: "card" }, detailRows([
      ["Transaction Hash", [h("span", { class: "mono" }, t.id), copyBtn(t.id)], "A unique identifier for this transaction: a hash of its body."],
      ["Status", t.height ? h("span", { class: "pill ok" }, "✓ Success") : h("span", { class: "pill warn" }, "◷ Pending (updates live)"), "Whether the transaction is in a block."],
      ["Block", t.height ? [linkBlock(t.height), h("span", { class: "pill" }, `${num(confirmations)} Block Confirmation${confirmations === 1 ? "" : "s"}`)] : "—", "The block that includes this transaction."],
      ["Timestamp", block ? `◷ ${ago(block.timestamp_ms)} (${utc(block.timestamp_ms)})` : "—", "When the block was produced."],
      "sep",
      ["From", [linkAddr(t.sender, true), copyBtn(t.sender)], "The sender, who signed the transaction."],
      ["To", [linkAddr(t.to, true), copyBtn(t.to)], "The recipient."],
      "sep",
      ["Value", h("b", {}, `${t.amount_stein} STEIN`), "The amount transferred."],
      ["Transaction Fee", `${t.fee_stein} STEIN`, "Paid per byte of the transaction: post-quantum signatures are large."],
      "sep",
      ["Signature", schemePill(t.scheme), "The post-quantum signature scheme that authorised this transaction."],
      ["Key Registration", t.registers_key === null ? "— (signature pruned)" : t.registers_key ? "Yes: the sender's first transaction carries its public key" : "No: the sender's key was already on chain", "An account's first transaction registers its public key; later ones omit it to save space."],
      ["Size", bytes(t.size), "Encoded size of the transaction, signature included."],
      ["Nonce", String(t.nonce), "The sender's transaction count before this one."],
      ["Valid Until Block", num(t.expires_at), "After this height the transaction can no longer be included."],
    ])),
  ];
}

async function renderBlockPage(arg) {
  const height = Number(arg);
  if (!Number.isInteger(height) || height < 1) throw new Error("not a block height");
  const b = await blockAt(height);
  if (!b) throw new Error(`block ${height} does not exist yet`);
  const tip = state.status ? state.status.height : height;
  const txs = b.txs.map((t) => ({ ...t, ts: b.timestamp_ms }));
  return [
    h("h1", { class: "page-title" }, "Block", h("span", { class: "mono" }, `#${num(b.height)}`)),
    h("div", { class: "tabs" }, h("span", { class: "tab" }, "Overview")),
    h("section", { class: "card" }, detailRows([
      ["Block Height", [h("b", {}, num(b.height)),
        b.height > 1 ? h("a", { href: `#/block/${b.height - 1}`, class: "chip", title: "Previous block" }, "‹") : null,
        b.height < tip ? h("a", { href: `#/block/${b.height + 1}`, class: "chip", title: "Next block" }, "›") : null], "The block's position in the chain."],
      ["Status", h("span", { class: "pill ok" }, "✓ Finalized"), "On the devnet a block is final once produced; with BFT consensus, once more than ⅔ of stake has signed it."],
      ["Timestamp", `◷ ${ago(b.timestamp_ms)} (${utc(b.timestamp_ms)})`, "When the validator produced the block."],
      ["Transactions", [h("b", {}, `${b.tx_count} transaction${b.tx_count === 1 ? "" : "s"}`), " in this block"], "Transfers included and executed in this block."],
      "sep",
      ["Validated By", [linkAddr(b.proposer, true), copyBtn(b.proposer)], "The validator that proposed this block."],
      ["Fees", b.fees_stein === undefined ? "—" : `${b.fees_stein} STEIN`, "Fees paid by this block's transactions."],
      ["Size", bytes(b.size), "Transactions' encoded size, signatures included."],
      ["Signatures", b.witnesses_available ? h("span", { class: "pill ok" }, "Retained") : h("span", { class: "pill" }, "Pruned after finality"), "Nodes keep signatures for a retention window (6 h), then prune them; the block still commits to them."],
      "sep",
      ["Hash", [h("span", { class: "mono" }, b.hash), copyBtn(b.hash)], "The block's hash."],
      ["Parent Hash", b.height > 1 ? h("a", { href: `#/block/${b.height - 1}`, class: "mono" }, b.parent) : h("span", { class: "mono" }, `${b.parent} (genesis)`), "The previous block's hash."],
      ["State Root", h("span", { class: "mono" }, b.state_root), "Commitment to every account after this block."],
    ])),
    h("section", { class: "card section-gap" },
      h("div", { class: "card-head" }, "Transactions"),
      table(TX_HEADERS(false), txRows(txs, { block: false }))),
  ];
}

async function renderAddressPage(address) {
  if (!/^st[0-9a-f]{72}$/.test(address)) throw new Error("not a Stein address");
  const [account, txs] = await Promise.all([api(`/account/${address}`), api(`/account/${address}/txs?limit=100`)]);
  const heights = [...new Set(txs.map((t) => t.height))];
  const times = new Map();
  for (const b of state.blocks) times.set(b.height, b.timestamp_ms);
  await Promise.all(heights.filter((x) => !times.has(x)).slice(0, 40).map(async (x) => {
    const b = await blockAt(x); if (b) times.set(x, b.timestamp_ms);
  }));
  const rows = txs.map((t) => ({ ...t, ts: times.get(t.height) || null }));
  const sent = txs.find((t) => t.sender === address && t.scheme);
  return [
    h("h1", { class: "page-title" }, "Address", h("span", { class: "mono" }, address), copyBtn(address)),
    h("div", { class: "split" },
      h("section", { class: "card" }, h("div", { class: "card-head" }, "Overview"),
        h("div", { class: "kv-mini" },
          h("div", { class: "k" }, "STEIN Balance"), h("div", { class: "v" }, h("b", {}, `${account.balance_stein} STEIN`)),
          h("div", { class: "k" }, "STEIN Value"), h("div", { class: "v muted" }, "N/A (devnet: no market)"))),
      h("section", { class: "card" }, h("div", { class: "card-head" }, "More Info"),
        h("div", { class: "kv-mini" },
          h("div", { class: "k" }, "Transactions Sent"), h("div", { class: "v" }, num(account.nonce)),
          h("div", { class: "k" }, "Public Key On Chain"), h("div", { class: "v" }, account.key_registered ? h("span", { class: "pill ok" }, "Registered") : h("span", { class: "pill" }, "Not yet: registered by its first transaction")),
          h("div", { class: "k" }, "Signature Scheme"), h("div", { class: "v" }, sent ? schemePill(sent.scheme) : h("span", { class: "muted" }, "Unknown until it sends"))))),
    h("div", { class: "tabs" }, h("span", { class: "tab" }, "Transactions")),
    h("section", { class: "card" },
      h("div", { class: "card-head" }, h("span", { class: "muted small" }, `Latest ${txs.length} transaction${txs.length === 1 ? "" : "s"}`)),
      table(TX_HEADERS(), txRows(rows))),
  ];
}

// ---------------------------------------------------------------------------
// Search, network switch, start-up

function go(raw, filter = "all") {
  const q = String(raw || "").trim().toLowerCase();
  if (!q) return;
  const isBlock = /^\d+$/.test(q), isAddr = /^st[0-9a-f]{72}$/.test(q), isTx = /^(0x)?[0-9a-f]{64}$/.test(q);
  if (isBlock && (filter === "all" || filter === "block")) location.hash = `#/block/${q}`;
  else if (isAddr && (filter === "all" || filter === "address")) location.hash = `#/address/${q}`;
  else if (isTx && (filter === "all" || filter === "tx")) location.hash = `#/tx/${q.replace(/^0x/, "")}`;
  else banner("Enter a block number, a transaction hash (64 hex characters) or an address (st…).");
}

function setupNetworks() {
  const select = $("node-select");
  const known = NETWORKS.some((n) => n.url === state.node);
  const options = NETWORKS.map((n) => h("option", { value: n.url, selected: n.url === state.node }, n.name));
  if (!known) options.push(h("option", { value: state.node, selected: true }, state.node));
  options.push(h("option", { value: "custom" }, "Custom node URL…"));
  select.replaceChildren(...options);
}

async function switchNode(url) {
  state.node = url;
  state.blocks = [];
  state.pending = [];
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
    state.blocks = await api(`/blocks?limit=${SPARK_BLOCKS}`);
    state.pending = await api("/mempool?limit=50");
  } catch { /* the banner shows the problem */ }
}

$("node-select").addEventListener("change", (e) => {
  let url = e.target.value;
  if (url === "custom") {
    url = (prompt("Node RPC URL (https://… or http://127.0.0.1:port)") || "").trim();
    if (!/^https?:\/\/\S+$/.test(url)) { e.target.value = state.node; return; }
  }
  switchNode(url.replace(/\/+$/, ""));
});
$("nav-search").addEventListener("submit", (e) => { e.preventDefault(); go($("nav-search-input").value); });
window.addEventListener("hashchange", () => { banner(""); render(); window.scrollTo(0, 0); });
setInterval(() => { for (const el of document.querySelectorAll(".ago")) el.textContent = ago(Number(el.dataset.ts)); }, 1000);
setInterval(() => { if (route().page === "home") renderOverview(); }, 5000);
setInterval(refreshStatus, 10000);

state.node = chooseNode();
setupNetworks();
connect();
loadInitial().then(render);
