// The sealed message: one fixed size, whatever the text and whether or not
// it is signed. Shared by the page (which seals), the Worker (which stores)
// and the inbox (which opens).
//
//   version u8 ‖ ML-KEM-768 ciphertext (1088) ‖ nonce (12) ‖ AES-256-GCM(padded) (PADDED + 16)
//
// Version 2, what is sealed now. The padded part is
//
//   signed u8 (0 or 1) ‖ length u16 LE ‖ UTF-8 text ‖ [signer's public key (897) ‖ signature (666)] ‖ zeros
//
// so neither the size of the message nor whether a wallet signed it shows
// from outside. Version 1 (4096 padded bytes: length u16 LE ‖ text ‖ zeros,
// never signed) is no longer sealed or accepted, but the inbox still opens it.

export const VERSION = 2;
export const PUBLIC_KEY_BYTES = 1184;
export const SECRET_KEY_BYTES = 2400;
export const KEM_BYTES = 1088;
export const NONCE_BYTES = 12;
export const TAG_BYTES = 16;
export const MAX_TEXT_BYTES = 4000;
export const PADDED_BYTES = 6144;
export const ENVELOPE_BYTES = 1 + KEM_BYTES + NONCE_BYTES + PADDED_BYTES + TAG_BYTES;

/** A signer's FN-DSA-512 public key and signature, as the wallet core gives them. */
export const SIGNER_KEY_BYTES = 897;
export const SIGNATURE_BYTES = 666;

export const V1_PADDED_BYTES = 4096;
export const V1_ENVELOPE_BYTES = 1 + KEM_BYTES + NONCE_BYTES + V1_PADDED_BYTES + TAG_BYTES;
