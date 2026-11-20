// Seals a message to the inbox's public key, and opens it with the secret
// key. Post-quantum throughout: ML-KEM-768 (FIPS 203) agrees a secret, HKDF
// turns it into a key used once, AES-256-GCM encrypts. Runs unchanged in a
// browser and in Node (both have WebCrypto).

import { ml_kem768 } from "@noble/post-quantum/ml-kem.js";
import {
  ENVELOPE_BYTES,
  KEM_BYTES,
  MAX_TEXT_BYTES,
  NONCE_BYTES,
  PADDED_BYTES,
  PUBLIC_KEY_BYTES,
  SECRET_KEY_BYTES,
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

/** Seals `text` to `publicKey`. Always returns ENVELOPE_BYTES bytes. */
export async function seal(publicKey, text) {
  if (publicKey.length !== PUBLIC_KEY_BYTES) throw new Error("bad public key");
  const body = new TextEncoder().encode(text);
  if (body.length === 0) throw new Error("The message is empty.");
  if (body.length > MAX_TEXT_BYTES) throw new Error("The message is too long.");

  const padded = new Uint8Array(PADDED_BYTES);
  new DataView(padded.buffer).setUint16(0, body.length, true);
  padded.set(body, 2);

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

/** Opens an envelope. Throws if it was not sealed to this key or was altered. */
export async function open(secretKey, envelope) {
  if (secretKey.length !== SECRET_KEY_BYTES) throw new Error("bad secret key");
  if (envelope.length !== ENVELOPE_BYTES || envelope[0] !== VERSION) {
    throw new Error("not a version 1 envelope");
  }
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
  const length = new DataView(padded.buffer).getUint16(0, true);
  if (length === 0 || length > MAX_TEXT_BYTES) throw new Error("bad padding");
  return new TextDecoder("utf-8", { fatal: true }).decode(padded.subarray(2, 2 + length));
}

/** A new inbox key pair: [publicKey, secretKey]. */
export function generateKeyPair() {
  const { publicKey, secretKey } = ml_kem768.keygen();
  return [publicKey, secretKey];
}
