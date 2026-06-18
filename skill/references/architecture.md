# Hot-Path Architecture

The execution hot path is a single pipeline: **stream -> detect -> simulate -> bundle -> land**. Each stage feeds the next; the only stage that touches funds is the last, and it never runs without passing the risk gate and the confirm-gate.

```
[yellowstone gRPC]   account/slot updates (processed commitment)
        |
        v
   stream.rs  ---- decode pool/account state
        |
        v
   detector.rs ---- compute post-fee/post-slippage spread vs threshold
        |
        v
   RiskGate.check_order (risk.rs) ---- caps, circuit breaker, kill-switch
        |
        v
   jito.rs  ---- build bundle (<=5 tx, tip LAST), SIMULATE FIRST
        |
        v
   confirm-gate ---- DRY_RUN logs intent; live send requires typed human confirm
        |
        v
   send bundle -> leader
```

## Where Rust matters (and where it does not)

Rust buys you a tight, allocation-free inner loop and predictable tail latency: decode, spread math, and the risk check should run in microseconds with no GC pauses. That matters because the window between observing a price and a competitor landing is small.

Rust does **not** buy you co-location, a private mempool, or a faster path to the leader. Those are infrastructure (RPC/gRPC proximity, ShredStream access, Jito relay quality). If your network RTT dominates, shaving microseconds off decode changes nothing. Profile before optimizing: measure detection latency and bundle-build latency separately, and compare both against your observed land/miss rate.

## Latency budget realities

- A Solana slot is ~400ms; leaders rotate every 4 slots (~1.6s per leader). Your bundle must reach the current or next leader before the opportunity is consumed on-chain.
- Useful split of the budget: **ingestion** (gRPC delivery + decode), **decision** (spread + risk), **build+sign** (instructions + tip), **transport** (to relay/leader). Transport and ingestion usually dominate; decision should be negligible.
- `processed` commitment gives you the freshest view for detection. It is NOT confirmed and can be rolled back, so treat detected state as a hypothesis, not truth. The on-chain simulation and the transaction's own account constraints are what protect you; detection optimism is fine because simulate-first catches stale reads.

## processed vs confirmed

- **Detection**: subscribe at `processed` for the lowest-latency view of pool state. Accept that some reads will be stale or dropped.
- **Settlement / accounting**: confirm fills at `confirmed` (or `finalized` for high-value reconciliation) before updating realized P&L in the risk gate. Never mark a fill or compute daily loss off a `processed` read.

## Honest economics

Spatial DEX arbitrage on Solana is infrastructure-dominated and heavily contested. Raw price gaps are arbitraged away by parties with better network position than a single-box bot will have. A retail operator's durable edge is **not** raw speed; it is:

- **Correct landing**: simulate-first, right CU price, Jito tip sizing, and retry discipline so the txns you do send actually land instead of burning fees on reverts.
- **Risk discipline**: caps, circuit breaker, and kill-switch so a bad streak or a stale-read storm cannot drain the account.
- **Less-contested opportunities**: funding-basis, cross-venue rate gaps, and longer-horizon plays where latency is not the deciding factor.

This skill is scaffolding plus knowledge, not a hosted bot and not a guaranteed profit machine. See `safety-rails.md`.

## Routing

- Stream/ingestion details -> `streaming-ingestion.md`
- Spread/threshold math -> `opportunity-detection.md`
- Bundle/tip/leader timing -> `jito-bundles.md`
- Priority fee/simulate/retries -> `transaction-landing.md`
- Caps/breaker/kill-switch -> `risk-and-killswitch.md`
- Program/Anchor/signing -> `delegation.md`
