//! Exact integer arithmetic for pool pricing: products of two 128-bit amounts
//! do not fit in 128 bits, so they are carried in 256 and divided back down.
//! No floating point, no rounding that is not stated, and nothing that wraps
//! unseen: every wrapping operation below is one whose carry is handled on the
//! next line.

/// `a × b` as `(high, low)` 128-bit halves.
pub fn mul_wide(a: u128, b: u128) -> (u128, u128) {
    const HALF: u32 = 64;
    const MASK: u128 = (1 << 64) - 1;
    let (a1, a0) = (a.wrapping_shr(HALF), a & MASK);
    let (b1, b0) = (b.wrapping_shr(HALF), b & MASK);
    // Each factor is below 2^64, so no product overflows.
    let p00 = a0.wrapping_mul(b0);
    let p01 = a0.wrapping_mul(b1);
    let p10 = a1.wrapping_mul(b0);
    let p11 = a1.wrapping_mul(b1);
    let (mid, mid_carry) = p01.overflowing_add(p10);
    let (low, low_carry) = p00.overflowing_add(mid.wrapping_shl(HALF));
    // The true product is below 2^256, so the high half cannot overflow.
    let high = p11
        .wrapping_add(mid.wrapping_shr(HALF))
        .wrapping_add(u128::from(mid_carry).wrapping_shl(HALF))
        .wrapping_add(u128::from(low_carry));
    (high, low)
}

/// `(high, low) ÷ d` as `(quotient, remainder)`. `None` if `d` is zero or the
/// quotient does not fit in 128 bits.
fn div_wide(high: u128, low: u128, d: u128) -> Option<(u128, u128)> {
    // The quotient fits exactly when high < d (which also excludes d = 0).
    if high >= d {
        return None;
    }
    // Schoolbook division in base 2. Before every step rem < d.
    let mut rem = high;
    let mut quotient = 0u128;
    for bit in (0..u128::BITS).rev() {
        let carried = rem.wrapping_shr(127) == 1;
        rem = rem.wrapping_shl(1) | (low.wrapping_shr(bit) & 1);
        // 2·rem + bit is below 2d. If it carried out of 128 bits it is above
        // d, and the wrapping subtraction gives the true difference.
        if carried || rem >= d {
            rem = rem.wrapping_sub(d);
            quotient |= 1u128.wrapping_shl(bit);
        }
    }
    Some((quotient, rem))
}

/// `⌊a × b ÷ d⌋`. `None` if `d` is zero or the result does not fit.
pub fn mul_div(a: u128, b: u128, d: u128) -> Option<u128> {
    let (high, low) = mul_wide(a, b);
    div_wide(high, low, d).map(|(q, _)| q)
}

/// `⌈a × b ÷ d⌉`. `None` if `d` is zero or the result does not fit.
pub fn mul_div_up(a: u128, b: u128, d: u128) -> Option<u128> {
    let (high, low) = mul_wide(a, b);
    let (q, rem) = div_wide(high, low, d)?;
    if rem == 0 { Some(q) } else { q.checked_add(1) }
}

/// `⌊√(a × b)⌋`: the geometric mean, which always fits in 128 bits.
pub fn sqrt_mul(a: u128, b: u128) -> u128 {
    let target = mul_wide(a, b);
    // Decide the root's bits from the top: keep a bit if the square still fits.
    let mut root = 0u128;
    for bit in (0..u128::BITS).rev() {
        let candidate = root | 1u128.wrapping_shl(bit);
        if mul_wide(candidate, candidate) <= target {
            root = candidate;
        }
    }
    root
}

#[cfg(test)]
mod tests {
    #![allow(clippy::arithmetic_side_effects)]
    use super::*;

    const MAX: u128 = u128::MAX;

    #[test]
    fn products_match_128_bit_arithmetic_where_that_is_exact() {
        for (a, b) in [
            (0u128, 0u128),
            (1, 1),
            (u128::from(u64::MAX), u128::from(u64::MAX)),
            (123_456_789_012_345_678, 987_654_321_098_765_432),
            (1 << 64, (1 << 63) + 5),
        ] {
            assert_eq!(mul_wide(a, b), (0, a * b), "{a} × {b}");
        }
    }

    #[test]
    fn products_carry_into_the_high_half() {
        // (2^128 − 1)² = 2^256 − 2^129 + 1
        assert_eq!(mul_wide(MAX, MAX), (MAX - 1, 1));
        assert_eq!(mul_wide(1 << 127, 2), (1, 0));
        assert_eq!(mul_wide(1 << 100, 1 << 100), (1 << 72, 0));
        // Both cross terms large: the middle sum itself carries.
        let a = (u128::from(u64::MAX) << 64) | 1;
        assert_eq!(
            mul_wide(a, a),
            (
                0xffff_ffff_ffff_fffe_0000_0000_0000_0002,
                0xffff_ffff_ffff_fffe_0000_0000_0000_0001
            )
        );
    }

