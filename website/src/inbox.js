// steinchain.com/inbox: reads the message inbox in the browser.
//
// The inbox's secret key is added once from its key file and kept in this
// browser only, encrypted under a passphrase (PBKDF2-SHA-256, 600,000 rounds,
// then AES-256-GCM). Unlocked, it does two things, both here: it answers the
// server's challenge, which is what lets this page fetch the sealed messages,
// and it opens them. Neither the key nor the passphrase is ever sent.
//
// A message may be signed by a Stein wallet. The signature is checked here,
// with the wallet core, and the signer's address shown only if it holds.

import { ml_kem768 } from "@noble/post-quantum/ml-kem.js";
import { loadCore } from "../vendor/stein-wallet-core/core.mjs";
import { open } from "./envelope.js";
import { PUBLIC_KEY_BYTES, SECRET_KEY_BYTES } from "./format.js";
import { INBOX_KEY } from "./inbox-key.js";
import { fromBase64, fromHex, proofOf, toBase64 } from "./proof.js";
import { messageSigner } from "./statement.js";

const VAULT = "steinchain:inbox-key:v1";
const ROUNDS = 600_000;
// Where an ML-KEM-768 secret key carries its own public key.
const PUBLIC_AT = 1152;

const EXPLORER = "https://steinscan.io/#/address/";

const $ = (id) => document.getElementById(id);
const views = { setup: $("view-setup"), locked: $("view-locked"), open: $("view-open") };
let secretKey = null;
let shownThrough = 0;

function show(name) {
  for (const [key, view] of Object.entries(views)) view.hidden = key !== name;
}

function say(id, message, kind = "") {
  const node = $(id);
  node.textContent = message;
  node.className = `form-state ${kind}`;
}

/* ---- The key, encrypted in this browser. ---- */

async function vaultKey(passphrase, salt) {
  const material = await crypto.subtle.importKey(
    "raw", new TextEncoder().encode(passphrase), "PBKDF2", false, ["deriveKey"],
  );
  return crypto.subtle.deriveKey(
    { name: "PBKDF2", hash: "SHA-256", salt, iterations: ROUNDS },
    material,
    { name: "AES-GCM", length: 256 },
    false,
    ["encrypt", "decrypt"],
  );
}

async function saveKey(key, passphrase) {
  const salt = crypto.getRandomValues(new Uint8Array(16));
  const iv = crypto.getRandomValues(new Uint8Array(12));
  const sealed = await crypto.subtle.encrypt(
    { name: "AES-GCM", iv }, await vaultKey(passphrase, salt), key,
  );
  localStorage.setItem(VAULT, JSON.stringify({
    v: 1, salt: toBase64(salt), iv: toBase64(iv), key: toBase64(new Uint8Array(sealed)),
  }));
}

async function loadKey(passphrase) {
  const vault = JSON.parse(localStorage.getItem(VAULT));
  const key = await crypto.subtle.decrypt(
    { name: "AES-GCM", iv: fromBase64(vault.iv) },
    await vaultKey(passphrase, fromBase64(vault.salt)),
    fromBase64(vault.key),
  );
  return new Uint8Array(key);
}

/** The secret key in a key file's text, if it is this inbox's key. */
function parseKeyFile(text) {
  const [scheme, encoded] = text.trim().split(":");
  if (scheme !== "ml-kem-768" || !encoded) throw new Error("That is not an inbox key file.");
  let key;
  try {
    key = fromBase64(encoded.trim());
  } catch {
    throw new Error("That is not an inbox key file.");
  }
  if (key.length !== SECRET_KEY_BYTES) throw new Error("That is not an inbox key file.");
  const itsPublic = toBase64(key.subarray(PUBLIC_AT, PUBLIC_AT + PUBLIC_KEY_BYTES));
  if (itsPublic !== INBOX_KEY) throw new Error("That key belongs to a different inbox.");
  return key;
}

/* ---- The inbox. ---- */

async function post(path, body) {
  const response = await fetch(path, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body ?? {}),
    credentials: "omit",
    referrerPolicy: "no-referrer",
  });
  if (!response.ok) {
    const detail = await response.json().catch(() => ({}));
    throw new Error(detail.error ?? `error ${response.status}`);
  }
  return response.json();
}

/** Proves possession of the key, then lists the inbox (deleting first if asked). */
async function fetchInbox(extra) {
  const { id, challenge } = await post("/api/inbox/challenge");
  const shared = ml_kem768.decapsulate(fromBase64(challenge), secretKey);
  return post("/api/inbox/open", { id, proof: await proofOf(shared), ...extra });
}

// The wallet core (FN-DSA-512 verification), loaded when first needed.
let core;
const walletCore = () => (core ??= fetch("/stein_wallet_core.wasm").then(loadCore));

function addressLink(address) {
  const link = document.createElement("a");
  link.href = EXPLORER + address;
  link.rel = "noopener";
  link.target = "_blank";
  link.className = "address";
  link.textContent = address;
  return link;
}

