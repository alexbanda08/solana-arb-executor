# Opportunity Detection

Maps to `src/detector.rs`. The detector consumes decoded pool/account state from `stream.rs` and emits a candidate only when the **post-fee, post-slippage** spread clears a configured threshold. A raw price gap is never a trade signal on its own.

## Decode streamed state

`stream.rs` delivers raw account data at `processed` commitment. The detector decodes each watched account into the minimal shape it needs (reserves for a constant-product pool, or the relevant fields for a CLMM/orderbook venue). Keep decode allocation-free in the inner loop: parse into stack structs, do not build intermediate `Vec`/`String`. Treat every read as a hypothesis (processed can be stale or rolled back); simulate-first in `jito.rs` is the truth check, not the detector.

## Compute the spread

For a two-leg cycle (buy on venue A, sell on venue B), the gross edge is the output you would receive on B for the input spent on A, minus the input. That gross number is meaningless until you subtract:

1. **DEX/pool fees** on every leg (e.g. 30 bps each leg -> 60 bps round trip).
2. **Price impact / slippage** from your own size against pool reserves. Use the venue's actual curve (constant-product: `out = reserve_out - k / (reserve_in + in_after_fee)`), not a spot-price approximation. Impact grows with size; the optimal size is finite.
3. **Transaction costs**: base fee `5000 * num_sigs`, plus priority fee `ceil(cu_price * cu_limit / 1_000_000)`, plus the Jito tip. See `transaction-landing.md` for the fee math (`fees.rs`) and `jito-bundles.md` for tip sizing.

Net edge (lamports) = gross_out - input - pool_fees - slippage - base_fee - priority_fee - tip.

Emit a candidate only when net edge >= threshold, where the threshold is a positive margin (not zero) to absorb estimation error and the fact that competitors may move the pool before you land.

## Threshold and sizing

- Express the threshold in lamports (or bps of notional) and load it from config, not a literal in the hot loop.
- Size the trade toward the point where marginal net edge -> 0, but cap at `RiskLimits.max_notional_lamports` and the per-position cap. The risk gate (`risk.rs`) is the final authority; the detector should not propose an order that the gate will reject.
- Recompute on every relevant account update; do not cache a stale spread across slots.

## Cycle / triangular note

The same logic extends to 3+ legs (A->B->C->A) and triangular routes. The arithmetic is identical: chain the per-leg output-after-fee functions and subtract all costs from the final return-to-start amount. Caveats:

- Each extra leg adds pool fees, slippage, CU, and a strictly higher chance one leg's state is stale by land time -> raise the threshold per added hop.
- More legs means more CU; watch the 1,400,000 CU/tx ceiling and the <=5 tx/bundle limit. A long cycle that does not fit must be dropped, not truncated.
- Cycles are more competitive and more fragile than two-leg spreads; honest expectation is that most clear-on-paper cycles will not net positive after costs and contention.

## What the detector does NOT do

It does not sign, build, or send anything. It produces a typed candidate (legs, sizes, expected net edge, the accounts involved) and hands it to `RiskGate.check_order`. Execution lives in `jito.rs` behind simulate-first and the confirm-gate.
