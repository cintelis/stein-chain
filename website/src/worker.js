// steinchain.com's Worker. The site is static files; the Worker itself only
// keeps the message inbox:
//
//   POST /api/message           take a sealed message (anyone, anonymously)
//   POST /api/inbox/challenge   start opening the inbox
//   POST /api/inbox/open        list (and optionally delete) the sealed messages,
//                               for a caller who proves they hold the inbox's key
//
// What it keeps about a message: the sealed bytes and the UTC day. Nothing
// about who sent it: no address, no headers, no cookie, no log line. It
// cannot read a message, and it holds no secret of any kind: only the
// inbox's secret key opens a message, and that key is not on Cloudflare.

import { ml_kem768 } from "@noble/post-quantum/ml-kem.js";
import { ENVELOPE_BYTES, VERSION } from "./format.js";
import { INBOX_KEY } from "./inbox-key.js";
import { fromBase64, proofOf, toBase64 } from "./proof.js";

// A ceiling on what strangers can store: about 2.6 MB a day.
const DAILY_CAP = 500;
// A challenge is good once, for two minutes.
const CHALLENGE_TTL_S = 120;
const MAX_CHALLENGES = 1000;
// Messages returned by one call to open; the page asks again for more.
const PAGE = 200;

const reply = (status, body) =>
  new Response(body === undefined ? null : JSON.stringify(body), {
    status,
    headers: {
      "cache-control": "no-store",
      "x-content-type-options": "nosniff",
      ...(body === undefined ? {} : { "content-type": "application/json" }),
    },
  });
const refuse = (status, error) => reply(status, { error });

const sender = (request) => request.headers.get("cf-connecting-ip") ?? "unknown";

async function message(request, env) {
  if (request.headers.get("content-length") !== String(ENVELOPE_BYTES)) {
    return refuse(400, "not a sealed message");
  }

  // The address is used here, in memory, to slow a flood. It is not stored.
  const { success } = await env.MESSAGE_LIMIT.limit({ key: sender(request) });
  if (!success) return refuse(429, "too many messages; try again in a minute");

  const envelope = new Uint8Array(await request.arrayBuffer());
  if (envelope.length !== ENVELOPE_BYTES || envelope[0] !== VERSION) {
    return refuse(400, "not a sealed message");
  }

  const day = new Date().toISOString().slice(0, 10);
  const today = await env.INBOX.prepare("SELECT count(*) AS n FROM messages WHERE day = ?")
    .bind(day)
    .first("n");
  if (today >= DAILY_CAP) return refuse(429, "the inbox is full for today");

  await env.INBOX.prepare("INSERT INTO messages (day, envelope) VALUES (?, ?)")
    .bind(day, envelope)
    .run();
  return reply(204);
}

async function challenge(request, env) {
  const { success } = await env.INBOX_LIMIT.limit({ key: sender(request) });
  if (!success) return refuse(429, "too many attempts; try again in a minute");

  const now = Math.floor(Date.now() / 1000);
  await env.INBOX.prepare("DELETE FROM challenges WHERE expires < ?").bind(now).run();
  const open = await env.INBOX.prepare("SELECT count(*) AS n FROM challenges").first("n");
  if (open >= MAX_CHALLENGES) return refuse(429, "too many attempts; try again in a minute");

  const { cipherText, sharedSecret } = ml_kem768.encapsulate(fromBase64(INBOX_KEY));
  const id = crypto.randomUUID();
  await env.INBOX.prepare("INSERT INTO challenges (id, proof, expires) VALUES (?, ?, ?)")
    .bind(id, await proofOf(sharedSecret), now + CHALLENGE_TTL_S)
    .run();
  return reply(200, { id, challenge: toBase64(cipherText) });
}

async function open(request, env) {
  if (Number(request.headers.get("content-length") ?? "x") > 1000) return refuse(400, "bad request");
  let body;
  try {
    body = await request.json();
  } catch {
    return refuse(400, "bad request");
  }
  const { id, proof, delete_through: through } = body ?? {};
  if (typeof id !== "string" || typeof proof !== "string") return refuse(400, "bad request");
  if (through !== undefined && !Number.isSafeInteger(through)) return refuse(400, "bad request");

  // Taken out as it is checked: a challenge can be answered once.
  const now = Math.floor(Date.now() / 1000);
  const row = await env.INBOX.prepare("DELETE FROM challenges WHERE id = ? RETURNING proof, expires")
    .bind(id)
    .first();
  if (!row || row.expires < now || row.proof !== proof) return refuse(403, "not the inbox's key");

  if (through !== undefined) {
    await env.INBOX.prepare("DELETE FROM messages WHERE id <= ?").bind(through).run();
  }
  const { results } = await env.INBOX.prepare(
    "SELECT id, day, hex(envelope) AS envelope FROM messages ORDER BY id LIMIT ?",
  )
    .bind(PAGE + 1)
    .all();
  return reply(200, { messages: results.slice(0, PAGE), more: results.length > PAGE });
}

const ROUTES = {
  "/api/message": message,
  "/api/inbox/challenge": challenge,
  "/api/inbox/open": open,
};

export default {
  async fetch(request, env) {
    const url = new URL(request.url);
    if (!url.pathname.startsWith("/api/")) return env.ASSETS.fetch(request);

    const route = ROUTES[url.pathname];
    if (!route) return refuse(404, "not found");
    if (request.method !== "POST") return refuse(405, "POST only");

    // Only this site's own pages may call the API.
    let origin;
    try {
      origin = new URL(request.headers.get("origin") ?? "").host;
    } catch {
      origin = null;
    }
    if (origin !== url.host) return refuse(403, "wrong origin");

    try {
      return await route(request, env);
    } catch {
      // No detail: an error must not echo anything about the request.
      return refuse(500, "the inbox could not do that");
    }
  },
};
