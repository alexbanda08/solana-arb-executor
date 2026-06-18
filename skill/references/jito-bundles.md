# Jito Bundles

Maps to `skill/templates/src/jito.rs`. Crate: `jito-sdk-rust = "0.3"`.

> MATURITY FLAG: `jito-sdk-rust` is 0.x (early). The 0.x API can change between
> minor releases and surface area is incomplete. Pin `"0.3"`, re-verify on update
> (see references/sdk-versions.md), and keep a direct Block Engine JSON-RPC
> fallback in mind. Treat method names below as a guide and follow `cargo build`.

## What a bundle gives you
A Jito bundle is an ordered list of up to 5 transactions that land atomically and
sequentially within one block, or not at all (all-or-nothing). This is what makes
arb execution safe to submit: either the whole detect->swap->swap path lands, or
nothing does -> no half-filled leg. You attach a TIP to win inclusion; tips are
priced by competition, not by a fixed fee.

## Hard rules
- <= 5 transactions per bundle.
- TIP goes in the LAST transaction of the bundle (a transfer to a Jito tip
  account). Putting the tip earlier lets a partial prefix pay without your arb
  landing; last-tx tip means you only tip if the whole bundle is included.
- Tip accounts ROTATE. Fetch the current tip accounts at runtime; do not hardcode.
- A bundle either lands fully or not at all; design every tx to be safe to retry as
  a fresh bundle (new blockhash) on failure.

## Mandatory flow (jito.rs)
1. Build the arb transactions (swap legs). Delegate program/instruction/signing
   construction to ../solana-dev (see references/delegation.md). Use a FRESH recent
   blockhash (references/transaction-landing.md).
2. SIMULATE FIRST. Simulate the path against current state via RPC
   (`simulateTransaction`, see references/transaction-landing.md). If the simulated
   net is not strictly profitable AFTER fees + tip, ABORT. No simulate, no send.
3. Re-check economics with RiskGate.check_order (references/risk-and-killswitch.md):
   notional/position caps, daily-loss, circuit breaker, kill-switch.
4. Fetch current Jito tip accounts; pick one.
5. Compute tip from expected edge (see "Tip sizing"); append the tip transfer as
   the LAST transaction.
6. CONFIRM-GATE: if `REQUIRE_CONFIRM` (default true) or `DRY_RUN` (default true),
   DO NOT send. In DRY_RUN, log the fully-built bundle + intended tip and stop.
   Live send requires an explicit typed human confirmation (safety canon,
   references/safety-rails.md). No flag bypasses this gate.
7. Submit the bundle to the Block Engine. Capture the bundle id/uuid.
8. Poll bundle status until landed/failed/timeout; reconcile PnL on `confirmed`.

## Leader-aware submit
- Bundles are processed by the current/next Jito leader. Submitting when a Jito
  leader is at or near the top of the slot schedule improves inclusion odds.
- Use the slot clock from the stream (references/streaming-ingestion.md) plus the
  leader schedule to time submission. If no Jito leader is in the immediate window,
  expect lower inclusion and either skip or accept the lower odds; do not blindly
  spam re-submits.

## Tip sizing
- Tip must come out of expected edge, and net must stay positive:
  `net = expected_profit - base_fees - priority_fee - tip`.
  base_fees + priority_fee from references/transaction-landing.md (fees.rs).
- Tipping ~too low loses inclusion to competitors; tipping ~too high donates your
  edge. Express tip as a fraction of expected edge with a hard floor and a hard
  ceiling, and ABORT if the resulting `net <= min_profit_threshold`.
- There is no guaranteed-inclusion tip. Spatial arb is infra-dominated; the durable
  edge is correct landing + risk discipline + less-contested opportunities, not a
  bigger tip (see README economics note).

## Sketch (jito.rs)
```rust
use anyhow::{bail, Result};
use jito_sdk_rust::JitoJsonRpcSDK;

pub struct BundleCtx {
    pub dry_run: bool,
    pub require_confirm: bool,
    pub min_profit_lamports: u64,
}

pub async fn submit_arb_bundle(
    jito: &JitoJsonRpcSDK,
    txs_b64: Vec<String>,      // arb legs already built+signed elsewhere
    sim_net_lamports: i128,    // result of simulate-first, after fees
    tip_lamports: u64,
    ctx: &BundleCtx,
) -> Result<Option<String>> {
    if txs_b64.is_empty() || txs_b64.len() > 5 {
        bail!("bundle must contain 1..=5 transactions");
    }
    // 2/3: economics gate (simulate already done by caller)
    if sim_net_lamports < ctx.min_profit_lamports as i128 {
        bail!("simulated net below threshold; abort (no send)");
    }

    // 4: fetch CURRENT tip accounts (rotates) - method per 0.3 API, verify via cargo build
    let _tip_accounts = jito.get_tip_accounts().await?;
    // 5: caller must have appended the tip transfer (to a fetched tip account)
    //    as the LAST tx in txs_b64. tip_lamports is recorded for logging.
    let _ = tip_lamports;

    // 6: CONFIRM-GATE - never auto-send
    if ctx.dry_run || ctx.require_confirm {
        // log fully-built bundle + intended tip; do NOT send.
        // live path requires explicit typed human confirmation upstream.
        return Ok(None);
    }

    // 7: submit (only reached after explicit confirmation upstream)
    let resp = jito.send_bundle(serde_json::json!({ "transactions": txs_b64 }), None).await?;
    let bundle_id = resp
        .get("result").and_then(|v| v.as_str())
        .map(|s| s.to_string());
    Ok(bundle_id)
    // 8: caller polls get_bundle_statuses(bundle_id) until landed/failed/timeout.
}
```
The JSON shapes/method names above are illustrative for 0.3; confirm exact
signatures with `cargo build` and the installed crate docs. Do not ship invented
methods.

## Status polling
- After submit, poll bundle status (`get_bundle_statuses` / inflight) on a short
  interval with a timeout of a few slots. Outcomes: Landed, Failed/Dropped, Pending.
- On Failed/Dropped: do NOT auto-resend. Re-detect, re-simulate, rebuild with a
  fresh blockhash, and re-run the confirm-gate. Treat repeated reverts as input to
  the circuit breaker (record_revert, references/risk-and-killswitch.md).

## Checklist
- [ ] <= 5 tx; tip transfer is the LAST tx.
- [ ] Tip account fetched at runtime (rotates), never hardcoded.
- [ ] Simulate-first; abort if net <= threshold after fees + tip.
- [ ] RiskGate.check_order passed before any submit.
- [ ] Confirm-gate enforced; DRY_RUN/REQUIRE_CONFIRM default true; no bypass.
- [ ] Leader-aware timing; status polled; no blind auto-resend.
