#![allow(clippy::arithmetic_side_effects)]
//! Known answers for FN-DSA-512, made from fixed seeds.
//!
//! They check two things:
//! * **Every platform makes the same keys and signatures from the same
//!   seeds.** Signing is the one step that uses floating point, and `fn-dsa`
//!   uses the processor's own on x86-64 and ARM64. The digests of the vectors
//!   are fixed below, and CI compares them on Linux, Windows and macOS. A
//!   platform that rounded differently would fail here.
//! * **A second implementation agrees.** `scripts/cross-check-fn-dsa.sh`
//!   gives the same vectors to the C implementation by the author of
//!   `fn-dsa`. It must make the same keys and signatures, and accept and
//!   reject the same ones. That code is fetched for the check and is never
//!   part of a build (SPEC §3.5).
//!
//! The vectors hold for `fn-dsa` 0.4.0, which scheme tag 3 is pinned to. A
//! change to a digest below is a change to what the chain accepts.

use std::fmt::Write as _;
use std::ops::Range;

use rand_core::{CryptoRng, RngCore};

use super::{KEYGEN_STACK, SEED_LEN, SIGNATURE_LEN, Signer, Verifier, seed_rng};

/// What the vectors are derived from. Changing it changes every vector.
const CONTEXT: &str = "stein fn-dsa-512-draft test vectors v1";
const KEYS: usize = 8;
const MESSAGE_LENS: [usize; 8] = [0, 1, 31, 32, 33, 64, 255, 1000];
/// `fn-dsa` reads this much from its generator: once for a key pair, once for
/// a signature.
const KEYGEN_SEED_LEN: usize = 32;
const SIGN_SEED_LEN: usize = 40;
/// A signature is a header byte, a 40-byte nonce, then the compressed values
/// padded with zeros.
const NONCE: Range<usize> = 1..41;
const BODY: Range<usize> = 41..SIGNATURE_LEN;
const BODY_CHANGES: usize = 6;

const KEYS_DIGEST: &str = "f330095d315cead32844bfa93a20ff07fe3a857c9054a9b8fd1426c48d04dd43";
const SIGNATURES_DIGEST: &str = "a382137c454a691d71c82739a3bcdeb6b7d902923b182821db8efefaad680476";
const VERDICTS_DIGEST: &str = "06b102bbfeac8b20fd1cba16ada4530b78143245e2514dee782515ff5518e882";

/// Gives `fn-dsa` the seed it asks for when it signs, and nothing else.
struct Fixed([u8; SIGN_SEED_LEN]);

impl RngCore for Fixed {
    fn next_u32(&mut self) -> u32 {
        unreachable!("fn-dsa asks for bytes")
    }
    fn next_u64(&mut self) -> u64 {
        unreachable!("fn-dsa asks for bytes")
    }
    fn fill_bytes(&mut self, dest: &mut [u8]) {
        dest.copy_from_slice(&self.0);
    }
    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand_core::Error> {
        self.fill_bytes(dest);
        Ok(())
    }
}

impl CryptoRng for Fixed {}

/// The vectors as text, a line each. `scripts/cross-check-fn-dsa/check.c`
/// reads the same lines.
#[derive(Default)]
struct Vectors {
    /// `key <index> <fn-dsa seed> <secret key> <public key>`
    keys: String,
    /// `sig <key index> <seed> <message> <signature>`
    signatures: String,
    /// `ver <key index> <message> <signature> <1 if it verifies, else 0>`
    verdicts: String,
}

/// Hex, with `-` for nothing so that every field is a word.
fn word(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        "-".into()
    } else {
        hex::encode(bytes)
    }
}

/// Records what `verifier` says, which must be `expected`.
fn verdict(
    out: &mut String,
    index: usize,
    verifier: &Verifier,
    message: &[u8],
    signature: &[u8],
    expected: bool,
) {
    assert_eq!(
        verifier.verify(message, signature),
        expected,
        "key {index}, message {}, signature {}",
        word(message),
        word(signature)
    );
    writeln!(
        out,
        "ver {index} {} {} {}",
        word(message),
        word(signature),
        u8::from(expected)
    )
    .expect("write");
}

