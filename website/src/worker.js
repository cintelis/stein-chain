// steinchain.com's Worker. The site is static files; the one thing the Worker
// does itself is take a sealed message for the inbox (POST /api/message).
//
// What it keeps about a message: the sealed bytes and the UTC day. Nothing
// about who sent it: no address, no headers, no cookie, no log line. It
// cannot read a message either; only the inbox's secret key opens one, and
// that key is not on Cloudflare. There is no endpoint that reads the inbox.

import { ENVELOPE_BYTES, VERSION } from "./format.js";

// A ceiling on what strangers can store: about 2.6 MB a day.
const DAILY_CAP = 500;

const reply = (status, error) =>
  new Response(error ? JSON.stringify({ error }) : null, {
    status,
    headers: {
      "cache-control": "no-store",
      "x-content-type-options": "nosniff",
      ...(error ? { "content-type": "application/json" } : {}),
    },
  });

async function message(request, env, url) {
  if (request.method !== "POST") return reply(405, "POST only");

  // Only this site's own page may post here.
  let origin;
  try {
    origin = new URL(request.headers.get("origin") ?? "").host;
  } catch {
    origin = null;
  }
  if (origin !== url.host) return reply(403, "wrong origin");

  if (request.headers.get("content-length") !== String(ENVELOPE_BYTES)) {
    return reply(400, "not a sealed message");
  }

  // The address is used here, in memory, to slow a flood. It is not stored.
  const sender = request.headers.get("cf-connecting-ip") ?? "unknown";
  const { success } = await env.MESSAGE_LIMIT.limit({ key: sender });
  if (!success) return reply(429, "too many messages; try again in a minute");

  const envelope = new Uint8Array(await request.arrayBuffer());
  if (envelope.length !== ENVELOPE_BYTES || envelope[0] !== VERSION) {
    return reply(400, "not a sealed message");
  }

  const day = new Date().toISOString().slice(0, 10);
  const today = await env.INBOX.prepare("SELECT count(*) AS n FROM messages WHERE day = ?")
    .bind(day)
    .first("n");
  if (today >= DAILY_CAP) return reply(429, "the inbox is full for today");

  await env.INBOX.prepare("INSERT INTO messages (day, envelope) VALUES (?, ?)")
    .bind(day, envelope)
    .run();
  return reply(204);
}

export default {
  async fetch(request, env) {
    const url = new URL(request.url);
    if (url.pathname === "/api/message") {
      try {
        return await message(request, env, url);
      } catch {
        // No detail: an error must not echo anything about the request.
        return reply(500, "could not store the message");
      }
    }
    if (url.pathname.startsWith("/api/")) return reply(404, "not found");
    return env.ASSETS.fetch(request);
  },
};
