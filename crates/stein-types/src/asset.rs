//! Assets other than STEIN: tokens anyone can issue, and the liquidity pools
//! that trade one asset for another (SPEC §4.6, §4.7).
//!
//! Both are native transaction types, not contracts. A token and a pool each
//! live at an address no key controls, so their state is keyed by address like
//! everything else (SHARD-READY I-4).

use std::fmt;
use std::str::FromStr;

use borsh::{BorshDeserialize, BorshSerialize};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use stein_crypto::Address;

use crate::math::{mul_div, mul_div_up, sqrt_mul};
use crate::{AMOUNT_DECIMALS, Amount};

/// Something an account can hold: STEIN, or a token identified by its address.
/// A pool's shares are held like a token, under the pool's address.
///
/// The order (STEIN first, then tokens by address) is the canonical order of a
/// pair: every pair of assets has exactly one pool.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, BorshSerialize, BorshDeserialize,
)]
pub enum Asset {
    /// The native coin.
    Stein,
    /// A token, or a pool's shares, by address.
    Token(Address),
}

const STEIN_NAME: &str = "stein";

impl Asset {
    /// Canonical encoding, as hashed into a pool's address.
    pub fn encode(&self) -> Vec<u8> {
        borsh::to_vec(self).unwrap_or_default()
    }
}

impl fmt::Display for Asset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stein => f.write_str(STEIN_NAME),
            Self::Token(address) => address.fmt(f),
        }
    }
}

impl FromStr for Asset {
    type Err = stein_crypto::AddressParseError;

    /// `stein` (any case), or a token's address.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.eq_ignore_ascii_case(STEIN_NAME) {
            return Ok(Self::Stein);
        }
        s.parse().map(Self::Token)
    }
}

impl Serialize for Asset {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Asset {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let text = <String as Deserialize>::deserialize(d)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

/// The address of the pool that trades `a` for `b`, in either order.
pub fn pool_address(a: Asset, b: Asset) -> Address {
    let (first, second) = if a <= b { (a, b) } else { (b, a) };
    Address::pool(&first.encode(), &second.encode())
}

/// Longest token symbol, in bytes.
pub const MAX_SYMBOL_LEN: usize = 12;
/// Most decimal places a token may declare.
pub const MAX_TOKEN_DECIMALS: u8 = 18;

/// Whether `symbol` may name a token: 1 to 12 characters, capital letters and
/// digits, starting with a letter, and not `STEIN`. Symbols are labels, not
/// identities: two tokens may share one, and only the address tells them apart.
pub fn valid_symbol(symbol: &str) -> bool {
    let bytes = symbol.as_bytes();
    (1..=MAX_SYMBOL_LEN).contains(&bytes.len())
        && bytes.first().is_some_and(u8::is_ascii_uppercase)
        && bytes
            .iter()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
        && symbol != "STEIN"
}

/// A token, as recorded on chain when it is issued. Nothing in it ever changes:
/// the supply is fixed, and there is no authority that could mint or freeze.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct Token {
    /// The account that issued it and received the whole supply.
    pub issuer: Address,
    /// Display label; not unique.
    pub symbol: String,
    /// Decimal places wallets show.
    pub decimals: u8,
    /// Total supply in base units: fixed at issue, below 2^127.
    #[serde(with = "crate::amount::string")]
    pub supply: Amount,
}

/// A liquidity pool. Its reserves are the balances of its own address, so they
/// are not repeated here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct Pool {
    /// First asset of the pair, in canonical order.
    pub asset_a: Asset,
    /// Second asset.
    pub asset_b: Asset,
    /// Shares in existence, including the locked [`MIN_LIQUIDITY`].
    #[serde(with = "crate::amount::string")]
    pub shares: Amount,
}

/// The fee a swap leaves in the pool, in thousandths of the amount paid in.
/// It stays in the reserves, so it accrues to the pool's shareholders.
pub const SWAP_FEE_PER_MILLE: Amount = 3;

/// Shares of every pool that are locked for good when it is first funded,
/// held by the pool itself. They keep a pool from being emptied back to zero
/// shares, and make it costly to inflate the value of one share enough to
/// round a later depositor's shares down to nothing.
pub const MIN_LIQUIDITY: Amount = 1_000;

/// What `amount_in` buys from a pool with these reserves: constant product,
/// after the fee, rounded down. An empty pool pays nothing. `None` only if
/// the amounts are out of range.
pub fn swap_output(reserve_in: Amount, reserve_out: Amount, amount_in: Amount) -> Option<Amount> {
    let fee = mul_div_up(amount_in, SWAP_FEE_PER_MILLE, 1_000)?;
    let net = amount_in.checked_sub(fee)?;
    mul_div(net, reserve_out, reserve_in.checked_add(net)?)
}

