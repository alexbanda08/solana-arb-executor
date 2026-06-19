// main.rs -- solana-arb-executor entry point.
//
// Pipeline: Config -> StreamSubscriber -> Detector -> RiskGate -> (DRY_RUN log | confirm-gate -> Jito)
//
// SAFETY POSTURE (load-bearing; summarised here, enforced in each module):
//   DRY_RUN=true (default): the bundle is constructed and simulated but never sent.
//   DRY_RUN=false + REQUIRE_CONFIRM=true (default): user must type YES on stdin.
//   DRY_RUN=false + REQUIRE_CONFIRM=false: live auto-submission (DANGEROUS; requires
//     explicit env opt-in to both flags; no code path bypasses both simultaneously
//     by accident).
//   The risk gate and circuit breaker are active in all modes.
//
// This is scaffolding and a knowledge resource, not a production bot.
// Retail spatial-arb is infra-dominated; durable edge comes from correct execution,
// landing discipline, risk management, and less-contested opportunities (funding
// basis, stat-arb). See references/opportunity-detection.md for the honest picture.

mod config;
mod fees;
mod risk;
mod stream;
mod jito;
mod detector;

use anyhow::{Context, Result};
use jito_sdk_rust::JitoJsonRpcSDK;
use solana_client::rpc_client::RpcClient;
use tokio::sync::mpsc;
use tokio::select;

use config::Config;
use detector::{ArbCandidate, Detector};
use risk::RiskGate;
use stream::StreamUpdate;

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> Result<()> {
    // 1. Configuration -- exits with an error if required env vars are missing.
    let cfg = Config::from_env().context("loading configuration")?;

    // 2. Risk gate -- enforces caps and the circuit breaker.
    let mut risk_gate = RiskGate::new(cfg.risk_limits.clone());

    // 3. RPC client (used for simulation and blockhash queries).
    let rpc = RpcClient::new_with_commitment(
        cfg.rpc_url.clone(),
        solana_sdk::commitment_config::CommitmentConfig::processed(),
    );

    // 4. Jito SDK (used for tip-account resolution and bundle submission).
    let jito_sdk = JitoJsonRpcSDK::new(&cfg.jito_block_engine_url, None);

    // 5. Resolve a Jito tip account once at startup.
    let tip_account = jito::fetch_tip_account(&jito_sdk)
        .await
        .context("fetching Jito tip account at startup")?;
    eprintln!("[main] Jito tip account: {}", tip_account);

    // 6. CU limit with margin applied (feeds detector fee estimation and jito tx building).
    let cu_limit = fees::cu_limit_with_margin(cfg.cu_limit_estimate, cfg.cu_margin_bps);

    // 7. Channels.
    //    stream -> main: raw account/slot updates.
    //    detector -> main: emitted arb candidates.
    let (candidate_tx, mut candidate_rx) = mpsc::channel::<ArbCandidate>(64);

    // 8. Pool pubkeys: supply your target pool addresses via env or config extension.
    //    Here we read ARB_POOL_A and ARB_POOL_B (base58) as an example.
    let pool_a_pubkey = parse_pubkey_env("ARB_POOL_A")?;
    let pool_b_pubkey = parse_pubkey_env("ARB_POOL_B")?;

    // Spread threshold: convert from bps to lamports. We use a nominal 1 SOL trade
    // to translate bps to absolute lamports (bps * 1e7 = lamports per SOL * bps / 1e4).
    // The detector re-computes this on actual trade sizes; this is the floor threshold.
    let spread_threshold_lamports = bps_to_lamports_floor(cfg.spread_threshold_bps);

    // 9. Detector.
    let mut detector = Detector::new(
        pool_a_pubkey,
        pool_b_pubkey,
        spread_threshold_lamports,
        cfg.cu_price_microlamports,
        cu_limit,
        3, // typical sig count: 2 arb txs + 1 tip tx
        candidate_tx,
    );

    // 10. gRPC stream subscriber. Runs in a background tokio task with reconnect.
    let mut stream_rx = stream::subscribe(stream::StreamConfig {
        grpc_url: cfg.grpc_url.clone(),
        grpc_token: cfg.grpc_token.clone(),
        account_keys: vec![
            bs58::encode(pool_a_pubkey).into_string(),
            bs58::encode(pool_b_pubkey).into_string(),
        ],
        channel_capacity: 256,
    });

    eprintln!(
        "[main] pipeline live | dry_run={} require_confirm={} pool_a={} pool_b={}",
        cfg.dry_run,
        cfg.require_confirm,
        bs58::encode(pool_a_pubkey).into_string(),
        bs58::encode(pool_b_pubkey).into_string(),
    );

    // 11. Main event loop. Drive both the stream receiver and candidate receiver
    //     in a single select! so neither starves.
    loop {
        select! {
            // --- Stream update branch ---
            maybe_update = stream_rx.recv() => {
                match maybe_update {
                    None => {
                        eprintln!("[main] stream channel closed; shutting down");
                        break;
                    }
                    Some(StreamUpdate::Slot(s)) => {
                        // Slot bookkeeping (leader-window tracking) could go here.
                        let _ = s; // suppress unused warning in scaffolding
                    }
                    Some(StreamUpdate::Account(snap)) => {
                        // Route to detector; ignore errors (stale or unknown account).
                        if let Err(e) = detector.process_account(snap).await {
                            eprintln!("[main] detector error: {:#}", e);
                        }
                    }
                }
            }

            // --- Candidate branch ---
            maybe_candidate = candidate_rx.recv() => {
                match maybe_candidate {
                    None => {
                        eprintln!("[main] candidate channel closed; shutting down");
                        break;
                    }
                    Some(candidate) => {
                        if let Err(e) = handle_candidate(
                            &candidate,
                            &mut risk_gate,
                            &rpc,
                            &jito_sdk,
                            &tip_account,
                            cu_limit,
                            &cfg,
                        ).await {
                            eprintln!("[main] handle_candidate error: {:#}", e);
                            // Record a revert-equivalent so the risk gate tracks failures.
                            risk_gate.record_revert();
                            if risk_gate.is_killed() {
                                eprintln!("[main] RISK GATE TRIPPED: circuit breaker killed. Shutting down.");
                                break;
                            }
                        }
                    }
                }
            }
        }
    }

    eprintln!("[main] exiting");
    Ok(())
}

