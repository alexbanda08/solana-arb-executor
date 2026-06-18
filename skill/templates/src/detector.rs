// detector.rs -- Opportunity detection over streamed pool state.
//
// Receives AccountSnapshot updates from stream.rs, maintains a local view of
// two pool reserves, and emits an ArbCandidate when the gross spread (after
// subtracting estimated tx cost) exceeds the configured threshold.
//
// Design decisions:
//   - Pure-ish logic: this module does arithmetic and state bookkeeping; it does
//     not sign, simulate, or submit anything. Side-effect-free by construction
//     except for the mpsc send at the end.
//   - Uses fees::priority_fee_lamports and fees::total_fee_lamports to derive
//     net profit. The fee estimate is conservative (intentionally pessimistic).
//   - Constant-product AMM model (x * y = k) is used for illustration. Replace
//     `compute_output` with your target pool's invariant (Orca CLMM, Raydium
//     CPMM, etc.) and decode pool state in `decode_pool_reserves`.
//   - No allocation in the hot update path: we update a fixed array of PoolState
//     entries indexed by a u8 pool ID derived from the pubkey.
//
// Latency note: this runs synchronously in the tokio select! loop in main.rs.
// Keep the entire update-through-emit path under 10 us. Heavy parsing (Anchor
// discriminators, CLMM math) should be validated offline and the hot path
// should operate only on pre-decoded reserve fields.

use anyhow::Result;
use crate::fees;
use crate::stream::AccountSnapshot;
use tokio::sync::mpsc;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// A detected arbitrage candidate ready for risk-gate evaluation and execution.
#[derive(Debug, Clone)]
pub struct ArbCandidate {
    /// Slot in which the triggering account update arrived.
    pub slot: u64,
    /// Estimated gross profit before fees, in lamports.
    pub gross_profit_lamports: u64,
    /// Estimated net profit after tx fees, in lamports.
    pub net_profit_lamports: u64,
    /// The two pools involved (indices into the detector's pool table).
    pub pool_a_pubkey: [u8; 32],
    pub pool_b_pubkey: [u8; 32],
    /// Direction: true = buy on pool A, sell on pool B.
    pub buy_on_a: bool,
    /// Optimal trade size (input lamports) computed by the detector.
    pub optimal_input_lamports: u64,
}

// ---------------------------------------------------------------------------
// Pool state
// ---------------------------------------------------------------------------

/// Minimal constant-product pool state derived from raw account data.
#[derive(Debug, Clone, Default)]
struct PoolState {
    pubkey: [u8; 32],
    /// Reserve of token A, in the token's native smallest unit.
    reserve_a: u64,
    /// Reserve of token B, in the token's native smallest unit.
    reserve_b: u64,
    /// Last slot this state was updated from the stream.
    last_slot: u64,
    /// Whether this pool has been initialised from at least one account update.
    initialised: bool,
}

// ---------------------------------------------------------------------------
// Detector
// ---------------------------------------------------------------------------

/// Maintains state for exactly two pools forming a two-leg arb circuit.
/// Extend to N pools by replacing the pair with a Vec<PoolState>.
pub struct Detector {
    pool_a: PoolState,
    pool_b: PoolState,
    /// Minimum net profit threshold to emit a candidate, in lamports.
    spread_threshold_lamports: u64,
    /// CU price used for fee estimation, in microlamports.
    cu_price_microlamports: u64,
    /// CU limit (with margin already applied) per bundle.
    cu_limit_per_bundle: u32,
    /// Number of signatures per bundle (2 arb txs + 1 tip tx = 3 typical).
    sigs_per_bundle: u64,
    /// Downstream channel for emitting candidates.
    candidate_tx: mpsc::Sender<ArbCandidate>,
}

impl Detector {
    pub fn new(
        pool_a_pubkey: [u8; 32],
        pool_b_pubkey: [u8; 32],
        spread_threshold_lamports: u64,
        cu_price_microlamports: u64,
        cu_limit_per_bundle: u32,
        sigs_per_bundle: u64,
        candidate_tx: mpsc::Sender<ArbCandidate>,
    ) -> Self {
        let mut pool_a = PoolState::default();
        pool_a.pubkey = pool_a_pubkey;
        let mut pool_b = PoolState::default();
        pool_b.pubkey = pool_b_pubkey;

        Detector {
            pool_a,
            pool_b,
            spread_threshold_lamports,
            cu_price_microlamports,
            cu_limit_per_bundle,
            sigs_per_bundle,
            candidate_tx,
        }
    }

