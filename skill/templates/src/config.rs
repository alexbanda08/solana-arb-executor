// config.rs -- environment-driven configuration for solana-arb-executor.
//
// All secrets (private keys, auth tokens) are injected via environment variables
// at runtime. No defaults contain real endpoints or credentials. DRY_RUN and
// REQUIRE_CONFIRM both default to `true` so the binary is safe-by-default.
//
// Required env vars (no built-in fallback -- binary exits if missing):
//   ARB_RPC_URL         -- Solana RPC HTTP endpoint
//   ARB_GRPC_URL        -- Yellowstone gRPC endpoint
//   ARB_GRPC_TOKEN      -- Bearer token for gRPC auth
//   ARB_JITO_URL        -- Jito block-engine endpoint
//
// Optional env vars (all have safe defaults):
//   ARB_DRY_RUN                    -- "true"|"false"  (default: true)
//   ARB_REQUIRE_CONFIRM            -- "true"|"false"  (default: true)
//   ARB_MAX_NOTIONAL_SOL           -- max per-trade notional, SOL (default: 1.0)
//   ARB_MAX_POSITION_SOL           -- max open position, SOL (default: 2.0)
//   ARB_MAX_DAILY_LOSS_SOL         -- daily loss circuit-breaker, SOL (default: 0.5)
//   ARB_MAX_CONSECUTIVE_REVERTS    -- revert streak before kill (default: 5)
//   ARB_SPREAD_THRESHOLD_BPS       -- minimum spread to flag as opportunity (default: 30)
//   ARB_CU_LIMIT_ESTIMATE          -- CU limit estimate per tx (default: 200_000)
//   ARB_CU_MARGIN_BPS              -- CU estimate margin in bps (default: 1500 = 15%)
//   ARB_CU_PRICE_MICROLAMPORTS     -- priority fee price (default: 10_000)
//   ARB_TIP_BPS                    -- tip as bps of net edge (default: 5000 = 50%)
//   ARB_TIP_FLOOR_LAMPORTS         -- minimum tip if we tip at all (default: 1_000)
//   ARB_TIP_CEIL_LAMPORTS          -- maximum tip, never donate more (default: 10_000_000)

use anyhow::{bail, Context, Result};
use crate::risk::RiskLimits;

const LAMPORTS_PER_SOL: u64 = 1_000_000_000;

/// Top-level runtime configuration. Constructed once at startup and passed by
/// shared reference through the pipeline.
#[derive(Debug, Clone)]
pub struct Config {
    // --- safety defaults (must both be true in production until explicitly overridden) ---
    /// When true, log the intended bundle but never send it to the block engine.
    pub dry_run: bool,
    /// When true, block automatic submission; require explicit stdin "YES" confirmation.
    pub require_confirm: bool,

    // --- network endpoints ---
    pub rpc_url: String,
    pub grpc_url: String,
    pub grpc_token: String,
    pub jito_block_engine_url: String,

    // --- risk limits ---
    pub risk_limits: RiskLimits,

    // --- opportunity detection ---
    /// Minimum gross spread in basis points to flag a candidate.
    pub spread_threshold_bps: u32,

    // --- transaction sizing ---
    /// CU estimate passed to fees::cu_limit_with_margin.
    pub cu_limit_estimate: u32,
    /// Margin applied on top of cu_limit_estimate, in basis points.
    pub cu_margin_bps: u32,
    /// Priority fee price in microlamports per CU.
    pub cu_price_microlamports: u64,

    // --- tip sizing (Jito) ---
    /// Tip expressed as basis points of the net edge. Contested atomic arb pays
    /// ~50-60% of extracted edge; 5000 bps (50%) is a competitive default.
    pub tip_bps: u32,
    /// Hard floor: if we tip at all, never tip below this (a dust tip never lands).
    pub tip_floor_lamports: u64,
    /// Hard ceiling: never donate more than this regardless of edge.
    pub tip_ceil_lamports: u64,
}

