import assert from "node:assert/strict";
import crypto from "node:crypto";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { ml_kem768 } from "@noble/post-quantum/ml-kem.js";
import { generateKeyPair, open, seal } from "../src/envelope.js";
import { ENVELOPE_BYTES, MAX_TEXT_BYTES, PUBLIC_KEY_BYTES } from "../src/format.js";
import { proofOf } from "../src/proof.js";
import { askUrl, messageSigner, registrationAddress, splitAnswer, toBase64Url } from "../src/statement.js";
import { loadCore } from "../vendor/stein-wallet-core/core.mjs";

const core = await loadCore(
  readFileSync(new URL("../vendor/stein-wallet-core/stein_wallet_core.wasm", import.meta.url)),
);
const walletSeed = (byte) => new Uint8Array(32).fill(byte);

test("a sealed message opens with the secret key, and only with it", async () => {
  const [publicKey, secretKey] = await generateKeyPair();
  const [, otherSecret] = await generateKeyPair();
  const text = "Hello from nobody in particular. ✓";
  const envelope = await seal(publicKey, text);
  assert.deepEqual(await open(secretKey, envelope), { text, signed: null });
  await assert.rejects(open(otherSecret, envelope));
});

test("every envelope has the same size, whatever the text", async () => {
  const [publicKey, secretKey] = await generateKeyPair();
  const long = "x".repeat(MAX_TEXT_BYTES);
  for (const text of ["a", "two words", long]) {
    const envelope = await seal(publicKey, text);
    assert.equal(envelope.length, ENVELOPE_BYTES);
    assert.equal((await open(secretKey, envelope)).text, text);
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

test("a version 1 envelope still opens", async () => {
  const fixture = JSON.parse(readFileSync(new URL("./fixtures/envelope-v1.json", import.meta.url), "utf8"));
  const secretKey = new Uint8Array(Buffer.from(fixture.secret_key, "base64"));
  const envelope = new Uint8Array(Buffer.from(fixture.envelope, "hex"));
  assert.deepEqual(await open(secretKey, envelope), { text: fixture.text, signed: null });
});

test("a signed message is the same size and names its signer, if the signature holds", async () => {
  const [inboxKey, secretKey] = await generateKeyPair();
  const text = "x".repeat(MAX_TEXT_BYTES);
  const signed = core.signStatement({
    seed: walletSeed(7),
    kind: 2,
    payload: new Uint8Array([...inboxKey, ...new TextEncoder().encode(text)]),
  });
  const envelope = await seal(inboxKey, text, signed);
  assert.equal(envelope.length, ENVELOPE_BYTES);

  const opened = await open(secretKey, envelope);
  assert.equal(opened.text, text);
  // The address stein-chain's own wallet gives for this seed.
  assert.equal(
    messageSigner(core, inboxKey, opened.text, opened.signed),
    "st75b5c47ef28c7b2efdda5d1eb3b6fe12eb58afe3f30d4644c273b70cfe8d59215637069b",
  );
  // Not for another text, nor for another inbox, nor as a registration.
  assert.equal(messageSigner(core, inboxKey, "other words", opened.signed), null);
  const [otherInbox] = await generateKeyPair();
  assert.equal(messageSigner(core, otherInbox, opened.text, opened.signed), null);
  assert.equal(registrationAddress(core, opened.signed), null);
});

test("a registration proves its own account and nothing else", () => {
  const address = core.addressFromSeed(walletSeed(7));
  const signed = core.signStatement({ seed: walletSeed(7), kind: 1, payload: core.decodeAddress(address) });
  assert.equal(registrationAddress(core, signed), address);

  // Another account's address signed by this key is not a registration.
  const other = core.addressFromSeed(walletSeed(42));
  const wrong = core.signStatement({ seed: walletSeed(7), kind: 1, payload: core.decodeAddress(other) });
  assert.equal(registrationAddress(core, wrong), null);
  const altered = { publicKey: signed.publicKey, signature: signed.signature.slice() };
  altered.signature[50] ^= 1;
  assert.equal(registrationAddress(core, altered), null);
  assert.equal(registrationAddress(core, { publicKey: new Uint8Array(897), signature: signed.signature }), null);
});

test("the wallet's answer is read back, and junk is not", () => {
  const signed = core.signStatement({ seed: walletSeed(7), kind: 1, payload: new Uint8Array(32) });
  const answer = toBase64Url(new Uint8Array([...signed.publicKey, ...signed.signature]));
  assert.deepEqual(splitAnswer(answer), signed);
  assert.equal(splitAnswer("declined"), null);
  assert.equal(splitAnswer("!!"), null);
  const url = new URL(askUrl("https://wallet.example", "message", "ab".repeat(16), new Uint8Array(1184), "héllo"));
  const params = new URLSearchParams(url.hash.slice(1));
  assert.equal(url.pathname, "/sign");
  assert.equal(params.get("kind"), "message");
  assert.equal(Buffer.from(params.get("text"), "base64url").toString(), "héllo");
});
