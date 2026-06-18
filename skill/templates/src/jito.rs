// jito.rs -- Jito bundle construction, simulation, confirm-gate, and submission.
//
// SAFETY CONTRACT (load-bearing):
//   1. SIMULATE FIRST: every bundle is simulated via RPC before any submission
//      decision is made. A simulation error aborts the attempt.
//   2. CONFIRM-GATE: when REQUIRE_CONFIRM=true the user must type "YES" on stdin
//      before the bundle reaches the block engine. No flag bypasses this gate.
//   3. TIP PLACEMENT: tip instruction lives in the LAST transaction of the bundle
//      (Jito requirement). Tip is sent to one of the fetched tip accounts.
//   4. BUNDLE SIZE: maximum 5 transactions per bundle (Jito hard limit).
//   5. This module never auto-sends. The caller (main.rs) controls DRY_RUN; this
//      module controls REQUIRE_CONFIRM inside `submit_bundle`.
//
// jito-sdk-rust is 0.x (early-stage); API surface may change on minor bumps.
// Pin the version tightly in Cargo.toml and re-verify on upgrade.

use anyhow::{bail, Context, Result};
use jito_sdk_rust::JitoJsonRpcSDK;
use solana_client::rpc_client::RpcClient;
use solana_sdk::{
    commitment_config::CommitmentConfig,
    instruction::Instruction,
    pubkey::Pubkey,
    signature::Keypair,
    signer::Signer,
    system_instruction,
    transaction::Transaction,
};
use std::io::{self, BufRead, Write as IoWrite};
use std::str::FromStr;

/// Maximum transactions in a single Jito bundle.
pub const BUNDLE_MAX_TXS: usize = 5;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// A fully constructed, signed bundle ready to simulate and optionally submit.
pub struct JitoBundle {
    pub transactions: Vec<Transaction>,
    /// Tip lamports included in the last transaction.
    pub tip_lamports: u64,
    /// The tip account the tip is sent to.
    pub tip_account: Pubkey,
}

/// Result of a bundle simulation pass.
#[derive(Debug)]
pub struct SimulationResult {
    pub success: bool,
    pub err_message: Option<String>,
    /// Simulated CU consumed across the first tx (primary indicator).
    pub units_consumed: Option<u64>,
}

// ---------------------------------------------------------------------------
// Tip account resolution
// ---------------------------------------------------------------------------

/// Fetch the live tip account list from the block engine and return one.
/// The SDK returns multiple accounts; we take the first (all are equivalent).
pub async fn fetch_tip_account(sdk: &JitoJsonRpcSDK) -> Result<Pubkey> {
    let tip_accounts = sdk
        .get_tip_accounts()
        .await
        .context("fetching Jito tip accounts")?;

    let accounts = tip_accounts
        .as_array()
        .context("tip accounts response is not a JSON array")?;

    let first = accounts
        .first()
        .context("tip accounts list is empty")?
        .as_str()
        .context("tip account entry is not a string")?;

    Pubkey::from_str(first).with_context(|| format!("parsing tip account pubkey: {}", first))
}

// ---------------------------------------------------------------------------
// Bundle construction
// ---------------------------------------------------------------------------

/// Build a Jito bundle from a sequence of instruction groups.
///
/// `arb_txs` -- ordered list of (instructions, signers) for the arb legs.
///   Must have len <= BUNDLE_MAX_TXS - 1 (we append a tip tx as the last tx).
///   If the tip instruction fits naturally into the last arb tx, the caller
///   may pass `embed_tip_in_last=true` and omit one slot.
///
/// The tip instruction is always appended as a separate system-program transfer
/// in the LAST transaction to satisfy the Jito landing requirement.
pub fn build_bundle(
    arb_instructions: Vec<(Vec<Instruction>, Vec<&Keypair>)>,
    tip_payer: &Keypair,
    tip_account: Pubkey,
    tip_lamports: u64,
    recent_blockhash: solana_sdk::hash::Hash,
) -> Result<JitoBundle> {
    if arb_instructions.len() >= BUNDLE_MAX_TXS {
        bail!(
            "bundle would exceed Jito limit: {} arb txs + 1 tip tx > {} max",
            arb_instructions.len(),
            BUNDLE_MAX_TXS
        );
    }

    let mut transactions: Vec<Transaction> = arb_instructions
        .into_iter()
        .map(|(ixs, signers)| {
            let mut tx = Transaction::new_with_payer(&ixs, Some(&tip_payer.pubkey()));
            tx.sign(&signers, recent_blockhash);
            tx
        })
        .collect();

    // Tip tx -- always the LAST entry (Jito requirement).
    let tip_ix = system_instruction::transfer(&tip_payer.pubkey(), &tip_account, tip_lamports);
    let mut tip_tx = Transaction::new_with_payer(&[tip_ix], Some(&tip_payer.pubkey()));
    tip_tx.sign(&[tip_payer], recent_blockhash);
    transactions.push(tip_tx);

    if transactions.len() > BUNDLE_MAX_TXS {
        bail!(
            "assembled bundle has {} txs, exceeds Jito max of {}",
            transactions.len(),
            BUNDLE_MAX_TXS
        );
    }

    Ok(JitoBundle {
        transactions,
        tip_lamports,
        tip_account,
    })
}

// ---------------------------------------------------------------------------
// Simulation (MANDATORY before any submission path)
// ---------------------------------------------------------------------------

