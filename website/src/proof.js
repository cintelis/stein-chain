// Opening the inbox from a browser. The Worker holds no secret, so it cannot
// check a password. Instead it asks the caller to prove they hold the inbox's
// secret key: it encapsulates a fresh secret to the inbox's public key (the
// challenge) and accepts only the proof derived from that secret, once,
// within two minutes. Only the secret key can decapsulate the challenge.

const CONTEXT = new TextEncoder().encode("steinchain.com inbox proof v1");

/** The proof for a challenge's shared secret, as hex. */
export async function proofOf(shared) {
  const input = new Uint8Array(CONTEXT.length + shared.length);
  input.set(CONTEXT);
  input.set(shared, CONTEXT.length);
  const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", input));
  return Array.from(digest, (b) => b.toString(16).padStart(2, "0")).join("");
}

export const toBase64 = (bytes) => {
  let s = "";
  for (const b of bytes) s += String.fromCharCode(b);
  return btoa(s);
};

export const fromBase64 = (text) => Uint8Array.from(atob(text), (c) => c.charCodeAt(0));

export const fromHex = (hex) =>
  Uint8Array.from(hex.match(/.{2}/g) ?? [], (pair) => Number.parseInt(pair, 16));