/// Shares for the deposit that first funds a pool: the geometric mean of the
/// two amounts, of which [`MIN_LIQUIDITY`] are then locked.
pub fn first_shares(amount_a: Amount, amount_b: Amount) -> Amount {
    sqrt_mul(amount_a, amount_b)
}

/// What a deposit into a funded pool turns into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Deposit {
    /// Shares created for the depositor.
    pub shares: Amount,
    /// How much of the first asset the pool takes.
    pub used_a: Amount,
    /// How much of the second.
    pub used_b: Amount,
}

/// Shares for a deposit of up to `amount_a` and `amount_b` into a pool with
/// these reserves and shares. The pool takes the two assets in its current
/// ratio: the scarcer side sets the shares (rounded down), and each amount
/// taken is rounded up, never above what was offered.
pub fn deposit(
    reserve_a: Amount,
    reserve_b: Amount,
    total_shares: Amount,
    amount_a: Amount,
    amount_b: Amount,
) -> Option<Deposit> {
    let shares = mul_div(amount_a, total_shares, reserve_a)?.min(mul_div(
        amount_b,
        total_shares,
        reserve_b,
    )?);
    let used_a = mul_div_up(shares, reserve_a, total_shares)?;
    let used_b = mul_div_up(shares, reserve_b, total_shares)?;
    (used_a <= amount_a && used_b <= amount_b).then_some(Deposit {
        shares,
        used_a,
        used_b,
    })
}

/// What `shares` of a pool redeem for: the same fraction of each reserve,
/// rounded down.
pub fn withdrawal(
    reserve_a: Amount,
    reserve_b: Amount,
    total_shares: Amount,
    shares: Amount,
) -> Option<(Amount, Amount)> {
    Some((
        mul_div(shares, reserve_a, total_shares)?,
        mul_div(shares, reserve_b, total_shares)?,
    ))
}

/// Why a pool did not carry out an operation. The transaction is still valid
/// and still pays its fee; what it sent to the pool is returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(rename_all = "snake_case")]
pub enum PoolError {
    /// The pair has no funded pool.
    #[error("there is no funded pool for this pair")]
    NoPool,
    /// A first deposit too small to leave shares beyond the locked minimum.
    #[error("the first deposit into a pool is too small")]
    TooLittleLiquidity,
    /// The result is below the minimum the sender set.
    #[error("the result is below the minimum asked for")]
    BelowMinimum,
    /// The operation would give the sender nothing.
    #[error("the amount is too small to give anything back")]
    Nothing,
    /// The amounts do not fit the pool's arithmetic.
    #[error("the amounts are out of range for this pool")]
    OutOfRange,
}

/// What a transaction in a block did, beyond what its body says. Derived by
/// executing the block; every node derives the same.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum Outcome {
    /// Carried out exactly as the body says.
    Applied,
    /// A token was created.
    TokenIssued {
        /// Its address.
        token: Address,
    },
    /// Liquidity was added.
    LiquidityAdded {
        /// The pool.
        pool: Address,
        /// Shares the sender received.
        #[serde(with = "crate::amount::string")]
        shares: Amount,
        /// How much of the first asset the pool took; the rest came back.
        #[serde(with = "crate::amount::string")]
        used_a: Amount,
        /// How much of the second.
        #[serde(with = "crate::amount::string")]
        used_b: Amount,
    },
    /// Liquidity was removed.
    LiquidityRemoved {
        /// First asset paid out.
        #[serde(with = "crate::amount::string")]
        amount_a: Amount,
        /// Second asset paid out.
        #[serde(with = "crate::amount::string")]
        amount_b: Amount,
    },
    /// A swap was filled.
    Swapped {
        /// What the sender received.
        #[serde(with = "crate::amount::string")]
        amount_out: Amount,
    },
    /// The pool did not carry the operation out, and returned what was sent.
    Refunded {
        /// Why.
        reason: PoolError,
    },
}

