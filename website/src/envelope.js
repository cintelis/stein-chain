// Seals a message to the inbox's public key, and opens it with the secret
// key. Post-quantum throughout: ML-KEM-768 (FIPS 203) agrees a secret, HKDF
// turns it into a key used once, AES-256-GCM encrypts. Runs unchanged in a
// browser and in Node (both have WebCrypto).
//
// A message may carry a wallet's signature (src/statement.js). It travels
// inside the sealed part: only the inbox's owner learns who signed.

import { ml_kem768 } from "@noble/post-quantum/ml-kem.js";
import {
  ENVELOPE_BYTES,
  KEM_BYTES,
  MAX_TEXT_BYTES,
  NONCE_BYTES,
  PADDED_BYTES,
  PUBLIC_KEY_BYTES,
  SECRET_KEY_BYTES,
  SIGNATURE_BYTES,
  SIGNER_KEY_BYTES,
  V1_ENVELOPE_BYTES,
  VERSION,
} from "./format.js";

const INFO = new TextEncoder().encode("steinchain.com message v1");
const HEADER_BYTES = 1 + KEM_BYTES;

// The key is bound to this exchange: the KEM ciphertext is the HKDF salt.
async function messageKey(shared, kemCiphertext, usage) {
  const ikm = await crypto.subtle.importKey("raw", shared, "HKDF", false, ["deriveKey"]);
  return crypto.subtle.deriveKey(
    { name: "HKDF", hash: "SHA-256", salt: kemCiphertext, info: INFO },
    ikm,
    { name: "AES-GCM", length: 256 },
    false,
    [usage],
  );
}

/**
 * Seals `text` to `publicKey`, with the signer's `{ publicKey, signature }`
 * if there is one. Always returns ENVELOPE_BYTES bytes.
 */
export async function seal(publicKey, text, signed = null) {
  if (publicKey.length !== PUBLIC_KEY_BYTES) throw new Error("bad public key");
  const body = new TextEncoder().encode(text);
  if (body.length === 0) throw new Error("The message is empty.");
  if (body.length > MAX_TEXT_BYTES) throw new Error("The message is too long.");
  if (signed && (signed.publicKey.length !== SIGNER_KEY_BYTES || signed.signature.length !== SIGNATURE_BYTES)) {
    throw new Error("bad signature");
  }

  const padded = new Uint8Array(PADDED_BYTES);
  padded[0] = signed ? 1 : 0;
  new DataView(padded.buffer).setUint16(1, body.length, true);
  padded.set(body, 3);
  if (signed) {
    padded.set(signed.publicKey, 3 + body.length);
    padded.set(signed.signature, 3 + body.length + SIGNER_KEY_BYTES);
  }

  const { cipherText: kemCiphertext, sharedSecret: shared } = ml_kem768.encapsulate(publicKey);
  const envelope = new Uint8Array(ENVELOPE_BYTES);
  envelope[0] = VERSION;
  envelope.set(kemCiphertext, 1);
  const nonce = crypto.getRandomValues(new Uint8Array(NONCE_BYTES));
  envelope.set(nonce, HEADER_BYTES);

  const key = await messageKey(shared, kemCiphertext, "encrypt");
  const sealed = await crypto.subtle.encrypt(
    { name: "AES-GCM", iv: nonce, additionalData: envelope.subarray(0, HEADER_BYTES) },
    key,
    padded,
  );
  envelope.set(new Uint8Array(sealed), HEADER_BYTES + NONCE_BYTES);
  return envelope;
}

const utf8 = (bytes) => new TextDecoder("utf-8", { fatal: true }).decode(bytes);

/**
 * Opens an envelope: `{ text, signed }`, where `signed` is null or the
 * signer's `{ publicKey, signature }`, not yet verified (src/statement.js
 * does that). Throws if it was not sealed to this key or was altered.
 */
export async function open(secretKey, envelope) {
  if (secretKey.length !== SECRET_KEY_BYTES) throw new Error("bad secret key");
  const version = envelope[0];
  const known =
    (version === VERSION && envelope.length === ENVELOPE_BYTES) ||
    (version === 1 && envelope.length === V1_ENVELOPE_BYTES);
  if (!known) throw new Error("not an envelope this inbox knows");

  const kemCiphertext = envelope.subarray(1, HEADER_BYTES);
  const nonce = envelope.subarray(HEADER_BYTES, HEADER_BYTES + NONCE_BYTES);
  const shared = ml_kem768.decapsulate(kemCiphertext, secretKey);
  const key = await messageKey(shared, kemCiphertext, "decrypt");
  const padded = new Uint8Array(
    await crypto.subtle.decrypt(
      { name: "AES-GCM", iv: nonce, additionalData: envelope.subarray(0, HEADER_BYTES) },
      key,
      envelope.subarray(HEADER_BYTES + NONCE_BYTES),
    ),
  );
  const view = new DataView(padded.buffer);

  if (version === 1) {
    const length = view.getUint16(0, true);
    if (length === 0 || length > MAX_TEXT_BYTES) throw new Error("bad padding");
    return { text: utf8(padded.subarray(2, 2 + length)), signed: null };
  }

  const length = view.getUint16(1, true);
  if (padded[0] > 1 || length === 0 || length > MAX_TEXT_BYTES) throw new Error("bad padding");
  const text = utf8(padded.subarray(3, 3 + length));
  if (padded[0] === 0) return { text, signed: null };
  const at = 3 + length;
  return {
    text,
    signed: {
      publicKey: padded.slice(at, at + SIGNER_KEY_BYTES),
      signature: padded.slice(at + SIGNER_KEY_BYTES, at + SIGNER_KEY_BYTES + SIGNATURE_BYTES),
    },
  };
}

/** A new inbox key pair: [publicKey, secretKey]. */
export function generateKeyPair() {
  const { publicKey, secretKey } = ml_kem768.keygen();
  return [publicKey, secretKey];
}
