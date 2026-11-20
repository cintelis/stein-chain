// The only caller of the WebAssembly wallet core (wasm/src/ffi.rs).
//
// Plain JavaScript so that Node can run it in tests without a build step;
// core.d.mts gives it types for the app.

const SEED_LEN = 32;
const ADDRESS_TEXT_LEN = 74;
const TRANSFER_INPUT_LEN = 153;
const TRANSFER_OUTPUT_CAP = 32 + 16 + 2048;
/** Encoded length of an FN-DSA-512 public key. */
export const PUBLIC_KEY_LEN = 897;
/** Length of an FN-DSA-512 signature. */
export const SIGNATURE_LEN = 666;

const ERRORS = {
  [-1]: "the signing library refused the operation",
  [-2]: "an address must start with 'st'",
  [-3]: "an address is 'st' followed by 72 hex characters",
  [-4]: "address checksum mismatch: check for a typo",
  [-5]: "the amount must be more than zero",
  [-6]: "the fee is too large",
  [-7]: "internal error: output buffer too small",
};

function fail(code) {
  return new Error(ERRORS[code] ?? `wallet core error ${code}`);
}

const hex = (bytes) => Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");

/**
 * Instantiates the wallet core from the bytes of stein_wallet_core.wasm, a
 * Response for it, or a compiled WebAssembly.Module of it.
 */
export async function loadCore(source) {
  let bytes = source;
  if (source instanceof Response) {
    if (!source.ok) throw new Error(`could not fetch the wallet core (HTTP ${source.status})`);
    bytes = await source.arrayBuffer();
  }
  // Given a compiled module, instantiate() returns the instance itself.
  const made = await WebAssembly.instantiate(bytes, {});
  const x = (made.instance ?? made).exports;
  // Read the memory afresh every time: it is replaced when the module grows.
  const memory = () => new Uint8Array(x.memory.buffer);

  /** Runs `body` with buffers of the given sizes, wiping and freeing them after. */
  function withBuffers(sizes, body) {
    const pointers = sizes.map((size) => x.stein_alloc(size));
    try {
      return body(...pointers);
    } finally {
      pointers.forEach((pointer, i) => x.stein_free(pointer, sizes[i]));
    }
  }

  function encodeAddress(address) {
    return withBuffers([32, ADDRESS_TEXT_LEN], (input, out) => {
      memory().set(address, input);
      x.stein_address_encode(input, out);
      return new TextDecoder().decode(memory().subarray(out, out + ADDRESS_TEXT_LEN));
    });
  }

  function decodeAddress(text) {
    const bytes = new TextEncoder().encode(text.trim());
    return withBuffers([Math.max(bytes.length, 1), 32], (input, out) => {
      memory().set(bytes, input);
      const code = x.stein_address_decode(input, bytes.length, out);
      if (code !== 0) throw fail(code);
      return memory().slice(out, out + 32);
    });
  }

  function addressFromSeed(seed) {
    if (seed.length !== SEED_LEN) throw new Error("a seed is 32 bytes");
    return withBuffers([SEED_LEN, 32], (input, out) => {
      memory().set(seed, input);
      const code = x.stein_address_from_seed(input, out);
      if (code !== 0) throw fail(code);
      return encodeAddress(memory().slice(out, out + 32));
    });
  }

  /**
   * Signs a transfer, paying the minimum fee for its size. Draws the 32 bytes
   * of randomness each FN-DSA signature needs from the platform.
   */
  function signTransfer({ seed, chainId, nonce, expiresAt, minFeePerByte, to, amount, registerKey }) {
    if (seed.length !== SEED_LEN) throw new Error("a seed is 32 bytes");
    const recipient = decodeAddress(to);
    return withBuffers([TRANSFER_INPUT_LEN, TRANSFER_OUTPUT_CAP], (input, out) => {
      const bytes = new Uint8Array(TRANSFER_INPUT_LEN);
      const view = new DataView(bytes.buffer);
      bytes.set(seed, 0);
      crypto.getRandomValues(bytes.subarray(32, 64));
      view.setBigUint64(64, BigInt(chainId), true);
      view.setBigUint64(72, BigInt(nonce), true);
      view.setBigUint64(80, BigInt(expiresAt), true);
      setU128(view, 88, BigInt(minFeePerByte));
      bytes.set(recipient, 104);
      setU128(view, 136, BigInt(amount));
      bytes[152] = registerKey ? 1 : 0;
      memory().set(bytes, input);
      bytes.fill(0);

      const length = x.stein_transfer(input, out, TRANSFER_OUTPUT_CAP);
      if (length < 0) throw fail(length);
      const result = memory().slice(out, out + length);
      const fee = new DataView(result.buffer).getBigUint64(32, true) |
        (new DataView(result.buffer).getBigUint64(40, true) << 64n);
      return { id: hex(result.subarray(0, 32)), fee, raw: result.subarray(48) };
    });
  }

  /**
   * Signs a statement: proof to a site, not a transaction (see the core's
   * documentation). Returns the account's public key and the signature.
   */
  function signStatement({ seed, kind, payload }) {
    if (seed.length !== SEED_LEN) throw new Error("a seed is 32 bytes");
    const cap = PUBLIC_KEY_LEN + SIGNATURE_LEN;
    return withBuffers([SEED_LEN, 32, Math.max(payload.length, 1), cap], (s, r, p, out) => {
      memory().set(seed, s);
      memory().set(crypto.getRandomValues(new Uint8Array(32)), r);
      memory().set(payload, p);
      const length = x.stein_statement_sign(s, r, kind, p, payload.length, out, cap);
      if (length < 0) throw fail(length);
      const result = memory().slice(out, out + length);
      return { publicKey: result.subarray(0, PUBLIC_KEY_LEN), signature: result.subarray(PUBLIC_KEY_LEN) };
    });
  }

  /** Whether `signature` is `publicKey`'s signature of the statement. */
  function verifyStatement({ publicKey, kind, payload, signature }) {
    if (publicKey.length !== PUBLIC_KEY_LEN || signature.length !== SIGNATURE_LEN) return false;
    return withBuffers([PUBLIC_KEY_LEN, Math.max(payload.length, 1), SIGNATURE_LEN], (k, p, s) => {
      memory().set(publicKey, k);
      memory().set(payload, p);
      memory().set(signature, s);
      return x.stein_statement_verify(k, kind, p, payload.length, s) === 1;
    });
  }

  /** The address, in text form, that a public key controls. */
  function addressFromPublicKey(publicKey) {
    if (publicKey.length !== PUBLIC_KEY_LEN) throw new Error("not an FN-DSA-512 public key");
    return withBuffers([PUBLIC_KEY_LEN, 32], (input, out) => {
      memory().set(publicKey, input);
      const code = x.stein_address_from_public_key(input, out);
      if (code !== 0) throw fail(code);
      return encodeAddress(memory().slice(out, out + 32));
    });
  }

  return {
    addressFromSeed, encodeAddress, decodeAddress, signTransfer,
    signStatement, verifyStatement, addressFromPublicKey,
  };
}

function setU128(view, at, value) {
  if (value < 0n || value >= 1n << 128n) throw new Error("amount out of range");
  view.setBigUint64(at, value & 0xffff_ffff_ffff_ffffn, true);
  view.setBigUint64(at + 8, value >> 64n, true);
}
