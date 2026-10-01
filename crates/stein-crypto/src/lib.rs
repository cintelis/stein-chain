//! Cryptographic primitives for Stein Chain.
//!
//! Policy: **post-quantum only.** Every signature the chain creates or accepts is
//! ML-DSA (FIPS 204): ML-DSA-44 or ML-DSA-65, tagged per key (see [`Scheme`]).
//! There is no enum variant, code path or configuration switch that selects a
//! classical scheme. Hashing is BLAKE3 with
//! a distinct derive-key context per use, so a hash computed for one purpose can
//! never be replayed as a hash for another (SPEC §3).

mod address;
#[cfg(feature = "fn-dsa-experimental")]
mod fn_dsa_draft;
mod hash;
mod keys;
mod seed_file;

pub use address::{Address, AddressParseError};
pub use hash::{Hash, hash, merkle_root};
pub use keys::{
    Domain, InvalidSignature, KeyError, PublicKey, SEED_LEN, Scheme, SecretKey, Signature,
    VerifyingKey, verify_batch,
};
pub use seed_file::{SeedFileError, create_seed_file, secret_key_from_seed_file};

pub(crate) mod hexser {
    //! Hex encoding for serde, used by every fixed-size byte type.

    use serde::de::Error as _;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&hex::encode(bytes))
    }

    pub fn deserialize_vec<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let text = <String as Deserialize>::deserialize(d)
            .map_err(|_| D::Error::custom("expected a hex string"))?;
        hex::decode(text.strip_prefix("0x").unwrap_or(&text)).map_err(D::Error::custom)
    }
}
