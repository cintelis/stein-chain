//! STEIN amounts: integers of base units, never floating point.

/// An amount of base units. 1 STEIN = 10^9 base units.
pub type Amount = u128;

/// Decimal places of one STEIN.
pub const AMOUNT_DECIMALS: u32 = 9;

/// One STEIN in base units.
pub const ONE_STEIN: Amount = 1_000_000_000;

/// Why a decimal string is not an amount.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AmountError {
    /// Not a plain non-negative decimal number.
    #[error("not a decimal amount")]
    Format,
    /// More decimal places than the asset has (nine for STEIN).
    #[error("more decimal places than the asset has")]
    Precision,
    /// Does not fit in 128 bits.
    #[error("amount too large")]
    Overflow,
}

/// Parses `"1.5"` into `1_500_000_000`.
pub fn parse_amount(text: &str) -> Result<Amount, AmountError> {
    parse_units(text, AMOUNT_DECIMALS)
}

/// Formats base units as a decimal STEIN string without trailing zeros.
pub fn format_amount(amount: Amount) -> String {
    format_units(amount, AMOUNT_DECIMALS)
}

/// Parses a decimal string into base units of an asset with `decimals`
/// decimal places.
pub fn parse_units(text: &str, decimals: u32) -> Result<Amount, AmountError> {
    let (whole, frac) = text.split_once('.').unwrap_or((text, ""));
    let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    if whole.is_empty() && frac.is_empty() || !digits(whole) || !digits(frac) {
        return Err(AmountError::Format);
    }
    if frac.len() > decimals as usize {
        return Err(AmountError::Precision);
    }
    let whole: Amount = if whole.is_empty() {
        0
    } else {
        whole.parse().map_err(|_| AmountError::Overflow)?
    };
    let padded = format!("{frac:0<width$}", width = decimals as usize);
    let frac: Amount = if padded.is_empty() {
        0
    } else {
        padded.parse().map_err(|_| AmountError::Overflow)?
    };
    10u128
        .checked_pow(decimals)
        .and_then(|one| whole.checked_mul(one))
        .and_then(|w| w.checked_add(frac))
        .ok_or(AmountError::Overflow)
}

/// Formats base units of an asset with `decimals` decimal places, without
/// trailing zeros.
pub fn format_units(amount: Amount, decimals: u32) -> String {
    let Some(one) = 10u128.checked_pow(decimals).filter(|_| decimals > 0) else {
        return amount.to_string();
    };
    let whole = amount.checked_div(one).unwrap_or(0);
    let frac = amount.checked_rem(one).unwrap_or(0);
    if frac == 0 {
        return whole.to_string();
    }
    let frac = format!("{frac:0width$}", width = decimals as usize);
    format!("{whole}.{}", frac.trim_end_matches('0'))
}

/// Amounts in JSON as decimal strings: 128 bits do not fit a JSON number.
/// Reads a string or, for small values, a number.
pub mod string {
    use serde::{Deserialize, Deserializer, Serializer};

    use super::Amount;

    /// Writes the amount as a decimal string.
    pub fn serialize<S: Serializer>(amount: &Amount, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(amount)
    }

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Text {
        String(String),
        Number(u64),
    }

    /// Reads a decimal string, or a number that fits 64 bits.
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Amount, D::Error> {
        match Text::deserialize(d)? {
            Text::String(text) => text.parse().map_err(serde::de::Error::custom),
            Text::Number(n) => Ok(Amount::from(n)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_formats() {
        assert_eq!(parse_amount("1.5"), Ok(1_500_000_000));
        assert_eq!(parse_amount("0.000000001"), Ok(1));
        assert_eq!(parse_amount(".25"), Ok(250_000_000));
        assert_eq!(parse_amount("7"), Ok(7 * ONE_STEIN));
        assert_eq!(parse_amount("1.0000000001"), Err(AmountError::Precision));
        assert_eq!(parse_amount("-1"), Err(AmountError::Format));
        assert_eq!(parse_amount("1e9"), Err(AmountError::Format));
        assert_eq!(parse_amount("."), Err(AmountError::Format));
        assert_eq!(
            parse_amount("999999999999999999999999999999999999999"),
            Err(AmountError::Overflow)
        );
        assert_eq!(format_amount(1_500_000_000), "1.5");
        assert_eq!(format_amount(1), "0.000000001");
        assert_eq!(format_amount(3 * ONE_STEIN), "3");
    }

    #[test]
    fn other_decimal_places() {
        assert_eq!(parse_units("12", 0), Ok(12));
        assert_eq!(parse_units("1.5", 0), Err(AmountError::Precision));
        assert_eq!(parse_units("1.5", 2), Ok(150));
        assert_eq!(parse_units("0.000000000000000001", 18), Ok(1));
        assert_eq!(format_units(12, 0), "12");
        assert_eq!(format_units(150, 2), "1.5");
        assert_eq!(format_units(1, 18), "0.000000000000000001");
        assert_eq!(format_units(u128::MAX, 0), u128::MAX.to_string());
        assert_eq!(
            parse_units("340282366920938463463374607431768211456", 0),
            Err(AmountError::Overflow)
        );
    }
}
