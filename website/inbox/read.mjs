// Reads the inbox: fetches the sealed messages from the D1 database with
// wrangler and opens them here, with the secret key on this machine.
//
//   node inbox/read.mjs                  print every message
//   node inbox/read.mjs --purge          print them, then delete them from the database
//   node inbox/read.mjs --key <file>     secret key file (default ~/.stein/inbox.key)
//
// wrangler needs access to the Cloudflare account: `npx wrangler login`, or
// CLOUDFLARE_API_TOKEN with D1 edit rights. The Worker itself has no way to
// read the inbox, so this is the only door.

import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { open } from "../src/envelope.js";

const DATABASE = "steinchain-inbox";
const args = process.argv.slice(2);
const purge = args.includes("--purge");
const keyFile = args.includes("--key")
  ? args[args.indexOf("--key") + 1]
  : join(homedir(), ".stein", "inbox.key");

const [scheme, encoded] = readFileSync(keyFile, "utf8").trim().split(":");
if (scheme !== "ml-kem-768" || !encoded) throw new Error(`${keyFile} is not an inbox key`);
const secretKey = new Uint8Array(Buffer.from(encoded, "base64"));

const website = join(dirname(fileURLToPath(import.meta.url)), "..");
function sql(command) {
  const out = execFileSync(
    process.execPath,
    [join(website, "node_modules", "wrangler", "bin", "wrangler.js"),
      "d1", "execute", DATABASE, "--remote", "--json", "--command", command],
    { cwd: website, encoding: "utf8", maxBuffer: 256 * 1024 * 1024, stdio: ["ignore", "pipe", "inherit"] },
  );
  return JSON.parse(out)[0].results;
}

const rows = sql("SELECT id, day, hex(envelope) AS envelope FROM messages ORDER BY id");
if (rows.length === 0) {
  console.log("The inbox is empty.");
  process.exit(0);
}

let opened = 0;
for (const row of rows) {
  console.log(`\n#${row.id} · ${row.day}`);
  try {
    console.log(await open(secretKey, new Uint8Array(Buffer.from(row.envelope, "hex"))));
    opened += 1;
  } catch {
    // Anyone can post bytes of the right size; only real envelopes open.
    console.log("(not a message sealed to this key)");
  }
}
console.log(`\n${opened} of ${rows.length} opened.`);

if (purge) {
  // Only what was just shown: a message that arrived meanwhile stays.
  const last = Math.max(...rows.map((row) => Number(row.id)));
  sql(`DELETE FROM messages WHERE id <= ${last}`);
  console.log(`Deleted ${rows.length} from the database.`);
}