    // Computed with arbitrary-precision integers, independently of this code.
    const VECTORS: &[(u128, u128, u128, u128, u128)] = &[
        // a, b, d, ⌊ab/d⌋, ab mod d
        (
            170141183460469231731687303715884105727,
            170141183460469231731687303715884105727,
            170141183460469231731687303715884105727,
            170141183460469231731687303715884105727,
            0,
        ),
        (
            170141183460469231731687303715884105727,
            997,
            1000,
            169630759910087824036492241804736453409,
            819,
        ),
        (
            123456789012345678901234567890123456789,
            98765432109876543210987654321098765432,
            340282366920938463463374607431768211455,
            35832779770616718878242575074126380497,
            117670634951394259923162974189614924713,
        ),
        (
            340282366920938463463374607431768211455,
            340282366920938463463374607431768211454,
            340282366920938463463374607431768211455,
            340282366920938463463374607431768211454,
            0,
        ),
        (
            1000000000000000000000000000,
            3141592653589793238462643383,
            2718281828459045235360287471,
            1155727349790921717910093183,
            978075477433064116722589807,
        ),
        (
            18446744073709551617,
            18446744073709551629,
            7,
            48611766702991209103089860637671704870,
            3,
        ),
    ];

    #[test]
    fn division_matches_independent_vectors() {
        for &(a, b, d, q, r) in VECTORS {
            assert_eq!(mul_div(a, b, d), Some(q), "⌊{a}·{b}/{d}⌋");
            let up = if r == 0 { q } else { q + 1 };
            assert_eq!(mul_div_up(a, b, d), Some(up), "⌈{a}·{b}/{d}⌉");
            // And the identity q·d + r = a·b, in 256 bits.
            let (high, low) = mul_wide(q, d);
            let (low, carry) = low.overflowing_add(r);
            assert_eq!((high + u128::from(carry), low), mul_wide(a, b));
        }
    }

    #[test]
    fn division_agrees_with_128_bit_arithmetic_on_many_small_inputs() {
        // A fixed linear congruential sequence: the same inputs on every run.
        let mut x = 0x2545_f491_4f6c_dd1du64;
        let mut next = || {
            x = x
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            u128::from(x >> 20)
        };
        for _ in 0..20_000 {
            let (a, b, d) = (next(), next(), next() + 1);
            assert_eq!(mul_div(a, b, d), Some(a * b / d));
            assert_eq!(mul_div_up(a, b, d), Some((a * b).div_ceil(d)));
        }
    }

    #[test]
    fn division_refuses_zero_and_results_that_do_not_fit() {
        assert_eq!(mul_div(5, 5, 0), None);
        assert_eq!(mul_div(0, 0, 0), None);
        assert_eq!(mul_div(MAX, 2, 1), None);
        assert_eq!(mul_div(MAX, MAX, MAX - 1), None);
        assert_eq!(mul_div(MAX, 2, 2), Some(MAX));
        // Rounding up may be what no longer fits.
        assert_eq!(mul_div(MAX, 3, 3), Some(MAX));
        assert_eq!(
            mul_div_up(MAX, 2, 3),
            Some(226854911280625642308916404954512140970)
        );
        assert_eq!(mul_div_up(0, 9, 4), Some(0));
        assert_eq!(mul_div_up(1, 1, 4), Some(1));
    }

    #[test]
    fn square_roots_are_exact_floors() {
        assert_eq!(sqrt_mul(0, 99), 0);
        assert_eq!(sqrt_mul(1, 1), 1);
        assert_eq!(sqrt_mul(2, 2), 2);
        assert_eq!(sqrt_mul(2, 1), 1);
        assert_eq!(sqrt_mul(1 << 100, 1 << 100), 1 << 100);
        assert_eq!(sqrt_mul(MAX, MAX), MAX);
        assert_eq!(sqrt_mul(MAX, 1), (1 << 64) - 1);
        // ⌊√(10^27 · 4·10^30)⌋, from arbitrary-precision arithmetic.
        assert_eq!(
            sqrt_mul(
                1_000_000_000_000_000_000_000_000_000,
                4_000_000_000_000_000_000_000_000_000_000
            ),
            63_245_553_203_367_586_639_977_870_888
        );
        // r is the floor: r² ≤ ab < (r+1)².
        for (a, b) in [
            (3u128, 5u128),
            (MAX - 7, 12_345),
            (1 << 127, (1 << 126) + 3),
        ] {
            let r = sqrt_mul(a, b);
            assert!(mul_wide(r, r) <= mul_wide(a, b));
            assert!(mul_wide(r + 1, r + 1) > mul_wide(a, b));
        }
    }
}
