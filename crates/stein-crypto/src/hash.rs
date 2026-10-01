//! BLAKE3 hashing with mandatory domain separation, and the Merkle tree.

use std::fmt;

use borsh::{BorshDeserialize, BorshSerialize};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A 32-byte BLAKE3 digest.
#[derive(
    Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, BorshSerialize, BorshDeserialize,
)]
pub struct Hash(pub [u8; 32]);

impl Hash {
    /// The all-zero hash: the parent of the genesis block.
    pub const ZERO: Self = Self([0; 32]);

    /// The digest bytes.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Display for Hash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&hex::encode(self.0))
    }
}

impl fmt::Debug for Hash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Hash({self})")
    }
}

impl std::str::FromStr for Hash {
    type Err = hex::FromHexError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut out = [0u8; 32];
        hex::decode_to_slice(s.strip_prefix("0x").unwrap_or(s), &mut out)?;
        Ok(Self(out))
    }
}

impl Serialize for Hash {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        crate::hexser::serialize(&self.0, s)
    }
}

impl<'de> Deserialize<'de> for Hash {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let bytes = crate::hexser::deserialize_vec(d)?;
        let array = bytes
            .try_into()
            .map_err(|_| serde::de::Error::custom("a hash is 32 bytes"))?;
        Ok(Self(array))
    }
}

/// BLAKE3 in derive-key mode: `context` must be a hard-coded, globally unique
/// string of the form `"stein <purpose> v<N>"`. Every hash on the chain goes
/// through here; there is deliberately no context-free hash function.
pub fn hash(context: &'static str, parts: &[&[u8]]) -> Hash {
    let mut hasher = blake3::Hasher::new_derive_key(context);
    for part in parts {
        hasher.update(part);
    }
    Hash(*hasher.finalize().as_bytes())
}

const MERKLE_LEAF: &str = "stein merkle leaf v1";
const MERKLE_NODE: &str = "stein merkle node v1";
const MERKLE_EMPTY: &str = "stein merkle empty v1";

/// Merkle root over already-hashed leaves.
///
/// The tree is RFC 6962's: the left subtree takes the largest power of two
/// smaller than the leaf count. Leaves and interior nodes are hashed under
/// different contexts, and no leaf is ever duplicated to fill a level, so the
/// Bitcoin-style ambiguity (CVE-2012-2459) where two different leaf lists share
/// a root cannot arise.
pub fn merkle_root(leaves: &[Hash]) -> Hash {
    match leaves.len() {
        0 => hash(MERKLE_EMPTY, &[]),
        1 => hash(MERKLE_LEAF, &[&leaves[0].0]),
        n => {
            let split = n.next_power_of_two() / 2;
            let (left, right) = leaves.split_at(split);
            let (l, r) = if n >= 1024 {
                rayon::join(|| merkle_root(left), || merkle_root(right))
            } else {
                (merkle_root(left), merkle_root(right))
            };
            hash(MERKLE_NODE, &[&l.0, &r.0])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaf(i: u8) -> Hash {
        Hash([i; 32])
    }

    #[test]
    fn contexts_separate_domains() {
        assert_ne!(hash("stein a v1", &[b"x"]), hash("stein b v1", &[b"x"]));
    }

    #[test]
    fn merkle_root_commits_to_order_and_count() {
        let three = [leaf(1), leaf(2), leaf(3)];
        let swapped = [leaf(1), leaf(3), leaf(2)];
        let duplicated = [leaf(1), leaf(2), leaf(3), leaf(3)];
        assert_ne!(merkle_root(&three), merkle_root(&swapped));
        assert_ne!(merkle_root(&three), merkle_root(&duplicated));
        assert_ne!(merkle_root(&[]), merkle_root(&[Hash::ZERO]));
    }

    #[test]
    fn a_single_leaf_is_not_its_own_root() {
        assert_ne!(merkle_root(&[leaf(7)]), leaf(7));
    }

    #[test]
    fn parallel_and_serial_paths_agree() {
        let leaves: Vec<Hash> = (0..3000u32)
            .map(|i| hash("stein test v1", &[&i.to_le_bytes()]))
            .collect();
        let n = leaves.len();
        let split = n.next_power_of_two() / 2;
        let expected = hash(
            MERKLE_NODE,
            &[
                &merkle_root(&leaves[..split]).0,
                &merkle_root(&leaves[split..]).0,
            ],
        );
        assert_eq!(merkle_root(&leaves), expected);
    }
}