fn build() -> Vectors {
    let mut stream = blake3::Hasher::new_derive_key(CONTEXT).finalize_xof();
    let mut take = |len: usize| {
        let mut bytes = vec![0u8; len];
        stream.fill(&mut bytes);
        bytes
    };
    let mut out = Vectors::default();

    let mut keys = Vec::new();
    for index in 0..KEYS {
        let seed: [u8; SEED_LEN] = take(SEED_LEN).try_into().expect("seed");
        // The seed `fn-dsa` works from is the first thing it reads.
        let mut keygen_seed = [0u8; KEYGEN_SEED_LEN];
        seed_rng(&seed).fill_bytes(&mut keygen_seed);
        let (secret, public) = Signer::encoded_pair(&seed);
        let (signer, same) = Signer::keygen(&seed).expect("key");
        assert_eq!(public, same);
        writeln!(
            out.keys,
            "key {index} {} {} {}",
            word(&keygen_seed),
            word(&secret),
            word(&public)
        )
        .expect("write");
        keys.push((signer, Verifier::new(&public).expect("public key")));
    }

    for (index, (signer, verifier)) in keys.iter().enumerate() {
        let other = (index + 1) % KEYS;
        for len in MESSAGE_LENS {
            let message = take(len);
            let seed: [u8; SIGN_SEED_LEN] = take(SIGN_SEED_LEN).try_into().expect("seed");
            let signature = signer
                .sign_with(&mut Fixed(seed), &message)
                .expect("signature");
            let again = signer
                .sign_with(&mut Fixed(seed), &message)
                .expect("signature");
            assert_eq!(signature, again, "one seed, one signature");
            writeln!(
                out.signatures,
                "sig {index} {} {} {}",
                word(&seed),
                word(&message),
                word(&signature)
            )
            .expect("write");

            let v = &mut out.verdicts;
            verdict(v, index, verifier, &message, &signature, true);
            verdict(v, other, &keys[other].1, &message, &signature, false);

            let mut changed = message.clone();
            match changed.first_mut() {
                Some(byte) => *byte ^= 1,
                None => changed.push(0),
            }
            verdict(v, index, verifier, &changed, &signature, false);

            // One bit changed: in the header, in the nonce, in the body, and
            // in the last byte, which is padding.
            let mut pick = |range: Range<usize>| {
                let n = u32::from_le_bytes(take(4).try_into().expect("four bytes"));
                range.start + n as usize % range.len()
            };
            let mut places = vec![0, pick(NONCE), SIGNATURE_LEN - 1];
            places.extend((0..BODY_CHANGES).map(|_| pick(BODY)));
            for place in places {
                let bit = 1u8 << (pick(0..8) as u32);
                let mut changed = signature.clone();
                changed[place] ^= bit;
                verdict(v, index, verifier, &message, &changed, false);
            }

            let short = &signature[..SIGNATURE_LEN - 1];
            verdict(v, index, verifier, &message, short, false);
            let mut long = signature.clone();
            long.push(0);
            verdict(v, index, verifier, &message, &long, false);
            verdict(v, index, verifier, &message, &take(SIGNATURE_LEN), false);
            verdict(v, index, verifier, &message, &[], false);
        }
    }
    out
}

fn digest(text: &str) -> String {
    blake3::hash(text.as_bytes()).to_hex().to_string()
}

#[test]
fn every_platform_makes_the_same_keys_and_signatures() {
    // Key generation needs more stack than a test thread has.
    let vectors = std::thread::Builder::new()
        .stack_size(KEYGEN_STACK)
        .spawn(build)
        .expect("thread")
        .join()
        .expect("vectors");

    // For scripts/cross-check-fn-dsa.sh. Written before the comparison, so
    // that a platform which disagrees can still be put beside the C code.
    if let Some(path) = std::env::var_os("STEIN_FN_DSA_VECTORS") {
        let text = [&*vectors.keys, &*vectors.signatures, &*vectors.verdicts].concat();
        std::fs::write(path, text).expect("write the vectors");
    }

    assert_eq!(
        [
            ("keys", &*digest(&vectors.keys)),
            ("signatures", &*digest(&vectors.signatures)),
            ("verdicts", &*digest(&vectors.verdicts)),
        ],
        [
            ("keys", KEYS_DIGEST),
            ("signatures", SIGNATURES_DIGEST),
            ("verdicts", VERDICTS_DIGEST),
        ],
        "this platform's FN-DSA-512 keys or signatures differ from the fixed vectors"
    );
}