impl Config {
    /// Build from environment variables. Returns an error if any required var is absent
    /// or any value fails to parse. Logs effective safety flags at INFO level.
    pub fn from_env() -> Result<Self> {
        let dry_run = parse_bool_env("ARB_DRY_RUN", true)?;
        let require_confirm = parse_bool_env("ARB_REQUIRE_CONFIRM", true)?;

        // Required endpoints -- no defaults; missing = startup failure.
        let rpc_url = require_env("ARB_RPC_URL")?;
        let grpc_url = require_env("ARB_GRPC_URL")?;
        let grpc_token = require_env("ARB_GRPC_TOKEN")?;
        let jito_block_engine_url = require_env("ARB_JITO_URL")?;

        // Risk caps -- expressed in SOL for human readability, stored as lamports.
        let max_notional_lamports = sol_to_lamports(
            parse_f64_env("ARB_MAX_NOTIONAL_SOL", 1.0)
                .context("ARB_MAX_NOTIONAL_SOL")?,
        )?;
        let max_position_lamports = sol_to_lamports(
            parse_f64_env("ARB_MAX_POSITION_SOL", 2.0)
                .context("ARB_MAX_POSITION_SOL")?,
        )?;
        let max_daily_loss_lamports = sol_to_lamports(
            parse_f64_env("ARB_MAX_DAILY_LOSS_SOL", 0.5)
                .context("ARB_MAX_DAILY_LOSS_SOL")?,
        )?;
        let max_consecutive_reverts =
            parse_u32_env("ARB_MAX_CONSECUTIVE_REVERTS", 5).context("ARB_MAX_CONSECUTIVE_REVERTS")?;

        let spread_threshold_bps =
            parse_u32_env("ARB_SPREAD_THRESHOLD_BPS", 30).context("ARB_SPREAD_THRESHOLD_BPS")?;

        let cu_limit_estimate =
            parse_u32_env("ARB_CU_LIMIT_ESTIMATE", 200_000).context("ARB_CU_LIMIT_ESTIMATE")?;
        let cu_margin_bps =
            parse_u32_env("ARB_CU_MARGIN_BPS", 1500).context("ARB_CU_MARGIN_BPS")?;
        let cu_price_microlamports =
            parse_u64_env("ARB_CU_PRICE_MICROLAMPORTS", 10_000).context("ARB_CU_PRICE_MICROLAMPORTS")?;

        let tip_bps = parse_u32_env("ARB_TIP_BPS", 5000).context("ARB_TIP_BPS")?;
        let tip_floor_lamports =
            parse_u64_env("ARB_TIP_FLOOR_LAMPORTS", 1_000).context("ARB_TIP_FLOOR_LAMPORTS")?;
        let tip_ceil_lamports =
            parse_u64_env("ARB_TIP_CEIL_LAMPORTS", 10_000_000).context("ARB_TIP_CEIL_LAMPORTS")?;
        if tip_floor_lamports > tip_ceil_lamports {
            bail!(
                "ARB_TIP_FLOOR_LAMPORTS ({}) must not exceed ARB_TIP_CEIL_LAMPORTS ({})",
                tip_floor_lamports,
                tip_ceil_lamports
            );
        }

        let cfg = Config {
            dry_run,
            require_confirm,
            rpc_url,
            grpc_url,
            grpc_token,
            jito_block_engine_url,
            risk_limits: RiskLimits {
                max_notional_lamports,
                max_position_lamports,
                max_daily_loss_lamports,
                max_consecutive_reverts,
            },
            spread_threshold_bps,
            cu_limit_estimate,
            cu_margin_bps,
            cu_price_microlamports,
            tip_bps,
            tip_floor_lamports,
            tip_ceil_lamports,
        };

        // Emit safety + tip posture at startup so it is visible in logs.
        eprintln!(
            "[config] tip_bps={} tip_floor={} tip_ceil={} lamports",
            cfg.tip_bps, cfg.tip_floor_lamports, cfg.tip_ceil_lamports,
        );
        eprintln!(
            "[config] DRY_RUN={} REQUIRE_CONFIRM={} max_notional={:.4} SOL max_daily_loss={:.4} SOL",
            cfg.dry_run,
            cfg.require_confirm,
            max_notional_lamports as f64 / LAMPORTS_PER_SOL as f64,
            max_daily_loss_lamports as f64 / LAMPORTS_PER_SOL as f64,
        );

        if !cfg.dry_run {
            eprintln!(
                "[config] WARNING: DRY_RUN=false -- live submission is ENABLED. \
                 REQUIRE_CONFIRM={} governs the confirm-gate.",
                cfg.require_confirm
            );
        }

        Ok(cfg)
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn require_env(key: &str) -> Result<String> {
    std::env::var(key).with_context(|| format!("required env var {} is not set", key))
}

fn parse_bool_env(key: &str, default: bool) -> Result<bool> {
    match std::env::var(key) {
        Err(_) => Ok(default),
        Ok(v) => match v.to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" => Ok(true),
            "false" | "0" | "no" => Ok(false),
            other => bail!("env var {} has invalid bool value {:?}; use true/false", key, other),
        },
    }
}

fn parse_u32_env(key: &str, default: u32) -> Result<u32> {
    match std::env::var(key) {
        Err(_) => Ok(default),
        Ok(v) => v.parse::<u32>().with_context(|| format!("env var {} must be u32, got {:?}", key, v)),
    }
}

fn parse_u64_env(key: &str, default: u64) -> Result<u64> {
    match std::env::var(key) {
        Err(_) => Ok(default),
        Ok(v) => v.parse::<u64>().with_context(|| format!("env var {} must be u64, got {:?}", key, v)),
    }
}

fn parse_f64_env(key: &str, default: f64) -> Result<f64> {
    match std::env::var(key) {
        Err(_) => Ok(default),
        Ok(v) => v.parse::<f64>().with_context(|| format!("env var {} must be f64, got {:?}", key, v)),
    }
}

fn sol_to_lamports(sol: f64) -> Result<u64> {
    if sol < 0.0 {
        bail!("SOL amount must be non-negative, got {}", sol);
    }
    Ok((sol * LAMPORTS_PER_SOL as f64).round() as u64)
}
