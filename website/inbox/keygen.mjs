// Makes the inbox's key pair. The secret key goes to a file outside the
// repository and is never overwritten; the public key goes into
// src/inbox-key.js, which is committed and shipped with the page.
//
//   node inbox/keygen.mjs [secret-key-file]     default ~/.stein/inbox.key
//
// Messages sealed to an old public key can only be opened with its secret
// key: read and purge the inbox before replacing the pair.

import { mkdirSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { generateKeyPair } from "../src/envelope.js";

const out = process.argv[2] ?? join(homedir(), ".stein", "inbox.key");
const [publicKey, secretKey] = await generateKeyPair();
const b64 = (bytes) => Buffer.from(bytes).toString("base64");

mkdirSync(dirname(out), { recursive: true });
// "wx": fail if the file exists.
writeFileSync(out, `ml-kem-768:${b64(secretKey)}\n`, { flag: "wx", mode: 0o600 });

const here = dirname(fileURLToPath(import.meta.url));
writeFileSync(
  join(here, "..", "src", "inbox-key.js"),
  `// The inbox's ML-KEM-768 public key (base64). Written by inbox/keygen.mjs.
// Public by design: anyone may seal a message to it, only the secret key
// opens one.
export const INBOX_KEY =
  "${b64(publicKey)}";
`,
);
console.log(`secret key written to ${out}: the only copy, and the only way to read the inbox`);
console.log("public key written to src/inbox-key.js: commit it");