/** A line saying who signed a message, once the signature has been checked. */
async function signerLine(text, signed, registered) {
  const line = document.createElement("div");
  line.className = "signer";
  let signer = null;
  try {
    signer = messageSigner(await walletCore(), fromBase64(INBOX_KEY), text, signed);
  } catch {
    // The core did not load: say so rather than show an unchecked address.
    line.classList.add("bad");
    line.textContent = "Signed, but the signature could not be checked here.";
    return line;
  }
  if (!signer) {
    line.classList.add("bad");
    line.textContent = "Carries a signature that does not verify. Treat it as anonymous.";
    return line;
  }
  line.append("Signed by ", addressLink(signer), registered.has(signer) ? " · registered" : "");
  return line;
}

function renderWallets(wallets) {
  const list = $("wallets");
  list.replaceChildren();
  for (const row of wallets) {
    const item = document.createElement("li");
    item.append(addressLink(row.address), ` · ${row.day}`);
    list.append(item);
  }
  const n = wallets.length;
  $("wallet-count").textContent = n === 0 ? "No registered wallets yet." : `${n} registered wallet${n === 1 ? "" : "s"}`;
}

async function render({ messages, more, wallets = [] }) {
  const registered = new Set(wallets.map((w) => w.address));
  renderWallets(wallets);
  const list = $("messages");
  list.replaceChildren();
  shownThrough = 0;
  for (const row of messages) {
    shownThrough = Math.max(shownThrough, Number(row.id));
    const item = document.createElement("article");
    item.className = "card msg";
    const head = document.createElement("div");
    head.className = "label";
    head.textContent = `#${row.id} · ${row.day}`;
    const body = document.createElement("p");
    item.append(head, body);
    try {
      const { text, signed } = await open(secretKey, fromHex(row.envelope));
      // Text from a stranger: only ever set as text, never as markup.
      body.textContent = text;
      if (signed) item.append(await signerLine(text, signed, registered));
    } catch {
      // Anyone can post bytes of the right size; only real envelopes open.
      body.textContent = "(not a message sealed to this key)";
      body.className = "unread";
    }
    list.append(item);
  }
  const n = messages.length;
  $("inbox-count").textContent =
    n === 0 ? "The inbox is empty." : `${n} message${n === 1 ? "" : "s"}${more ? ", and more after these" : ""}`;
  $("delete").hidden = n === 0;
  resetDelete();
}

async function refresh(extra) {
  say("open-state", "Fetching…");
  try {
    await render(await fetchInbox(extra));
    say("open-state", "");
  } catch (e) {
    say("open-state", `Could not read the inbox: ${e.message}`, "bad");
  }
}

function lock() {
  secretKey?.fill(0);
  secretKey = null;
  $("messages").replaceChildren();
  show(localStorage.getItem(VAULT) ? "locked" : "setup");
}

/* ---- Buttons. ---- */

$("key-file").addEventListener("change", async (event) => {
  const file = event.target.files[0];
  if (file) $("key-text").value = (await file.text()).trim();
});

$("setup-form").addEventListener("submit", async (event) => {
  event.preventDefault();
  const passphrase = $("new-passphrase").value;
  try {
    const key = parseKeyFile($("key-text").value);
    if (passphrase.length < 8) throw new Error("Use a passphrase of at least 8 characters.");
    if (passphrase !== $("new-passphrase-again").value) throw new Error("The two passphrases differ.");
    say("setup-state", "Encrypting the key…");
    await saveKey(key, passphrase);
    secretKey = key;
    event.target.reset();
    say("setup-state", "");
    show("open");
    await refresh();
  } catch (e) {
    say("setup-state", e.message, "bad");
  }
});

$("unlock-form").addEventListener("submit", async (event) => {
  event.preventDefault();
  say("unlock-state", "Unlocking…");
  try {
    secretKey = await loadKey($("passphrase").value);
  } catch {
    say("unlock-state", "Wrong passphrase.", "bad");
    return;
  }
  event.target.reset();
  say("unlock-state", "");
  show("open");
  await refresh();
});

$("refresh").addEventListener("click", () => refresh());
$("lock").addEventListener("click", lock);

// Deleting and forgetting cannot be undone: each takes a second click.
function resetDelete() {
  $("delete").textContent = "Delete these messages";
  delete $("delete").dataset.armed;
}
$("delete").addEventListener("click", async () => {
  const button = $("delete");
  if (!button.dataset.armed) {
    button.dataset.armed = "1";
    button.textContent = "Click again to delete them for good";
    return;
  }
  await refresh({ delete_through: shownThrough });
});

for (const id of ["forget", "forget-locked"]) {
  $(id).addEventListener("click", (event) => {
    const button = event.currentTarget;
    if (!button.dataset.armed) {
      button.dataset.armed = "1";
      button.textContent = "Click again to remove the key from this browser";
      return;
    }
    delete button.dataset.armed;
    button.textContent = "Remove the key from this browser";
    localStorage.removeItem(VAULT);
    lock();
  });
}

show(localStorage.getItem(VAULT) ? "locked" : "setup");
