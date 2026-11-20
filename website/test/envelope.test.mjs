import assert from "node:assert/strict";
import crypto from "node:crypto";
import { test } from "node:test";
import { ml_kem768 } from "@noble/post-quantum/ml-kem.js";
import { generateKeyPair, open, seal } from "../src/envelope.js";
import { ENVELOPE_BYTES, MAX_TEXT_BYTES, PUBLIC_KEY_BYTES } from "../src/format.js";
import { proofOf } from "../src/proof.js";

test("a sealed message opens with the secret key, and only with it", async () => {
  const [publicKey, secretKey] = await generateKeyPair();
  const [, otherSecret] = await generateKeyPair();
  const text = "Hello from nobody in particular. ✓";
  const envelope = await seal(publicKey, text);
  assert.equal(await open(secretKey, envelope), text);
  await assert.rejects(open(otherSecret, envelope));
});

test("every envelope has the same size, whatever the text", async () => {
  const [publicKey, secretKey] = await generateKeyPair();
  const long = "x".repeat(MAX_TEXT_BYTES);
  for (const text of ["a", "two words", long]) {
    const envelope = await seal(publicKey, text);
    assert.equal(envelope.length, ENVELOPE_BYTES);
    assert.equal(await open(secretKey, envelope), text);
  }
});

test("the same text seals differently each time", async () => {
  const [publicKey] = await generateKeyPair();
  const a = await seal(publicKey, "same");
  const b = await seal(publicKey, "same");
  assert.notDeepEqual(a, b);
});

test("empty and oversized messages are refused", async () => {
  const [publicKey] = await generateKeyPair();
  await assert.rejects(seal(publicKey, ""));
  await assert.rejects(seal(publicKey, "x".repeat(MAX_TEXT_BYTES + 1)));
  // Bytes, not characters: this is 4,002 bytes in 2,001 characters.
  await assert.rejects(seal(publicKey, "é".repeat(MAX_TEXT_BYTES / 2 + 1)));
});

test("an altered envelope does not open", async () => {
  const [publicKey, secretKey] = await generateKeyPair();
  const envelope = await seal(publicKey, "intact");
  for (const at of [0, 1, 1100, 2000, ENVELOPE_BYTES - 1]) {
    const altered = envelope.slice();
    altered[at] ^= 1;
    await assert.rejects(open(secretKey, altered), `byte ${at}`);
  }
});

// The inbox page checks a key file against the site's public key by reading
// the public key out of the secret key, and proves possession to the server.
test("a secret key carries its public key, and answers a challenge", async () => {
  const [publicKey, secretKey] = await generateKeyPair();
  assert.deepEqual(secretKey.subarray(1152, 1152 + PUBLIC_KEY_BYTES), publicKey);
  const { cipherText, sharedSecret } = ml_kem768.encapsulate(publicKey);
  const proof = await proofOf(sharedSecret);
  assert.equal(await proofOf(ml_kem768.decapsulate(cipherText, secretKey)), proof);
  const [, otherSecret] = await generateKeyPair();
  assert.notEqual(await proofOf(ml_kem768.decapsulate(cipherText, otherSecret)), proof);
});

// The library against an independent implementation (OpenSSL, through Node):
// a secret encapsulated by one must be the secret decapsulated by the other.
test("ML-KEM-768 agrees with Node's implementation", async (t) => {
  let pair;
  try {
    pair = crypto.generateKeyPairSync("ml-kem-768");
  } catch {
    t.skip("this Node has no ML-KEM");
    return;
  }
  const spki = pair.publicKey.export({ format: "der", type: "spki" });
  const publicKey = new Uint8Array(spki.subarray(spki.length - PUBLIC_KEY_BYTES));
  const { cipherText: ciphertext, sharedSecret: shared } = ml_kem768.encapsulate(publicKey);
  const theirs = crypto.decapsulate(pair.privateKey, ciphertext);
  assert.deepEqual(new Uint8Array(theirs), new Uint8Array(shared));
});