    /// Process an incoming account snapshot. Updates pool state if the pubkey
    /// matches one of our tracked pools, then evaluates for an opportunity.
    ///
    /// Returns Ok(true) if a candidate was emitted, Ok(false) otherwise.
    pub async fn process_account(&mut self, snap: AccountSnapshot) -> Result<bool> {
        let updated = if snap.pubkey == self.pool_a.pubkey {
            if let Some((reserve_a, reserve_b)) = decode_pool_reserves(&snap.data) {
                self.pool_a.reserve_a = reserve_a;
                self.pool_a.reserve_b = reserve_b;
                self.pool_a.last_slot = snap.slot;
                self.pool_a.initialised = true;
                true
            } else {
                false
            }
        } else if snap.pubkey == self.pool_b.pubkey {
            if let Some((reserve_a, reserve_b)) = decode_pool_reserves(&snap.data) {
                self.pool_b.reserve_a = reserve_a;
                self.pool_b.reserve_b = reserve_b;
                self.pool_b.last_slot = snap.slot;
                self.pool_b.initialised = true;
                true
            } else {
                false
            }
        } else {
            false
        };

        if !updated {
            return Ok(false);
        }

        // Both pools must be initialised before we can compare prices.
        if !self.pool_a.initialised || !self.pool_b.initialised {
            return Ok(false);
        }

        self.evaluate_and_emit(snap.slot).await
    }

    // -----------------------------------------------------------------------
    // Core spread evaluation
    // -----------------------------------------------------------------------

    /// Evaluate the current pool states for an arb opportunity and emit a
    /// candidate if the net profit clears the threshold.
    ///
    /// Strategy: single-hop constant-product. Buy token B cheaply in whichever
    /// pool has a lower A/B price, then sell B in the other pool for more A.
    ///
    /// Optimal input sizing: for constant-product pairs with equal fees the
    /// closed-form optimal input is sqrt(k_a * k_b) - reserve_a (where k = x*y).
    /// We use a discrete binary search here to stay fee-model agnostic and to
    /// correctly handle the fee tier in each pool.
    async fn evaluate_and_emit(&mut self, slot: u64) -> Result<bool> {
        let fee_cost = self.estimated_fee_lamports();

        // Price of A in terms of B: price_a = reserve_b / reserve_a (scaled by 1e9).
        // Avoid division by zero defensively.
        if self.pool_a.reserve_a == 0
            || self.pool_a.reserve_b == 0
            || self.pool_b.reserve_a == 0
            || self.pool_b.reserve_b == 0
        {
            return Ok(false);
        }

        // Direction A: buy B on pool_a (sell A, get B), sell B on pool_b (sell B, get A).
        // Direction B: buy B on pool_b (sell A, get B), sell B on pool_a (sell B, get A).
        //
        // For each direction compute the net profit at a candidate trade size
        // found via binary search on [1, min(reserve_a_buy, reserve_b_sell) / 10].

        let max_input_a = self.pool_a.reserve_a / 10; // cap at 10% of reserve (slippage guard)
        let max_input_b = self.pool_b.reserve_a / 10;

        let best_dir_a = optimal_net_profit(
            &self.pool_a,
            &self.pool_b,
            max_input_a,
            fee_cost,
        );
        let best_dir_b = optimal_net_profit(
            &self.pool_b,
            &self.pool_a,
            max_input_b,
            fee_cost,
        );

        let (buy_on_a, gross, net, optimal_input) = if best_dir_a.net > best_dir_b.net {
            (true, best_dir_a.gross, best_dir_a.net, best_dir_a.input)
        } else {
            (false, best_dir_b.gross, best_dir_b.net, best_dir_b.input)
        };

        // Threshold check: must beat minimum net profit.
        if net < self.spread_threshold_lamports as i64 || optimal_input == 0 {
            return Ok(false);
        }

        let candidate = ArbCandidate {
            slot,
            gross_profit_lamports: gross as u64,
            net_profit_lamports: net as u64,
            pool_a_pubkey: self.pool_a.pubkey,
            pool_b_pubkey: self.pool_b.pubkey,
            buy_on_a,
            optimal_input_lamports: optimal_input,
        };

        eprintln!(
            "[detector] slot={} opportunity: gross={} net={} input={} buy_on_a={}",
            slot,
            candidate.gross_profit_lamports,
            candidate.net_profit_lamports,
            candidate.optimal_input_lamports,
            candidate.buy_on_a
        );

        // Best-effort send; if the pipeline is saturated, drop rather than block the stream.
        let _ = self.candidate_tx.try_send(candidate);
        Ok(true)
    }

    fn estimated_fee_lamports(&self) -> u64 {
        let priority = fees::priority_fee_lamports(self.cu_price_microlamports, self.cu_limit_per_bundle);
        fees::total_fee_lamports(self.sigs_per_bundle, priority)
    }
}

