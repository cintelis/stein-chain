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
    /// More than nine decimal places.
    #[error("STEIN has at most {AMOUNT_DECIMALS} decimal places")]
    Precision,
    /// Does not fit in 128 bits.
    #[error("amount too large")]
    Overflow,
}

/// Parses `"1.5"` into `1_500_000_000`.
pub fn parse_amount(text: &str) -> Result<Amount, AmountError> {
    let (whole, frac) = text.split_once('.').unwrap_or((text, ""));
    let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    if whole.is_empty() && frac.is_empty() || !digits(whole) || !digits(frac) {
        return Err(AmountError::Format);
    }
    if frac.len() > AMOUNT_DECIMALS as usize {
        return Err(AmountError::Precision);
    }
    let whole: Amount = if whole.is_empty() {
        0
    } else {
        whole.parse().map_err(|_| AmountError::Overflow)?
    };
    let padded = format!("{frac:0<9}");
    let frac: Amount = padded.parse().map_err(|_| AmountError::Format)?;
    whole
        .checked_mul(ONE_STEIN)
        .and_then(|w| w.checked_add(frac))
        .ok_or(AmountError::Overflow)
}

/// Formats base units as a decimal STEIN string without trailing zeros.
pub fn format_amount(amount: Amount) -> String {
    let whole = amount / ONE_STEIN;
    let frac = amount % ONE_STEIN;
    if frac == 0 {
        return whole.to_string();
    }
    let frac = format!("{frac:09}");
    format!("{whole}.{}", frac.trim_end_matches('0'))
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
}
