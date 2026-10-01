//! FN-DSA-512 (Falcon), per the **draft** of FIPS 206. EXPERIMENTAL.
//!
//! Compiled only with the `fn-dsa-experimental` feature, and accepted only on
//! a network whose genesis lists `fn-dsa-512-draft` in `tx_schemes` (the
//! FN-DSA test network, SPEC §3.5). Standard builds contain none of this, and
//! scripts/check-crypto-policy.sh fails if they ever do.
//!
//! Why it is fenced off:
//! * FIPS 206 is unpublished. The `fn-dsa` crate (0.4) implements a best guess
//!   at the draft and warns that keys and signatures may stop being accepted by
//!   later versions. A chain that accepted them could not replay its own
//!   history after an upgrade.
//! * It is a second signature implementation beside AWS-LC, outside any FIPS
//!   validation.
//! * Signing samples a discrete Gaussian, which is hard to make constant-time;
//!   wallets sign, and a leaked key is stolen funds.
//!
//! Keys are derived from the same 32-byte seed files as ML-DSA keys, through a
//! BLAKE3 XOF used as a deterministic generator for `fn-dsa`'s key generation.
//! FIPS 206 may define its own seed format, so FN-DSA seed files here are
//! Stein-specific and not portable to other wallets.

use std::sync::{Arc, Mutex};

use fn_dsa::{
    DOMAIN_NONE, FN_DSA_LOGN_512, HASH_ID_RAW, KeyPairGenerator, KeyPairGenerator512,
    SigningKey as _, SigningKey512, VerifyingKey as _, VerifyingKey512, sign_key_size,
    signature_size, vrfy_key_size,
};
use rand_core::{CryptoRng, OsRng, RngCore};
use zeroize::Zeroize;

use crate::keys::{KeyError, SEED_LEN};

/// Encoded public key length for FN-DSA-512.
pub const PUBLIC_KEY_LEN: usize = vrfy_key_size(FN_DSA_LOGN_512);
/// Signature length for FN-DSA-512 (this implementation pads to a fixed size).
pub const SIGNATURE_LEN: usize = signature_size(FN_DSA_LOGN_512);

const KEYGEN_CONTEXT: &str = "stein fn-dsa-512-draft keygen v1";
const KEYGEN_STACK: usize = 16 * 1024 * 1024;

/// A deterministic generator: BLAKE3 in derive-key mode over the seed, read as
/// an extendable output. Used only to turn a seed into a key pair.
struct SeedRng(blake3::OutputReader);

impl RngCore for SeedRng {
    fn next_u32(&mut self) -> u32 {
        let mut b = [0u8; 4];
        self.0.fill(&mut b);
        u32::from_le_bytes(b)
    }
    fn next_u64(&mut self) -> u64 {
        let mut b = [0u8; 8];
        self.0.fill(&mut b);
        u64::from_le_bytes(b)
    }
    fn fill_bytes(&mut self, dest: &mut [u8]) {
        self.0.fill(dest);
    }
    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand_core::Error> {
        self.0.fill(dest);
        Ok(())
    }
}

impl CryptoRng for SeedRng {}

/// A parsed FN-DSA public key.
/// Boxed: the key carries working buffers too large to move around the stack.
pub struct Verifier(Box<VerifyingKey512>);

impl Verifier {
    pub fn new(public: &[u8]) -> Result<Self, KeyError> {
        VerifyingKey512::decode(public)
            .map(|k| Self(Box::new(k)))
            .ok_or(KeyError::Crypto)
    }

    pub fn verify(&self, message: &[u8], signature: &[u8]) -> bool {
        self.0
            .verify(signature, &DOMAIN_NONE, &HASH_ID_RAW, message)
    }
}

/// An FN-DSA private key. Signing mutates internal state, hence the mutex.
///
/// Boxed: the key carries working buffers too large to move around the stack.
pub struct Signer(Mutex<Box<SigningKey512>>);

impl Signer {
    /// The key pair derived from `seed`: (signer, encoded public key).
    ///
    /// Key generation keeps several large buffers on the stack, more than a
    /// default thread stack holds on some platforms (1 MiB for Windows' main
    /// thread), so it runs on a thread of its own with room to spare.
    pub fn from_seed(seed: &[u8; SEED_LEN]) -> Result<(Self, Vec<u8>), KeyError> {
        let seed = *seed;
        std::thread::Builder::new()
            .name("fn-dsa-keygen".into())
            .stack_size(KEYGEN_STACK)
            .spawn(move || {
                let mut seed = seed;
                let pair = Self::keygen(&seed);
                seed.zeroize();
                pair
            })
            .map_err(|_| KeyError::Crypto)?
            .join()
            .map_err(|_| KeyError::Crypto)?
    }

    fn keygen(seed: &[u8; SEED_LEN]) -> Result<(Self, Vec<u8>), KeyError> {
        let mut hasher = blake3::Hasher::new_derive_key(KEYGEN_CONTEXT);
        hasher.update(seed);
        let mut rng = SeedRng(hasher.finalize_xof());
        let mut secret = vec![0u8; sign_key_size(FN_DSA_LOGN_512)];
        let mut public = vec![0u8; PUBLIC_KEY_LEN];
        KeyPairGenerator512::default().keygen(FN_DSA_LOGN_512, &mut rng, &mut secret, &mut public);
        let key = SigningKey512::decode(&secret).map(Box::new);
        secret.zeroize();
        let key = key.ok_or(KeyError::Crypto)?;
        Ok((Self(Mutex::new(key)), public))
    }

    /// Signs with fresh randomness from the operating system.
    pub fn sign(&self, message: &[u8]) -> Result<Vec<u8>, KeyError> {
        let mut signature = vec![0u8; SIGNATURE_LEN];
        let mut key = self.0.lock().map_err(|_| KeyError::Crypto)?;
        key.sign(
            &mut OsRng,
            &DOMAIN_NONE,
            &HASH_ID_RAW,
            message,
            &mut signature,
        )
        .ok_or(KeyError::Crypto)?;
        Ok(signature)
    }
}

/// Shared parsed key, cheap to clone like the AWS-LC one.
pub type SharedVerifier = Arc<Verifier>;
