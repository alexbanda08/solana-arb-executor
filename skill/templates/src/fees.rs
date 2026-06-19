//! Solana fee math for the arbitrage hot path.
//!
//! STD-ONLY and SELF-CONTAINED on purpose: no external crates, no cross-module
//! `use`. This lets the module compile and run its tests offline with:
//!
//!     rustc --edition 2021 --test src/fees.rs && ./fees
//!
//! Fee model (Solana, verified 2026-06):
//!   - Base fee: 5000 lamports per signature.
//!   - Priority fee: cu_price (micro-lamports per CU) * cu_limit (CUs),
//!     divided by 1_000_000, rounded UP (ceil) to whole lamports.
//!   - Compute budget: a single transaction may request at most
//!     1_400_000 compute units.
//!
//! ALWAYS simulate before submit to obtain a real CU estimate; the helpers here
//! turn that estimate into a safe limit and a total cost you can compare against
//! expected arbitrage revenue before risking a send.

/// Maximum compute units a single transaction may request on Solana.
pub const MAX_CU_PER_TX: u32 = 1_400_000;

/// Base fee in lamports charged per signature on a transaction.
pub const LAMPORTS_PER_SIGNATURE: u64 = 5000;

/// Micro-lamports per lamport (the unit the compute-unit price is quoted in).
pub const MICRO_LAMPORTS_PER_LAMPORT: u128 = 1_000_000;

/// Priority fee in lamports for a transaction, given the per-CU price in
/// micro-lamports and the requested compute-unit limit.
///
/// `priority_fee = ceil(cu_price_microlamports * cu_limit / 1_000_000)`
///
/// A `u128` intermediate is used so the multiplication cannot overflow: the
/// maximum product is `u64::MAX * MAX_CU_PER_TX`, which exceeds `u64` but fits
/// comfortably in `u128`. The result is rounded UP so the caller never
/// under-budgets the fee (an under-budgeted fee silently reorders or drops the
/// tx, which in arbitrage means a missed or reverted fill).
pub fn priority_fee_lamports(cu_price_microlamports: u64, cu_limit: u32) -> u64 {
    let product: u128 = (cu_price_microlamports as u128) * (cu_limit as u128);
    // Ceiling division: (a + b - 1) / b. Safe because product + (denom - 1)
    // still fits in u128 for any u64 * u32 product.
    let ceil = (product + (MICRO_LAMPORTS_PER_LAMPORT - 1)) / MICRO_LAMPORTS_PER_LAMPORT;
    // The ceiled lamport value fits in u64 for any realistic price/limit; clamp
    // defensively rather than panic in the hot path.
    if ceil > u64::MAX as u128 {
        u64::MAX
    } else {
        ceil as u64
    }
}

/// Total transaction fee in lamports: base fee for all signatures plus the
/// priority fee. Saturating add so a pathological priority value can never wrap.
pub fn total_fee_lamports(num_sigs: u64, priority_lamports: u64) -> u64 {
    LAMPORTS_PER_SIGNATURE
        .saturating_mul(num_sigs)
        .saturating_add(priority_lamports)
}

/// Apply a safety margin (in basis points) to a simulated CU estimate and cap
/// the result at the per-transaction maximum.
///
/// `with_margin = estimate * (10_000 + margin_bps) / 10_000`, capped at
/// `MAX_CU_PER_TX`. Simulation gives a lower bound; real execution can drift
/// slightly higher, so a margin (e.g. 1000 bps = 10%) avoids "exceeded CU
/// limit" reverts. A `u64` intermediate avoids overflow when scaling a large
/// estimate by a large margin.
pub fn cu_limit_with_margin(estimate: u32, margin_bps: u32) -> u32 {
    let scaled: u64 = (estimate as u64) * (10_000u64 + margin_bps as u64) / 10_000u64;
    if scaled > MAX_CU_PER_TX as u64 {
        MAX_CU_PER_TX
    } else {
        scaled as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn priority_fee_exact_division() {
        // 1000 microlamports/CU * 1000 CU = 1_000_000 microlamports = 1 lamport.
        assert_eq!(priority_fee_lamports(1000, 1000), 1);
        // 1_000_000 microlamports/CU * 1 CU = exactly 1 lamport.
        assert_eq!(priority_fee_lamports(1_000_000, 1), 1);
        // Exactly 2 lamports worth.
        assert_eq!(priority_fee_lamports(1_000_000, 2), 2);
    }

    #[test]
    fn priority_fee_rounds_up() {
        // 1 microlamport * 1 CU = 1 microlamport -> ceil to 1 lamport.
        assert_eq!(priority_fee_lamports(1, 1), 1);
        // 1500 microlamports/CU * 1000 CU = 1_500_000 microlamports = 1.5 lamports -> ceil to 2.
        assert_eq!(priority_fee_lamports(1500, 1000), 2);
        // 999_999 microlamports -> 0.999999 lamports -> ceil to 1.
        assert_eq!(priority_fee_lamports(999_999, 1), 1);
        // 1_000_001 microlamports -> 1.000001 -> ceil to 2.
        assert_eq!(priority_fee_lamports(1_000_001, 1), 2);
    }

    #[test]
    fn priority_fee_zero() {
        // No price or no compute -> no priority fee.
        assert_eq!(priority_fee_lamports(0, 1_400_000), 0);
        assert_eq!(priority_fee_lamports(50_000, 0), 0);
    }

    #[test]
    fn priority_fee_no_overflow_at_extremes() {
        // u64::MAX price * MAX_CU would overflow u64 in the multiply; u128
        // intermediate handles it and the result clamps to u64::MAX.
        let f = priority_fee_lamports(u64::MAX, MAX_CU_PER_TX);
        assert_eq!(f, u64::MAX);
    }

    #[test]
    fn total_fee_basic() {
        // 1 signature, no priority fee.
        assert_eq!(total_fee_lamports(1, 0), 5000);
        // 2 signatures + 10_000 lamport priority fee.
        assert_eq!(total_fee_lamports(2, 10_000), 20_000);
        // 0 signatures (degenerate) still adds priority.
        assert_eq!(total_fee_lamports(0, 777), 777);
    }

    #[test]
    fn total_fee_saturates() {
        // Pathological inputs saturate instead of wrapping.
        assert_eq!(total_fee_lamports(u64::MAX, u64::MAX), u64::MAX);
    }

    #[test]
    fn cu_margin_applies_and_caps() {
        // 10% margin on 200_000 -> 220_000.
        assert_eq!(cu_limit_with_margin(200_000, 1000), 220_000);
        // 0% margin is identity.
        assert_eq!(cu_limit_with_margin(200_000, 0), 200_000);
        // 50% margin on 1_000_000 -> 1_500_000, capped at MAX_CU_PER_TX.
        assert_eq!(cu_limit_with_margin(1_000_000, 5000), MAX_CU_PER_TX);
        // Estimate already at the cap stays at the cap.
        assert_eq!(cu_limit_with_margin(MAX_CU_PER_TX, 1000), MAX_CU_PER_TX);
    }

    #[test]
    fn cu_margin_rounds_down_fractional() {
        // 333 * 1.10 = 366.3 -> integer division floors to 366.
        assert_eq!(cu_limit_with_margin(333, 1000), 366);
    }
}