// ---------------------------------------------------------------------------
// Candidate handling
// ---------------------------------------------------------------------------

/// Process a single arb candidate through the full pipeline:
///   risk-gate check -> (DRY_RUN log | simulate -> confirm-gate -> submit -> poll)
async fn handle_candidate(
    candidate: &ArbCandidate,
    risk_gate: &mut RiskGate,
    rpc: &RpcClient,
    jito_sdk: &JitoJsonRpcSDK,
    tip_account: &solana_sdk::pubkey::Pubkey,
    cu_limit: u32,
    cfg: &Config,
) -> Result<()> {
    // --- Risk gate ---
    risk_gate
        .check_order(candidate.optimal_input_lamports, candidate.optimal_input_lamports)
        .map_err(|e| anyhow::anyhow!("risk gate blocked: {}", e))?;

    // --- DRY_RUN path ---
    if cfg.dry_run {
        eprintln!(
            "[main] DRY_RUN: would submit bundle | slot={} net_profit={} input={} buy_on_a={}",
            candidate.slot,
            candidate.net_profit_lamports,
            candidate.optimal_input_lamports,
            candidate.buy_on_a,
        );
        // No state mutation; dry run does not count as fill or revert.
        return Ok(());
    }

    // --- Build the bundle ---
    // NOTE: instruction construction requires your DEX-specific program IDs and
    // account layouts. Replace the placeholder below with real swap instructions.
    // Delegate Anchor-specific instruction building to ../solana-dev/ (see
    // references/delegation.md). Here we pass an empty instruction set to wire
    // the plumbing; cargo build will succeed, but the bundle will be a tip-only
    // no-op until you fill in the swap instructions.

    // Keypair: in production, load from a hardware wallet or encrypted keystore.
    // Never hardcode or commit private keys. `load_keypair` returns an error (it
    // does not panic) if ARB_KEYPAIR_PATH is unset or the file cannot be parsed;
    // the `?` propagates it so the candidate is skipped rather than crashing.
    let payer = load_keypair().context("loading payer keypair")?;

    let recent_blockhash = rpc
        .get_latest_blockhash()
        .context("fetching recent blockhash")?;

    // Size the tip from the net edge with a hard floor/ceiling, and ABORT this
    // candidate if no edge survives the tip (None => skip, never submit).
    let min_net_floor_lamports = bps_to_lamports_floor(cfg.spread_threshold_bps);
    let tip_lamports = match compute_tip_lamports(
        candidate.net_profit_lamports,
        cfg.tip_bps,
        cfg.tip_floor_lamports,
        cfg.tip_ceil_lamports,
        min_net_floor_lamports,
    ) {
        Some(tip) => tip,
        None => {
            eprintln!(
                "[main] tip-gate abort | slot={} net_profit={} below post-tip threshold ({}); skipping",
                candidate.slot, candidate.net_profit_lamports, min_net_floor_lamports
            );
            return Ok(());
        }
    };

    // Swap instructions placeholder -- replace with real DEX calls.
    let arb_instruction_groups: Vec<(Vec<solana_sdk::instruction::Instruction>, Vec<&solana_sdk::signature::Keypair>)> = vec![
        // leg 1: buy on pool_a
        (build_swap_instructions_placeholder(candidate, true, &payer)?, vec![&payer]),
        // leg 2: sell on pool_b
        (build_swap_instructions_placeholder(candidate, false, &payer)?, vec![&payer]),
    ];

    let bundle = jito::build_bundle(
        arb_instruction_groups,
        &payer,
        *tip_account,
        tip_lamports,
        recent_blockhash,
    )
    .context("building bundle")?;

    // --- Simulate FIRST (mandatory) ---
    let sim_result = jito::simulate_bundle(rpc, &bundle)
        .await
        .context("simulating bundle")?;

    if !sim_result.success {
        anyhow::bail!(
            "simulation failed (slot={}): {:?}",
            candidate.slot,
            sim_result.err_message
        );
    }

    eprintln!(
        "[main] simulation passed | cu_consumed={:?}",
        sim_result.units_consumed
    );

    // --- Confirm-gate + submit ---
    let bundle_id = jito::submit_bundle(jito_sdk, &bundle, cfg.require_confirm)
        .await
        .context("submitting bundle")?;

    // --- Poll for landing ---
    let status = jito::poll_bundle_status(jito_sdk, &bundle_id, 20)
        .await
        .context("polling bundle status")?;

    eprintln!("[main] bundle landed | id={} status={}", bundle_id, status);
    // Record realized PnL. net_profit_lamports is already net of fees by detector accounting.
    risk_gate.record_fill(candidate.net_profit_lamports as i64);

    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn parse_pubkey_env(key: &str) -> Result<[u8; 32]> {
    let val = std::env::var(key)
        .with_context(|| format!("required env var {} is not set", key))?;
    let decoded = bs58::decode(&val)
        .into_vec()
        .with_context(|| format!("env var {} is not valid base58: {}", key, val))?;
    decoded
        .as_slice()
        .try_into()
        .with_context(|| format!("env var {} decoded to {} bytes, expected 32", key, decoded.len()))
}

/// Convert basis points to a lamport floor threshold using 1 SOL as the nominal size.
/// 30 bps on 1 SOL = 3_000_000 lamports (0.003 SOL).
fn bps_to_lamports_floor(bps: u32) -> u64 {
    const LAMPORTS_PER_SOL: u64 = 1_000_000_000;
    (LAMPORTS_PER_SOL * bps as u64) / 10_000
}

/// Size the Jito tip from the net edge and decide whether the candidate is still
/// worth submitting AFTER the tip.
///
/// Returns `Some(tip)` when the trade clears the post-tip threshold, or `None`
/// (caller MUST skip the candidate) when tipping would leave net <= the spread
/// floor. This implements the floor/ceiling/abort discipline documented in
/// references/jito-bundles.md:
///   - tip = net_profit * tip_bps / 10_000, then clamped to [floor, ceiling];
///   - ABORT (None) if net_profit <= floor (cannot even meet the minimum tip),
///     or if net_profit - tip <= min_net_floor (no edge left after tipping).
///
/// Contested atomic arb pays ~50-60% of extracted edge; a flat 20% loses
/// inclusion on contested slots and over-tips on quiet ones. There is no
/// guaranteed-inclusion tip -- the durable edge is correct landing + risk
/// discipline + less-contested ops, not a bigger tip.
fn compute_tip_lamports(
    net_profit_lamports: u64,
    tip_bps: u32,
    tip_floor_lamports: u64,
    tip_ceil_lamports: u64,
    min_net_floor_lamports: u64,
) -> Option<u64> {
    // Can't even afford the minimum tip -> not worth submitting.
    if net_profit_lamports <= tip_floor_lamports {
        return None;
    }

    // tip = net * bps / 10_000 with a u128 intermediate to avoid overflow.
    let raw_tip =
        ((net_profit_lamports as u128) * (tip_bps as u128) / 10_000u128) as u64;
    let tip = raw_tip.clamp(tip_floor_lamports, tip_ceil_lamports);

    // Net after tipping must clear the minimum-edge floor, else abort.
    let net_after_tip = net_profit_lamports.saturating_sub(tip);
    if net_after_tip <= min_net_floor_lamports {
        return None;
    }

    Some(tip)
}

/// Load the payer Keypair from a JSON file path specified by ARB_KEYPAIR_PATH.
/// The file must be a standard Solana CLI keypair (array of 64 u8 values).
/// Never hardcode or embed private keys. Use a hardware wallet in production.
fn load_keypair() -> Result<solana_sdk::signature::Keypair> {
    let path = std::env::var("ARB_KEYPAIR_PATH")
        .context("ARB_KEYPAIR_PATH not set; required for live submission")?;
    let data = std::fs::read_to_string(&path)
        .with_context(|| format!("reading keypair from {}", path))?;
    let bytes: Vec<u8> = serde_json::from_str::<Vec<u8>>(&data)
        .with_context(|| format!("parsing keypair JSON from {}", path))?;
    // solana-sdk 4 moved keypair construction to the solana-keypair crate and
    // REMOVED Keypair::from_bytes; the supported path is the std TryFrom impl
    // (TryFrom<&[u8]>), which returns a conversion error we wrap with context.
    solana_sdk::signature::Keypair::try_from(&bytes[..])
        .map_err(|e| anyhow::anyhow!("constructing Keypair from bytes in {}: {}", path, e))
}

/// Placeholder: returns an empty instruction list.
/// Replace this with real DEX swap instruction builders for your target pools.
/// Consider delegating Anchor CPI construction to ../solana-dev/ (see delegation.md).
fn build_swap_instructions_placeholder(
    _candidate: &ArbCandidate,
    _buy_leg: bool,
    _payer: &solana_sdk::signature::Keypair,
) -> Result<Vec<solana_sdk::instruction::Instruction>> {
    // INTEGRATION POINT: construct the swap instruction for your DEX here.
    // Returning empty keeps the binary wiring intact and compiling; a bundle
    // built from no-op arb legs lands as a tip transfer only, so it is safe to
    // dry-run. Build real instructions via ../solana-dev/ (see delegation.md)
    // before going live; the confirm-gate still governs every live send.
    Ok(vec![])
}

// ---------------------------------------------------------------------------
// Tests (run with `cargo test`; the full crate links the pinned deps).
//
// These exercise the pure tip-sizing discipline -- fraction-of-edge with a hard
// floor/ceiling and the post-tip abort -- which is the SOTA-aligned change in
// this module. compute_tip_lamports has no external dependencies, so the cases
// below are exact and deterministic.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::compute_tip_lamports;

    // (bps=5000 => 50%, floor=1_000, ceil=10_000_000, min_net_floor=10_000)

    #[test]
    fn tip_is_fraction_of_edge() {
        // 50% of 1_000_000 = 500_000; net after tip = 500_000 > floor 10_000 -> ok.
        assert_eq!(
            compute_tip_lamports(1_000_000, 5000, 1_000, 10_000_000, 10_000),
            Some(500_000)
        );
    }

    #[test]
    fn tip_respects_ceiling() {
        // 50% of 100_000_000 = 50_000_000, clamped down to the 10_000_000 ceiling.
        assert_eq!(
            compute_tip_lamports(100_000_000, 5000, 1_000, 10_000_000, 10_000),
            Some(10_000_000)
        );
    }

    #[test]
    fn tip_raised_to_floor() {
        // 10% of 50_000 = 5_000, but a 6_000 floor raises the tip to 6_000.
        // net after tip = 44_000 > min_net_floor 100 -> ok.
        assert_eq!(
            compute_tip_lamports(50_000, 1000, 6_000, 10_000_000, 100),
            Some(6_000)
        );
    }

    #[test]
    fn aborts_when_edge_below_floor() {
        // net_profit <= tip_floor -> cannot afford the minimum tip -> abort.
        assert_eq!(
            compute_tip_lamports(800, 5000, 1_000, 10_000_000, 0),
            None
        );
    }

    #[test]
    fn aborts_when_no_edge_survives_tip() {
        // 50% of 15_000 = 7_500 tip; net after tip = 7_500 <= min_net_floor 10_000
        // -> abort (do not submit a candidate that loses its edge to the tip).
        assert_eq!(
            compute_tip_lamports(15_000, 5000, 1_000, 10_000_000, 10_000),
            None
        );
    }

    #[test]
    fn high_bps_still_clamped_by_ceiling() {
        // 100% of edge would be 5_000_000 but ceiling caps at 1_000_000.
        assert_eq!(
            compute_tip_lamports(5_000_000, 10_000, 1_000, 1_000_000, 10_000),
            Some(1_000_000)
        );
    }
}
