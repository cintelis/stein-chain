// The sealed message: one fixed size, whatever the text. Shared by the page
// (which seals), the Worker (which stores) and the inbox reader (which opens).
//
//   version u8 ‖ ML-KEM-768 ciphertext (1088) ‖ nonce (12) ‖ AES-256-GCM(padded text) (4096 + 16)
//
// The padded text is `length u16 LE ‖ UTF-8 text ‖ zeros`, so the size of an
// envelope says nothing about the size of the message in it.

export const VERSION = 1;
export const PUBLIC_KEY_BYTES = 1184;
export const SECRET_KEY_BYTES = 2400;
export const KEM_BYTES = 1088;
export const NONCE_BYTES = 12;
export const PADDED_BYTES = 4096;
export const TAG_BYTES = 16;
export const MAX_TEXT_BYTES = 4000;
export const ENVELOPE_BYTES = 1 + KEM_BYTES + NONCE_BYTES + PADDED_BYTES + TAG_BYTES;