/// Decimal places of an asset's display form, given the token if it is one.
pub fn decimals_of(asset: &Asset, token: Option<&Token>) -> u32 {
    match asset {
        Asset::Stein => AMOUNT_DECIMALS,
        Asset::Token(_) => token.map_or(0, |t| u32::from(t.decimals)),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::arithmetic_side_effects)]
    use super::*;

    fn token(n: u8) -> Asset {
        Asset::Token(Address([n; 32]))
    }

    #[test]
    fn assets_have_one_text_form_and_one_order() {
        assert_eq!("stein".parse(), Ok(Asset::Stein));
        assert_eq!("STEIN".parse(), Ok(Asset::Stein));
        let t = token(7);
        assert_eq!(t.to_string().parse(), Ok(t));
        assert!("steins".parse::<Asset>().is_err());
        assert!(Asset::Stein < token(0));
        assert!(token(1) < token(2));
        assert_eq!(
            serde_json::to_string(&Asset::Stein).expect("json"),
            "\"stein\""
        );
    }

    #[test]
    fn a_pair_has_one_pool_whichever_way_round_it_is_named() {
        let (a, b) = (token(1), token(2));
        assert_eq!(pool_address(a, b), pool_address(b, a));
        assert_ne!(pool_address(a, b), pool_address(a, Asset::Stein));
        assert_ne!(pool_address(a, b), pool_address(a, token(3)));
        // Not a token's address, nor a key's.
        assert_ne!(pool_address(a, b), Address::token(&Address([1; 32]), 0));
    }

    #[test]
    fn symbols() {
        for ok in ["A", "USD", "GOLD2", "ABCDEFGHIJKL"] {
            assert!(valid_symbol(ok), "{ok}");
        }
        for bad in ["", "usd", "1INCH", "A B", "ABCDEFGHIJKLM", "STEIN", "É"] {
            assert!(!valid_symbol(bad), "{bad}");
        }
    }

    #[test]
    fn a_swap_pays_the_constant_product_price_less_the_fee() {
        // 1,000 in against 100,000 : 100,000. Fee 3, so 997 trade:
        // 997 × 100,000 ÷ 100,997 = 987.158…
        assert_eq!(swap_output(100_000, 100_000, 1_000), Some(987));
        // The fee rounds up, the output down: dust buys nothing.
        assert_eq!(swap_output(100_000, 100_000, 1), Some(0));
        assert_eq!(swap_output(0, 0, 5), Some(0));
        assert_eq!(swap_output(u128::MAX, 1, u128::MAX), None);
        // The product of the reserves never falls.
        for (x, y, dx) in [
            (1_000u128, 1_000u128, 10u128),
            (5, 7_000_000, 3),
            (10u128.pow(30), 3, 10u128.pow(29)),
        ] {
            let dy = swap_output(x, y, dx).expect("out");
            assert!(dy < y);
            assert!(
                crate::math::mul_wide(x + dx, y - dy) >= crate::math::mul_wide(x, y),
                "{x} {y} {dx}"
            );
        }
        // Amounts at the top of the range still price.
        let big = crate::MAX_TOTAL_SUPPLY / 2;
        assert!(swap_output(big, big, big).is_some());
    }

    #[test]
    fn a_deposit_takes_the_pool_ratio_and_never_more_than_offered() {
        // Pool 1,000 : 4,000 with 2,000 shares. Offering 100 : 500 is short on
        // the first asset, so it sets the shares.
        let d = deposit(1_000, 4_000, 2_000, 100, 500).expect("deposit");
        assert_eq!(
            d,
            Deposit {
                shares: 200,
                used_a: 100,
                used_b: 400
            }
        );
        // Rounding favours the pool: shares down, amounts taken up.
        let d = deposit(1_000, 3_333, 777, 10, 10_000).expect("deposit");
        assert_eq!(d.shares, 7);
        assert_eq!((d.used_a, d.used_b), (10, 31));
        assert!(d.used_a * 777 >= d.shares * 1_000);
        assert!(d.used_b * 777 >= d.shares * 3_333);
        assert_eq!(deposit(0, 5, 10, 1, 1), None);
    }

    #[test]
    fn a_withdrawal_never_takes_more_than_its_share() {
        assert_eq!(withdrawal(1_000, 4_000, 2_000, 500), Some((250, 1_000)));
        assert_eq!(withdrawal(1_000, 3_333, 777, 7), Some((9, 30)));
        assert_eq!(withdrawal(1, 1, 0, 1), None);
    }

    #[test]
    fn outcomes_are_json_with_exact_amounts() {
        let json = serde_json::to_value(Outcome::Swapped {
            amount_out: u128::MAX,
        })
        .expect("json");
        assert_eq!(json["result"], "swapped");
        assert_eq!(
            json["amount_out"],
            "340282366920938463463374607431768211455"
        );
        let back: Outcome = serde_json::from_value(json).expect("back");
        assert_eq!(
            back,
            Outcome::Swapped {
                amount_out: u128::MAX
            }
        );
        let refunded = serde_json::to_value(Outcome::Refunded {
            reason: PoolError::BelowMinimum,
        })
        .expect("json");
        assert_eq!(refunded["reason"], "below_minimum");
    }
}