// ---------------------------------------------------------------------------
// Arithmetic helpers
// ---------------------------------------------------------------------------

struct ProfitResult {
    input: u64,
    gross: i64,
    net: i64,
}

/// Binary-search for the input size that maximises net profit when buying on
/// `pool_buy` and selling on `pool_sell` using constant-product invariant.
/// Fee rate is hardcoded to 30 bps (0.3%); adjust or parameterise per pool.
fn optimal_net_profit(
    pool_buy: &PoolState,
    pool_sell: &PoolState,
    max_input: u64,
    fee_cost: u64,
) -> ProfitResult {
    const FEE_BPS: u64 = 30;
    const FEE_DENOM: u64 = 10_000;

    if max_input == 0 {
        return ProfitResult { input: 0, gross: 0, net: -(fee_cost as i64) };
    }

    let mut lo: u64 = 1;
    let mut hi: u64 = max_input;
    let mut best = ProfitResult { input: 0, gross: 0, net: i64::MIN };

    // 32 iterations for u64 precision.
    for _ in 0..32 {
        if lo > hi { break; }
        let mid = lo + (hi - lo) / 2;

        let out_b = compute_output(pool_buy.reserve_a, pool_buy.reserve_b, mid, FEE_BPS, FEE_DENOM);
        if out_b == 0 {
            hi = mid.saturating_sub(1);
            continue;
        }

        let out_a = compute_output(pool_sell.reserve_b, pool_sell.reserve_a, out_b, FEE_BPS, FEE_DENOM);
        if out_a == 0 {
            hi = mid.saturating_sub(1);
            continue;
        }

        let gross = out_a as i64 - mid as i64;
        let net = gross - fee_cost as i64;

        if net > best.net {
            best = ProfitResult { input: mid, gross, net };
            lo = mid + 1;
        } else {
            hi = mid.saturating_sub(1);
        }
    }

    best
}

/// Constant-product AMM output: given input amount, compute output amount.
///   out = (reserve_out * amt_in * (FEE_DENOM - fee_bps)) / (reserve_in * FEE_DENOM + amt_in * (FEE_DENOM - fee_bps))
///
/// Uses u128 to avoid overflow.
fn compute_output(reserve_in: u64, reserve_out: u64, amt_in: u64, fee_bps: u64, fee_denom: u64) -> u64 {
    let fee_mult = fee_denom.saturating_sub(fee_bps) as u128;
    let numerator = (reserve_out as u128)
        .saturating_mul(amt_in as u128)
        .saturating_mul(fee_mult);
    let denominator = (reserve_in as u128)
        .saturating_mul(fee_denom as u128)
        .saturating_add((amt_in as u128).saturating_mul(fee_mult));
    if denominator == 0 {
        return 0;
    }
    (numerator / denominator) as u64
}

// ---------------------------------------------------------------------------
// Pool account decoder
// ---------------------------------------------------------------------------

/// Decode (reserve_a, reserve_b) from raw account bytes.
///
/// THIS IS A PLACEHOLDER LAYOUT. Replace with the actual pool struct layout
/// for your target DEX (Orca Whirlpool, Raydium CPMM, etc.). The Anchor
/// discriminator (first 8 bytes) is skipped; reserves are read as little-endian
/// u64 at fixed offsets matching the target layout.
///
/// Returns None if the data slice is too short or fails a basic sanity check.
fn decode_pool_reserves(data: &[u8]) -> Option<(u64, u64)> {
    // Anchor discriminator: 8 bytes. After that, adjust offsets for your pool.
    // Example: Orca Whirlpool token_vault_a at byte 101, token_vault_b at 133.
    // Raydium CPMM pool: different offsets -- check the IDL.
    //
    // Here we use a generic example layout (discriminator=8, reserve_a@8, reserve_b@16).
    const DISCRIMINATOR_LEN: usize = 8;
    const RESERVE_A_OFFSET: usize = DISCRIMINATOR_LEN;
    const RESERVE_B_OFFSET: usize = DISCRIMINATOR_LEN + 8;
    const MIN_LEN: usize = RESERVE_B_OFFSET + 8;

    if data.len() < MIN_LEN {
        return None;
    }

    let reserve_a = u64::from_le_bytes(
        data[RESERVE_A_OFFSET..RESERVE_A_OFFSET + 8].try_into().ok()?
    );
    let reserve_b = u64::from_le_bytes(
        data[RESERVE_B_OFFSET..RESERVE_B_OFFSET + 8].try_into().ok()?
    );

    // Basic sanity: non-zero reserves required.
    if reserve_a == 0 || reserve_b == 0 {
        return None;
    }

    Some((reserve_a, reserve_b))
}
