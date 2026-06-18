---
description: Add RiskGate caps, circuit breaker, and kill-switch from risk.rs into a strategy, wiring check_order, record_fill, record_revert, and kill() into the execution path.
---

# /add-killswitch

Integrate the RiskGate (src/risk.rs) into your execution strategy: wire notional
caps, position caps, daily-loss floor, circuit-breaker consecutive-revert counter,
and the hard kill-switch into every code path that touches order submission.

Reference leaf: solana-arb-executor / references/risk-and-killswitch.md

---

## Steps

### 1. Verify risk.rs compiles standalone

Before wiring, confirm the module is self-consistent:

```sh
rustc --edition 2021 --test src/risk.rs -o risk_test && ./risk_test
```

All tests must pass (exit 0, "test result: ok"). If this fails, fix risk.rs before
proceeding. Do not wire a broken gate into the execution path.

### 2. Understand the RiskGate API

Read src/risk.rs. The public surface you will call:

```rust
// Construct once at startup from config caps.
let gate = RiskGate::new(RiskLimits {
    max_notional_lamports:      config.max_notional_lamports,
    max_position_lamports:      config.max_position_lamports,
    max_daily_loss_lamports:    config.max_daily_loss_lamports,
    max_consecutive_reverts:    config.max_consecutive_reverts,
});

// Call before building any bundle. Returns Result<(), String>: Err(reason) if any
// cap is breached, the gate is killed, or the circuit breaker is tripped.
if let Err(reason) = gate.check_order(notional_lamports, position_after_lamports) {
    // drop the order; never send.
}

// Call after a confirmed fill (successful landing). Pass realized PnL in lamports
// (negative = loss); a loss accrues toward max_daily_loss, a fill clears the
// consecutive-revert streak.
gate.record_fill(realized_pnl_lamports);

// Call after a revert (simulate-fail or on-chain revert).
gate.record_revert(); // trips circuit breaker at max_consecutive_reverts

// Manual hard stop (one-way latch; no flag bypasses a killed gate).
gate.kill();

// Inspect state.
gate.is_killed();
gate.is_breaker_tripped();
```

### 3. Wrap RiskGate in an Arc<Mutex<>> for shared access

In main.rs, the gate must be accessible from both the detector task and the signal
handler. Wrap it:

```rust
use std::sync::{Arc, Mutex};

let gate = Arc::new(Mutex::new(RiskGate::new(RiskLimits {
    max_notional_lamports:   config.max_notional_lamports,
    max_position_lamports:   config.max_position_lamports,
    max_daily_loss_lamports: config.max_daily_loss_lamports,
    max_consecutive_reverts: config.max_consecutive_reverts,
})));

// Clone for tasks that need access.
let gate_detector = Arc::clone(&gate);
let gate_signal   = Arc::clone(&gate);
```

### 4. Wire check_order into the detector result handler

In the detection result handler (detector.rs or main.rs, wherever the Opportunity
struct is consumed), add the check before any bundle construction:

```rust
// LATENCY: Mutex lock is ~50 ns; acceptable in the detect->bundle path.
let check = {
    let g = gate_detector.lock().expect("risk gate mutex poisoned");
    g.check_order(opportunity.notional_lamports, opportunity.position_after_lamports)
};

match check {
    Ok(()) => {
        // Proceed to simulate + bundle build.
        build_and_send_bundle(&opportunity, &config).await?;
    }
    Err(reason) => {
        tracing::warn!("order blocked by risk gate: {reason}");
        // Do not build a bundle. Continue to next detect cycle.
    }
}
```

This is the single enforcement point. Every code path that constructs a bundle MUST
pass through this check. If you have multiple strategy arms, each one must call
check_order independently.

### 5. Wire record_fill and record_revert into the bundle result path

After a bundle send attempt, update the gate based on outcome:

```rust
match bundle_result {
    Ok(landed) if landed => {
        let mut g = gate_detector.lock().expect("risk gate mutex poisoned");
        // realized_pnl_lamports is net of fees; negative values accrue daily loss.
        g.record_fill(realized_pnl_lamports);
        tracing::info!("bundle landed; consecutive_reverts reset");
    }
    Ok(_) | Err(_) => {
        let mut g = gate_detector.lock().expect("risk gate mutex poisoned");
        g.record_revert();
        tracing::warn!("bundle reverted or failed; consecutive_reverts incremented");
        // record_revert trips circuit breaker at max_consecutive_reverts.
        // Subsequent check_order calls will return Err until reset() is called manually.
    }
}
```

Note: reset() is NOT called automatically. A tripped circuit breaker requires
explicit operator intervention (call gate.reset() after diagnosing the revert cause).

### 6. Wire kill() into the signal handler

Install a SIGINT/SIGTERM handler in main.rs that calls kill() on the gate:

