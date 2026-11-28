//! Text a person or a client types: addresses, assets, schemes, hashes and
//! amounts, and keys and signatures in their JSON form.
#![no_main]

use libfuzzer_sys::fuzz_target;
use stein_crypto::{Address, Hash, PublicKey, Scheme, Signature};
use stein_types::{Asset, MAX_TOKEN_DECIMALS, format_units, parse_units};

fuzz_target!(|data: &[u8]| {
    let Some((&first, rest)) = data.split_first() else {
        return;
    };
    let Ok(text) = std::str::from_utf8(rest) else {
        return;
    };
    if let Ok(address) = text.parse::<Address>() {
        // Only the case of its hex digits may differ from the canonical form.
        assert!(address.to_string().eq_ignore_ascii_case(text));
        assert_eq!(address.to_string().parse::<Address>(), Ok(address));
    }
    if let Ok(asset) = text.parse::<Asset>() {
        assert_eq!(asset.to_string().parse::<Asset>(), Ok(asset));
    }
    if let Ok(scheme) = text.parse::<Scheme>() {
        assert_eq!(scheme.to_string(), text);
    }
    let _ = text.parse::<Hash>();
    let _ = serde_json::from_str::<PublicKey>(text);
    let _ = serde_json::from_str::<Signature>(text);

    // Any precision is survived; a precision a token may have reads back.
    let _ = parse_units(text, u32::from(first));
    let decimals = u32::from(first % (MAX_TOKEN_DECIMALS + 1));
    if let Ok(amount) = parse_units(text, decimals) {
        assert_eq!(
            parse_units(&format_units(amount, decimals), decimals),
            Ok(amount)
        );
    }
});
