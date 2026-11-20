// Statements: what a Stein wallet signs for this site. A statement is not a
// transaction: the wallet core signs it under its own prefix
// ("STEIN-STATEMENT-v1" ‖ kind ‖ payload), which no transaction's signed
// bytes can equal, so it proves who asked and can never be used on the chain.
//
// Two kinds:
//
//   1  register   payload: the account's 32-byte address
//                 "I hold this account; add it to Stein Chain's list."
//   2  message    payload: the inbox's public key ‖ the message's UTF-8 text
//                 "I wrote this, to this inbox."
//
// The wallet (wallet.steinchain.com/sign) is asked through a link and answers
// through a link back; both carry their data in the URL fragment, which is
// never sent to a server:
//
//   ask      <wallet>/sign#v=1&kind=register&state=<hex>
//            <wallet>/sign#v=1&kind=message&state=<hex>&to=<inbox key>&text=<utf-8>
//   answer   <site>/#wallet=<public key ‖ signature>&kind=<kind>&state=<hex>
//            <site>/#wallet=declined&kind=<kind>&state=<hex>
//
// Binary values are base64url. Signing and verifying are the wallet core's
// (vendor/stein-wallet-core): the one FN-DSA-512 implementation the chain uses.

import { SIGNATURE_BYTES, SIGNER_KEY_BYTES } from "./format.js";

export const KIND_REGISTER = 1;
export const KIND_MESSAGE = 2;

export const toBase64Url = (bytes) => {
  let s = "";
  for (const b of bytes) s += String.fromCharCode(b);
  return btoa(s).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
};

export const fromBase64Url = (text) => {
  const padded = text.replace(/-/g, "+").replace(/_/g, "/");
  return Uint8Array.from(atob(padded + "=".repeat((4 - (padded.length % 4)) % 4)), (c) => c.charCodeAt(0));
};

/** What a message's signature covers: the inbox it is for, then its text. */
export function messagePayload(inboxKey, text) {
  const body = new TextEncoder().encode(text);
  const payload = new Uint8Array(inboxKey.length + body.length);
  payload.set(inboxKey);
  payload.set(body, inboxKey.length);
  return payload;
}

/** The link that asks the wallet to sign. */
export function askUrl(walletOrigin, kind, state, inboxKey, text) {
  const params = new URLSearchParams({ v: "1", kind, state });
  if (kind === "message") {
    params.set("to", toBase64Url(inboxKey));
    params.set("text", toBase64Url(new TextEncoder().encode(text)));
  }
  return `${walletOrigin}/sign#${params.toString()}`;
}

/** The public key and signature in a wallet's answer, or null if it is not one. */
export function splitAnswer(value) {
  let bytes;
  try {
    bytes = fromBase64Url(value);
  } catch {
    return null;
  }
  if (bytes.length !== SIGNER_KEY_BYTES + SIGNATURE_BYTES) return null;
  return { publicKey: bytes.subarray(0, SIGNER_KEY_BYTES), signature: bytes.subarray(SIGNER_KEY_BYTES) };
}

/**
 * The address that signed a message, or null if the signature is not a valid
 * one of this text for this inbox. `core` is the loaded wallet core.
 */
export function messageSigner(core, inboxKey, text, signed) {
  const ok = core.verifyStatement({
    publicKey: signed.publicKey,
    kind: KIND_MESSAGE,
    payload: messagePayload(inboxKey, text),
    signature: signed.signature,
  });
  return ok ? core.addressFromPublicKey(signed.publicKey) : null;
}

/**
 * The address a registration proves, or null if the signature is not that
 * account's signature of its own address.
 */
export function registrationAddress(core, signed) {
  let address;
  try {
    address = core.addressFromPublicKey(signed.publicKey);
  } catch {
    return null;
  }
  const ok = core.verifyStatement({
    publicKey: signed.publicKey,
    kind: KIND_REGISTER,
    payload: core.decodeAddress(address),
    signature: signed.signature,
  });
  return ok ? address : null;
}
