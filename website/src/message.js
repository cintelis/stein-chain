// The message form on steinchain.com. The text is sealed here, in the
// browser, to the inbox's public key; only the sealed bytes leave the page.

import { seal } from "./envelope.js";
import { MAX_TEXT_BYTES } from "./format.js";
import { INBOX_KEY } from "./inbox-key.js";

const $ = (id) => document.getElementById(id);
const form = $("message-form");
const text = $("message-text");
const count = $("message-count");
const send = $("message-send");
const state = $("message-state");

const publicKey = Uint8Array.from(atob(INBOX_KEY), (c) => c.charCodeAt(0));
const bytes = () => new TextEncoder().encode(text.value.trim()).length;

function say(message, kind) {
  state.textContent = message;
  state.className = `form-state ${kind}`;
}

function recount() {
  const used = bytes();
  count.textContent = `${used.toLocaleString("en-US")} / ${MAX_TEXT_BYTES.toLocaleString("en-US")}`;
  count.classList.toggle("over", used > MAX_TEXT_BYTES);
  send.disabled = used === 0 || used > MAX_TEXT_BYTES;
}

text.addEventListener("input", recount);
recount();

form.addEventListener("submit", async (event) => {
  event.preventDefault();
  send.disabled = true;
  say("Encrypting…", "");
  try {
    const envelope = await seal(publicKey, text.value.trim());
    const response = await fetch("/api/message", {
      method: "POST",
      headers: { "content-type": "application/octet-stream" },
      body: envelope,
      credentials: "omit",
      referrerPolicy: "no-referrer",
    });
    if (response.status === 204) {
      text.value = "";
      say("Sent. It was encrypted in your browser; only the recipient can read it.", "ok");
    } else if (response.status === 429) {
      say("Too many messages just now. Try again in a minute.", "bad");
    } else {
      say(`Not sent (error ${response.status}). Try again.`, "bad");
    }
  } catch (e) {
    say(e instanceof TypeError ? "Not sent: the site could not be reached." : `Not sent: ${e.message}`, "bad");
  }
  recount();
});
