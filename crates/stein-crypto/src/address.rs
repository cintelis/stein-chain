//! Account addresses: a domain-separated BLAKE3 hash of a scheme tag and a
//! public key. The tag is hashed in, so one key can never control accounts
//! under two schemes.

use std::fmt;
use std::str::FromStr;

use borsh::{BorshDeserialize, BorshSerialize};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::hash::hash;
use crate::keys::PublicKey;

const ADDRESS_CONTEXT: &str = "stein address v2";
const CHECKSUM_CONTEXT: &str = "stein address checksum v1";
const SYSTEM_CONTEXT: &str = "stein system account v1";
const PREFIX: &str = "st";
const CHECKSUM_LEN: usize = 4;

/// A 32-byte account address.
///
/// Text form: `st` + 64 hex characters + an 8-hex-character checksum. The
/// checksum catches typos before a transfer is signed; it is never on chain.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, BorshSerialize, BorshDeserialize)]
pub struct Address(pub [u8; 32]);

impl Address {
    /// The address that `key` controls.
    pub fn from_public_key(key: &PublicKey) -> Self {
        Self(hash(ADDRESS_CONTEXT, &[&[key.scheme().tag()], key.as_bytes()]).0)
    }

    /// The address of a protocol-owned system account, e.g. the fee pool.
    /// Derived in its own hash domain, so no key can ever control it.
    pub fn system(name: &str) -> Self {
        Self(hash(SYSTEM_CONTEXT, &[name.as_bytes()]).0)
    }

    fn checksum(&self) -> [u8; CHECKSUM_LEN] {
        let digest = hash(CHECKSUM_CONTEXT, &[&self.0]);
        let mut out = [0u8; CHECKSUM_LEN];
        out.copy_from_slice(&digest.0[..CHECKSUM_LEN]);
        out
    }
}

/// Why a string is not an address.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AddressParseError {
    /// Missing the `st` prefix.
    #[error("an address starts with '{PREFIX}'")]
    Prefix,
    /// Wrong length or not hex.
    #[error("an address is '{PREFIX}' followed by 72 hex characters")]
    Format,
    /// The checksum does not match: almost certainly a typo.
    #[error("address checksum mismatch (typo?)")]
    Checksum,
}

impl fmt::Display for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{PREFIX}{}{}",
            hex::encode(self.0),
            hex::encode(self.checksum())
        )
    }
}

impl fmt::Debug for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Address({self})")
    }
}

impl FromStr for Address {
    type Err = AddressParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let body = s.strip_prefix(PREFIX).ok_or(AddressParseError::Prefix)?;
        let mut raw = [0u8; 32 + CHECKSUM_LEN];
        hex::decode_to_slice(body, &mut raw).map_err(|_| AddressParseError::Format)?;
        let (addr, check) = raw.split_at(32);
        let mut bytes = [0u8; 32];
        bytes.copy_from_slice(addr);
        let address = Self(bytes);
        if address.checksum() != check {
            return Err(AddressParseError::Checksum);
        }
        Ok(address)
    }
}

impl Serialize for Address {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Address {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let text = <String as Deserialize>::deserialize(d)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_form_round_trips_and_catches_typos() {
        let address = Address([0xab; 32]);
        let text = address.to_string();
        assert_eq!(text.len(), 2 + 64 + 8);
        assert_eq!(text.parse::<Address>(), Ok(address));

        let mut typo = text.into_bytes();
        typo[10] = if typo[10] == b'0' { b'1' } else { b'0' };
        let typo = String::from_utf8(typo).unwrap_or_default();
        assert_eq!(typo.parse::<Address>(), Err(AddressParseError::Checksum));
        assert_eq!("0xabc".parse::<Address>(), Err(AddressParseError::Prefix));
    }
}
