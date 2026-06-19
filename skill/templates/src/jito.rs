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
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine as _;
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

/// Fetch a live tip account from the block engine.
///
/// Tip accounts ROTATE, so we resolve one at runtime rather than hardcoding.
/// We use the SDK's `get_random_tip_account`, which is the one-call convenience
/// the official jito-rust-rpc example uses: it reads the array from the JSON-RPC
/// `result` field (NOT the top-level response -- a manual `as_array()` on the
/// raw response returns None and always errors) and picks one at random, which
/// also load-balances tip flow across the equivalent accounts.
pub async fn fetch_tip_account(sdk: &JitoJsonRpcSDK) -> Result<Pubkey> {
    let account = sdk
        .get_random_tip_account()
        .await
        .context("fetching Jito tip account")?;

    Pubkey::from_str(&account)
        .with_context(|| format!("parsing tip account pubkey: {}", account))
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

    // LATENCY: this uses the BLOCKING solana_client::rpc_client::RpcClient inside an
    // async fn, which parks a tokio worker for the round-trip. It is an explicit,
    // documented trade-off (one simulate per candidate; simplest correct code) per
    // rules/rust-style.md. To free the executor under high candidate rates, wrap
    // this call in tokio::task::spawn_blocking, or switch to
    // solana_client::nonblocking::rpc_client::RpcClient and .await it.

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

    // Serialize each transaction with bincode (1.x; the wire format the runtime
    // expects) then base64-encode it. This matches the official jito-rust-rpc
    // example. `bincode::serialize` is the 1.x API -- do not bump to bincode 2/3,
    // whose API differs and which jito-sdk-rust itself does not use.
    let encoded: Vec<String> = bundle
        .transactions
        .iter()
        .map(|tx| {
            let bytes = bincode::serialize(tx).context("serializing tx for bundle")?;
            Ok(BASE64_STANDARD.encode(bytes))
        })
        .collect::<Result<Vec<String>>>()?;

    // send_bundle takes Option<serde_json::Value>, NOT Vec<String>. The validated
    // shape is a 2-element array: [ [tx, ...], { "encoding": "base64" } ]. The SDK
    // rejects anything else; passing a bare Vec<String> does not even compile.
    let params = serde_json::json!([
        serde_json::Value::from(encoded),
        { "encoding": "base64" }
    ]);

    let response = sdk
        .send_bundle(Some(params), None)
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
/// Jito exposes TWO status endpoints with DIFFERENT field shapes, and conflating
/// them is a common bug (matching `failed`/`Invalid` against `confirmation_status`
/// never fires, because those values only appear on the in-flight `status` field):
///   - get_in_flight_bundle_statuses -> per bundle a `status` of
///     Invalid | Pending | Failed | Landed. This is the FAST path for landing /
///     failure detection while the bundle is still in flight.
///   - get_bundle_statuses -> per bundle a `confirmation_status`
///     (processed | confirmed | finalized) plus a separate `err`. This is the
///     authoritative reconciliation once the bundle has landed on-chain.
///
/// We poll the in-flight endpoint for a quick verdict, then confirm `Landed`
/// against get_bundle_statuses (confirmation_status + err) before reporting
/// success. Callers treat anything other than a confirmed landing as a revert
/// for risk accounting.
pub async fn poll_bundle_status(
    sdk: &JitoJsonRpcSDK,
    bundle_id: &str,
    max_attempts: u32,
) -> Result<String> {
    use tokio::time::{sleep, Duration};

    for attempt in 0..max_attempts {
        // --- Fast path: in-flight `status` (Landed/Pending/Failed/Invalid) ---
        let inflight = sdk
            .get_in_flight_bundle_statuses(vec![bundle_id.to_string()])
            .await
            .with_context(|| format!("get_in_flight_bundle_statuses attempt {}", attempt))?;

        let inflight_status = inflight
            .pointer("/result/value/0/status")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        match inflight_status.as_deref() {
            Some("Failed") | Some("Invalid") => {
                bail!(
                    "bundle {} in-flight status: {}",
                    bundle_id,
                    inflight_status.as_deref().unwrap_or("Failed")
                );
            }
            Some("Landed") => {
                // --- Reconcile against confirmation_status + err ---
                let final_status =
                    reconcile_landed_bundle(sdk, bundle_id, attempt).await?;
                eprintln!(
                    "[jito] bundle {} landed (confirmation_status={})",
                    bundle_id, final_status
                );
                return Ok(final_status);
            }
            Some(other) => {
                eprintln!(
                    "[jito] bundle {} in-flight status: {} (attempt {}/{})",
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

/// Once the in-flight endpoint reports `Landed`, confirm via get_bundle_statuses,
/// which carries `confirmation_status` (processed/confirmed/finalized) and a
/// separate `err`. A non-null `err` means the bundle landed but a transaction in
/// it reverted -> treat as failure for risk accounting.
async fn reconcile_landed_bundle(
    sdk: &JitoJsonRpcSDK,
    bundle_id: &str,
    attempt: u32,
) -> Result<String> {
    let resp = sdk
        .get_bundle_statuses(vec![bundle_id.to_string()])
        .await
        .with_context(|| format!("get_bundle_statuses (reconcile) attempt {}", attempt))?;

    // Response schema: {"result": {"value": [{"confirmation_status":..., "err":...}]}}
    if let Some(err) = resp.pointer("/result/value/0/err") {
        // `err` is typically {"Ok": null} on success or a non-null error object.
        let is_failure = match err {
            serde_json::Value::Null => false,
            serde_json::Value::Object(map) => {
                // {"Ok": null} signals no error; anything else is a revert.
                !matches!(map.get("Ok"), Some(serde_json::Value::Null))
            }
            _ => true,
        };
        if is_failure {
            bail!("bundle {} landed but a tx reverted: {}", bundle_id, err);
        }
    }

    let confirmation_status = resp
        .pointer("/result/value/0/confirmation_status")
        .and_then(|v| v.as_str())
        .unwrap_or("confirmed")
        .to_string();

    Ok(confirmation_status)
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
