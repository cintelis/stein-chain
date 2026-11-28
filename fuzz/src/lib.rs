//! What the fuzz targets share.

use borsh::{BorshDeserialize, BorshSerialize};

/// Decodes `bytes` as a `T`. If they decode, they must be the one encoding of
/// the value: two encodings of one value would let two nodes hash it two ways.
pub fn canonical<T: BorshDeserialize + BorshSerialize>(bytes: &[u8]) -> Option<T> {
    let value = borsh::from_slice::<T>(bytes).ok()?;
    assert_eq!(
        borsh::to_vec(&value).expect("encode"),
        bytes,
        "a value decoded from bytes that are not its encoding"
    );
    Some(value)
}

/// `value` must read back from its own JSON.
pub fn json_round_trip<T>(value: &T)
where
    T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let text = serde_json::to_string(value).expect("json");
    let back: T = serde_json::from_str(&text).expect("a value does not read back from its JSON");
    assert_eq!(&back, value);
}