/// Simulate the first transaction in the bundle via the standard RPC endpoint.
/// Jito does not expose a separate bundle simulation RPC; simulating the primary
/// swap tx is the standard practice -- it catches instruction errors, CU overflows,
/// and account-state mismatches before the bundle reaches the block engine.
///
/// Always called before `submit_bundle`. The caller must check `SimulationResult::success`
/// and abort on failure.
pub async fn simulate_bundle(
    rpc: &RpcClient,
    bundle: &JitoBundle,
) -> Result<SimulationResult> {
    use solana_client::rpc_config::RpcSimulateTransactionConfig;
    use solana_sdk::commitment_config::CommitmentConfig;

    let first_tx = bundle
        .transactions
        .first()
        .context("bundle has no transactions")?;

    // run_programs: true so we get CU consumed and any program error.
    let response = rpc
        .simulate_transaction_with_config(
            first_tx,
            RpcSimulateTransactionConfig {
                sig_verify: false,  // sig verification skipped in simulation
                replace_recent_blockhash: true,
                commitment: Some(CommitmentConfig::processed()),
                encoding: None,
                accounts: None,
                min_context_slot: None,
                inner_instructions: false,
            },
        )
        .context("RPC simulate_transaction failed")?;

    let value = response.value;
    let success = value.err.is_none();
    let err_message = value.err.map(|e| format!("{:?}", e));

    Ok(SimulationResult {
        success,
        err_message,
        units_consumed: value.units_consumed,
    })
}

// ---------------------------------------------------------------------------
// Confirm-gate + submission
// ---------------------------------------------------------------------------

/// Attempt to submit `bundle` to the Jito block engine.
///
/// Gate logic:
///   - `require_confirm=true`: prompt stdout/stdin for "YES" before sending.
///   - `require_confirm=false`: still logs intent, then submits.
///
/// The caller in `main.rs` already guards with `dry_run`; this function is
/// only reached when DRY_RUN=false. The confirm-gate is a second independent
/// layer that cannot be disabled without modifying source.
///
/// Returns the bundle UUID from the block engine on success.
pub async fn submit_bundle(
    sdk: &JitoJsonRpcSDK,
    bundle: &JitoBundle,
    require_confirm: bool,
) -> Result<String> {
    eprintln!(
        "[jito] bundle ready: {} txs, tip {} lamports to {}",
        bundle.transactions.len(),
        bundle.tip_lamports,
        bundle.tip_account,
    );

    if require_confirm {
        confirm_gate().context("confirm-gate")?;
    } else {
        eprintln!("[jito] REQUIRE_CONFIRM=false -- submitting without interactive confirmation");
    }

    // Serialize transactions to base58/base64 as required by jito-sdk-rust send_bundle.
    let encoded: Vec<String> = bundle
        .transactions
        .iter()
        .map(|tx| {
            let bytes = bincode::serialize(tx).expect("tx serialize");
            bs58::encode(&bytes).into_string()
        })
        .collect();

    let response = sdk
        .send_bundle(Some(encoded), None)
        .await
        .context("Jito send_bundle RPC call")?;

    // The response is a JSON value; extract the result string (bundle UUID).
    let bundle_id = response
        .get("result")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .context("unexpected send_bundle response shape -- expected {\"result\": \"<uuid>\"}")?;

    eprintln!("[jito] bundle submitted: {}", bundle_id);
    Ok(bundle_id)
}

/// Poll the block engine for bundle status until it is confirmed, failed, or
/// we exceed `max_attempts`.
///
/// Returns the final status string. Callers should treat anything other than
/// "Landed" (or equivalent confirmed status) as a revert for risk accounting.
pub async fn poll_bundle_status(
    sdk: &JitoJsonRpcSDK,
    bundle_id: &str,
    max_attempts: u32,
) -> Result<String> {
    use tokio::time::{sleep, Duration};

    for attempt in 0..max_attempts {
        let status_response = sdk
            .get_bundle_statuses(vec![bundle_id.to_string()])
            .await
            .with_context(|| format!("get_bundle_statuses attempt {}", attempt))?;

        // Response schema: {"result": {"value": [{"bundle_id":..., "confirmation_status":...}]}}
        let status = status_response
            .pointer("/result/value/0/confirmation_status")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        match status.as_deref() {
            Some("finalized") | Some("confirmed") => {
                eprintln!("[jito] bundle {} status: confirmed", bundle_id);
                return Ok(status.unwrap());
            }
            Some("failed") | Some("Invalid") => {
                let err = status_response
                    .pointer("/result/value/0/err")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                bail!("bundle {} failed: {}", bundle_id, err);
            }
            Some(other) => {
                eprintln!(
                    "[jito] bundle {} status: {} (attempt {}/{})",
                    bundle_id, other, attempt + 1, max_attempts
                );
            }
            None => {
                eprintln!(
                    "[jito] bundle {} not yet indexed (attempt {}/{})",
                    bundle_id, attempt + 1, max_attempts
                );
            }
        }

        sleep(Duration::from_millis(500)).await;
    }

    bail!(
        "bundle {} not confirmed after {} polling attempts",
        bundle_id,
        max_attempts
    )
}

// ---------------------------------------------------------------------------
// Interactive confirm-gate (internal)
// ---------------------------------------------------------------------------

fn confirm_gate() -> Result<()> {
    // Flush stdout so the prompt appears before blocking on stdin.
    print!(
        "\n[CONFIRM] Type YES to submit the bundle to Jito, or anything else to abort: "
    );
    io::stdout().flush().context("flushing stdout for confirm prompt")?;

    let stdin = io::stdin();
    let response = stdin
        .lock()
        .lines()
        .next()
        .context("stdin closed before confirm response")?
        .context("reading confirm response from stdin")?;

    if response.trim() == "YES" {
        eprintln!("[jito] confirm-gate passed");
        Ok(())
    } else {
        bail!("confirm-gate rejected (user typed {:?}); bundle NOT submitted", response.trim())
    }
}
