---
globs:
  - "src/**/*.rs"
---

# Rule: No Auto-Execute of Fund-Moving Transactions

These rules apply to all Rust source files in this crate. They are
load-bearing safety constraints, not style suggestions. Violations must
be treated as blocking errors in code review.

## Mandatory simulate-before-submit

Every transaction MUST be simulation-checked before it is sent to any RPC
or bundled via Jito. Simulation must use the same ComputeBudget instructions
(priority fee + CU limit) that the real transaction will carry. A function
that builds a transaction and submits it without a prior simulate call is
a hard bug.

Pattern to enforce:

```
// CORRECT
let sim_result = rpc.simulate_transaction(&tx)?;
check_sim_ok(&sim_result)?;
// ... only now proceed to bundle / send
bundle.send_if_confirmed().await?;

// WRONG - never do this
bundle.send().await?;  // no simulate
```

## Mandatory confirm-gate before any fund-moving send

No code path may send a signed, fund-moving transaction automatically.
Before any `send_bundle`, `send_transaction`, or equivalent call the code
MUST check that the user has explicitly confirmed the action at runtime.

The confirm-gate is implemented via the `REQUIRE_CONFIRM` env var
(default: `true`) plus a blocking prompt or pre-authorized flag that the
operator sets explicitly before each run. No flag, environment variable,
or build-time feature may remove or bypass this gate.

```rust
// CORRECT - gate enforced in config.rs
if config.require_confirm {
    let confirmed = prompt_operator_confirm(&bundle_summary)?;
    if !confirmed {
        anyhow::bail!("operator did not confirm; bundle aborted");
    }
}

// WRONG - gate commented out, overridden, or absent
// config.require_confirm is ignored
rpc_client.send_transaction(&tx)?;
```

## DRY_RUN default

`DRY_RUN` defaults to `true`. In dry-run mode the execution path MUST
log the intended action (opportunity, fee cost, expected profit) and
return without sending. Enabling live mode requires an explicit opt-in:
set `DRY_RUN=false` in the environment before launch.

## Mandatory position caps and daily-loss limit

All order submission paths MUST call `RiskGate::check_order` before
proceeding. The gate enforces:

- `max_notional_lamports` - single-order notional cap
- `max_position_lamports` - total open position cap
- `max_daily_loss_lamports` - cumulative daily loss stop
- `max_consecutive_reverts` - circuit breaker trip count

If `check_order` returns `Err`, the order MUST be dropped without any
fallback attempt to send it directly.

## Kill-switch must be checked on every hot-path iteration

The main execution loop MUST call `RiskGate::is_killed()` at the top of
each iteration. If killed, the loop MUST exit cleanly and log the reason.
No code path may reset the kill-switch programmatically without operator
intervention.

## Circuit breaker

When `max_consecutive_reverts` consecutive transactions revert, the
`RiskGate` trips automatically. After a trip:

1. All new orders are rejected with `Err` until the gate is manually reset.
2. The reason for the trip is logged at ERROR level.
3. Reset requires an operator call to `RiskGate::reset()` - it MUST NOT
   happen automatically on a timer or retry.

## No bypass via feature flags or conditional compilation

No `#[cfg(...)]` block, no environment variable, no CLI flag may:
- skip the simulate step
- bypass the confirm-gate
- disable the RiskGate caps
- auto-reset the kill-switch

Code that introduces such a bypass will be rejected in review.

## Scaffolding disclaimer

This codebase is scaffolding and educational reference. It is NOT a
hosted, auto-running trading bot. The operator is responsible for:
- key management and wallet custody
- reviewing every live-mode run before authorizing
- setting conservative caps appropriate to their risk tolerance
- understanding that the spatial-arb arena is infrastructure-dominated;
  durable edge comes from correct landing discipline and less-contested
  opportunity types, not from this software alone