```rust
// tokio signal handling
let gate_signal_clone = Arc::clone(&gate_signal);
tokio::spawn(async move {
    tokio::signal::ctrl_c().await.expect("failed to listen for ctrl_c");
    tracing::warn!("SIGINT received; killing risk gate and shutting down");
    let mut g = gate_signal_clone.lock().expect("risk gate mutex poisoned");
    g.kill();
    // Give in-flight tasks ~500 ms to observe is_killed() and exit cleanly.
    tokio::time::sleep(Duration::from_millis(500)).await;
    std::process::exit(0);
});
```

The kill() latch is one-way. After kill(), all subsequent check_order calls return
Err regardless of caps. There is no automatic unkill. To restart after a kill, the
process must be restarted.

### 7. Add daily-loss accounting

The gate tracks max_daily_loss_lamports through `record_fill`: pass a negative
realized PnL and the gate accrues it (saturating) toward the daily-loss cap. A
revert that burned fees is a realized loss, so record it as a negative fill rather
than inventing a separate API:

```rust
// A bundle landed but the arb came out negative (fees > edge): record the loss.
gate.record_fill(realized_pnl_lamports); // e.g. -3_500 lamports of burned fees

// A bundle dropped/reverted with no on-chain effect: count it toward the breaker.
gate.record_revert();

// If a dropped attempt still cost lamports you want to charge against the daily
// cap, record both: the loss as a negative fill, then the revert for the breaker.
// Note record_fill clears the consecutive-revert streak, so order them as your
// accounting requires (call record_revert after record_fill if both apply).
```

This keeps daily-loss tracking in the gate (one place, via record_fill) rather than
distributed across strategy arms. Do not add a parallel loss path that bypasses the
gate's saturating accounting.

### 8. Test the circuit breaker in isolation

Use the std-only test harness (no cargo build needed):

```sh
rustc --edition 2021 --test src/risk.rs -o risk_test && ./risk_test -- circuit_breaker
```

If there is no dedicated circuit_breaker test, add one to risk.rs:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn circuit_breaker_trips_at_limit() {
        let mut gate = RiskGate::new(RiskLimits {
            max_notional_lamports:   1_000_000_000,
            max_position_lamports:   2_000_000_000,
            max_daily_loss_lamports: 500_000_000,
            max_consecutive_reverts: 3,
        });
        assert!(gate.check_order(100_000, 100_000).is_ok());
        gate.record_revert();
        gate.record_revert();
        gate.record_revert(); // trips at 3
        let result = gate.check_order(100_000, 100_000);
        assert!(result.is_err(), "circuit breaker must block orders after tripping");
        let msg = result.unwrap_err();
        assert!(msg.contains("circuit breaker") || msg.contains("revert"),
            "error message must name the cause: {msg}");
    }

    #[test]
    fn kill_blocks_all_orders() {
        let mut gate = RiskGate::new(RiskLimits {
            max_notional_lamports:   1_000_000_000,
            max_position_lamports:   2_000_000_000,
            max_daily_loss_lamports: 500_000_000,
            max_consecutive_reverts: 10,
        });
        assert!(gate.check_order(100_000, 100_000).is_ok());
        gate.kill();
        let result = gate.check_order(1, 1); // even trivially small order
        assert!(result.is_err(), "killed gate must block all orders");
    }
}
```

Recompile and run after adding the tests.

### 9. Smoke-test with DRY_RUN=true

With the gate wired:

```sh
RUST_LOG=debug ARB_DRY_RUN=true ARB_REQUIRE_CONFIRM=true cargo run 2>&1 | grep -E "risk|order|kill|circuit"
```

Expected: "order blocked" or "DRY_RUN: would send bundle" log lines, never a live
bundle send. Confirm no live transactions are submitted during a dry-run session by
checking your RPC provider's transaction history for your keypair.

### 10. Final wiring checklist

- [ ] risk.rs standalone tests pass (step 1).
- [ ] RiskGate wrapped in Arc<Mutex<>> in main.rs.
- [ ] check_order called before every bundle construction (step 4).
- [ ] record_fill called on confirmed landed bundle (step 5).
- [ ] record_revert called on every revert or simulate failure (step 5).
- [ ] kill() wired to SIGINT/SIGTERM handler (step 6).
- [ ] Daily-loss accounting feeding the gate (step 7).
- [ ] Circuit breaker test passes (step 8).
- [ ] Dry-run smoke test shows no live tx submissions (step 9).
- [ ] ARB_DRY_RUN=true and ARB_REQUIRE_CONFIRM=true remain in .env.

A tripped circuit breaker or a kill() latch requires operator diagnosis before
reset(). This is intentional: automated recovery from repeated reverts can amplify
losses. Review revert cause before calling reset().
