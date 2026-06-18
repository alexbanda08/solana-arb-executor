# Transaction Landing

Maps to `skill/templates/src/fees.rs` (pure math) and the send path in
`main.rs`/`jito.rs`. The math module is std-only and self-contained so it runs
offline via `rustc --edition 2021 --test fees.rs` (see README + SHARED rules).

## Fee model (exact)
A Solana transaction fee has two parts:

1. Base fee: `5000 lamports per signature`. Most arb txs have 1 signature -> 5000.
2. Priority fee: set via the ComputeBudget program by declaring a CU price
   (microlamports per compute unit) and a CU limit.

Priority fee formula (round UP):

```
priority_fee_lamports = ceil(cu_price_microlamports * cu_limit / 1_000_000)
```

Total:

```
total_fee_lamports = 5000 * num_signatures + priority_fee_lamports
```

Use a `u128` intermediate for the multiply to avoid `u64` overflow before the
divide (cu_price and cu_limit can both be large). fees.rs does exactly this.

```rust
// fees.rs (std-only, self-contained)
pub fn priority_fee_lamports(cu_price_microlamports: u64, cu_limit: u32) -> u64 {
    let num = cu_price_microlamports as u128 * cu_limit as u128;
    // ceil division by 1_000_000
    ((num + 999_999) / 1_000_000) as u64
}

pub fn total_fee_lamports(num_sigs: u64, priority_lamports: u64) -> u64 {
    5_000u64.saturating_mul(num_sigs).saturating_add(priority_lamports)
}
```

## CU limit and margin
- Per-transaction CU cap is `1_400_000`. Never request above it.
- Set an EXPLICIT `SetComputeUnitLimit`. Do not rely on the default (200k/instruction
  heuristic) - it overcharges priority fee and risks running out of CU.
- Derive the limit from simulation's `units_consumed`, then add a safety margin so a
  slightly heavier real execution does not exceed the limit. Cap at 1_400_000.

```rust
pub fn cu_limit_with_margin(estimate: u32, margin_bps: u32) -> u32 {
    // estimate * (1 + margin_bps/10_000), capped at the per-tx max
    let scaled = estimate as u64 * (10_000u64 + margin_bps as u64) / 10_000;
    scaled.min(1_400_000) as u32
}
```

## You PAY for the declared CU limit
Priority fee is charged on the CU LIMIT you declare, not on `units_consumed`. So an
inflated limit directly burns lamports every tx. Tight-but-safe limits matter:
simulate -> take `units_consumed` -> apply a modest margin (e.g. 10-20%) -> cap.

## Simulate before submit (mandatory)
- ALWAYS `simulateTransaction` against current state before sending. Simulation
  returns `err` (would it revert), `units_consumed` (size the CU limit), logs, and
  (with config) post-execution account state to confirm the arb is still profitable.
- Simulate with `sigVerify: false` and `replaceRecentBlockhash` for fast pre-checks,
  but use a REAL fresh blockhash for the actual sent tx.
- If simulation errors or net profit after `total_fee_lamports` + Jito tip is not
  strictly above your `min_profit_threshold`, ABORT. No simulate, no send (safety
  canon, references/safety-rails.md).

## Blockhash freshness
- A `recent_blockhash` is valid ~150 slots (~60s). Stale blockhash -> the tx is
  dropped/expired, not landed.
- Fetch a fresh blockhash right before building the final tx. For the latency hot
  path, keep a recently-cached blockhash (refreshed each slot from the slot clock,
  references/streaming-ingestion.md) and re-check its age before send.
- On any rebuild/retry, get a NEW blockhash; never reuse one from a failed attempt.

## Retry and commitment
- Bundle path (preferred for arb): submit via Jito and poll bundle status
  (references/jito-bundles.md). Do NOT auto-resend a failed bundle - re-detect,
  re-simulate, rebuild with a fresh blockhash, re-run the confirm-gate.
- Plain RPC path (non-bundle): resending the SAME signed tx (same blockhash) is
  idempotent and safe; resend a few times until `confirmed` or blockhash expiry.
  Track outcome by signature; never rebuild-with-new-blockhash-and-blind-resend, or
  you risk double execution.
- Use `confirmed` for "did it land" reconciliation; `processed` is for detection
  only and can be rolled back.

## Dedicated / low-latency RPC
- Shared public RPCs add latency and rate-limit at the worst time. For execution use
  a dedicated/low-latency RPC (ideally co-located near a leader / your Block Engine
  region) for `simulateTransaction` and `sendTransaction`.
- Separate concerns: Yellowstone gRPC for ingestion, dedicated RPC for
  simulate/send, Jito Block Engine for bundle submit. One slow endpoint should not
  block the others.

## Honest note
Landing is necessary, not sufficient. Even a perfectly landed tx loses if the edge
evaporated between detect and inclusion. Re-validate economics at simulate time and
let the RiskGate cap losses (references/risk-and-killswitch.md).

## Checklist
- [ ] Explicit SetComputeUnitLimit from simulation + margin, capped at 1_400_000.
- [ ] Priority fee = ceil(cu_price*cu_limit/1e6) via u128; base 5000/sig.
- [ ] Simulate first; abort if err or net <= threshold after fees + tip.
- [ ] Fresh blockhash on every build/retry; re-check age before send.
- [ ] Bundle: no blind resend. Plain RPC: resend SAME tx only.
- [ ] Dedicated low-latency RPC for simulate/send.
