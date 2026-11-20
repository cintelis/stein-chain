// The message form and wallet registration on steinchain.com.
//
// A message is sealed here, in the browser, to the inbox's public key; only
// the sealed bytes leave the page. Sent as it is, it is anonymous. Signed
// with a Stein wallet first, it carries the signer's address inside the
// sealed bytes. Signing happens at wallet.steinchain.com: this page sends the
// visitor there and the wallet sends them back (src/statement.js).

import { loadCore } from "../vendor/stein-wallet-core/core.mjs";
import { seal } from "./envelope.js";
import { MAX_TEXT_BYTES } from "./format.js";
import { INBOX_KEY } from "./inbox-key.js";
import { fromBase64, toBase64 } from "./proof.js";
import { askUrl, messageSigner, registrationAddress, splitAnswer } from "./statement.js";

// Set at build time for local development only (scripts/build.mjs).
const WALLET = typeof WALLET_ORIGIN === "string" ? WALLET_ORIGIN : "https://wallet.steinchain.com";
// What this page asked the wallet for, kept for the trip there and back.
const PENDING = "steinchain:pending";

const $ = (id) => document.getElementById(id);
const form = $("message-form");
const text = $("message-text");
const count = $("message-count");
const send = $("message-send");
const sign = $("message-sign");
const inboxKey = fromBase64(INBOX_KEY);
const bytes = () => new TextEncoder().encode(text.value.trim()).length;
const short = (address) => `${address.slice(0, 10)}…${address.slice(-6)}`;

function say(id, message, kind = "") {
  const node = $(id);
  node.textContent = message;
  node.className = `form-state ${kind}`;
}

function recount() {
  const used = bytes();
  count.textContent = `${used.toLocaleString("en-US")} / ${MAX_TEXT_BYTES.toLocaleString("en-US")}`;
  count.classList.toggle("over", used > MAX_TEXT_BYTES);
  send.disabled = sign.disabled = used === 0 || used > MAX_TEXT_BYTES;
}

/** Seals and posts a message. Returns true if the inbox took it. */
async function post(message, signed) {
  const envelope = await seal(inboxKey, message, signed);
  const response = await fetch("/api/message", {
    method: "POST",
    headers: { "content-type": "application/octet-stream" },
    body: envelope,
    credentials: "omit",
    referrerPolicy: "no-referrer",
  });
  if (response.status === 204) return true;
  say(
    "message-state",
    response.status === 429
      ? "Too many messages just now. Try again in a minute."
      : `Not sent (error ${response.status}). Try again.`,
    "bad",
  );
  return false;
}

const failure = (e) =>
  e instanceof TypeError ? "the site could not be reached." : `${e.message}`;

text.addEventListener("input", recount);

// Anonymous: sealed and sent as it is.
form.addEventListener("submit", async (event) => {
  event.preventDefault();
  send.disabled = sign.disabled = true;
  say("message-state", "Encrypting…");
  try {
    if (await post(text.value.trim(), null)) {
      text.value = "";
      say("message-state", "Sent. It was encrypted in your browser; only the recipient can read it.", "ok");
    }
  } catch (e) {
    say("message-state", `Not sent: ${failure(e)}`, "bad");
  }
  recount();
});

/** Sends the visitor to the wallet, remembering what was asked. */
function ask(kind, message) {
  const state = Array.from(crypto.getRandomValues(new Uint8Array(16)), (b) =>
    b.toString(16).padStart(2, "0"),
  ).join("");
  sessionStorage.setItem(PENDING, JSON.stringify({ state, kind, text: message }));
  location.assign(askUrl(WALLET, kind, state, inboxKey, message));
}

sign.addEventListener("click", () => ask("message", text.value.trim()));
$("register").addEventListener("click", () => ask("register", ""));

/** Back from the wallet: finish what was asked. */
async function answered(params) {
  // The answer is in the fragment; take it out of the address bar and history.
  const section = params.get("kind") === "register" ? "register-wallet" : "message";
  history.replaceState(null, "", `${location.pathname}#${section}`);
  $(section).scrollIntoView();

  let pending = null;
  try {
    pending = JSON.parse(sessionStorage.getItem(PENDING));
  } catch {
    // treated as no request
  }
  sessionStorage.removeItem(PENDING);
  const kind = params.get("kind");
  const where = kind === "register" ? "register-state" : "message-state";
  if (!pending || pending.kind !== kind || pending.state !== params.get("state")) {
    say(where, "That answer from the wallet does not match a request from this page.", "bad");
    return;
  }
  if (kind === "message") {
    text.value = pending.text;
    recount();
  }
  if (params.get("wallet") === "declined") {
    say(where, kind === "message" ? "Not signed, and not sent. Your message is still here." : "Not registered.", "bad");
    return;
  }
  const signed = splitAnswer(params.get("wallet"));
  if (!signed) {
    say(where, "The wallet's answer could not be read.", "bad");
    return;
  }

  try {
    if (kind === "message") {
      say(where, "Checking the signature…");
      // The same check the inbox will make: do not send what it would reject.
      const core = await loadCore(await fetch("/stein_wallet_core.wasm"));
      const signer = messageSigner(core, inboxKey, pending.text, signed);
      if (!signer) {
        say(where, "The wallet's signature does not match this message. Not sent.", "bad");
        return;
      }
      say(where, "Encrypting…");
      if (await post(pending.text, signed)) {
        text.value = "";
        recount();
        say(where, `Sent, signed by ${short(signer)}. Only the recipient can read it or see who signed.`, "ok");
      }
    } else {
      say(where, "Registering…");
      const response = await fetch("/api/register", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ public_key: toBase64(signed.publicKey), signature: toBase64(signed.signature) }),
        credentials: "omit",
        referrerPolicy: "no-referrer",
      });
      const body = await response.json().catch(() => ({}));
      if (response.ok) {
        say(where, body.already ? `${short(body.address)} was already registered.` : `Registered ${short(body.address)}.`, "ok");
      } else {
        say(where, `Not registered: ${body.error ?? `error ${response.status}`}.`, "bad");
      }
    }
  } catch (e) {
    say(where, `${kind === "message" ? "Not sent" : "Not registered"}: ${failure(e)}`, "bad");
  }
}

recount();
const params = new URLSearchParams(location.hash.slice(1));
if (params.has("wallet")) answered(params);
