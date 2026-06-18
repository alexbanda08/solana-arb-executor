# Risk and Kill-Switch

Maps to `skill/templates/src/risk.rs` (`RiskGate`). The risk module is std-only and
self-contained so it runs offline via `rustc --edition 2021 --test risk.rs`
(see README + SHARED rules). It carries NO external crate or cross-module imports.

This is the last line of defense between a bad signal and your funds. Every order
passes through `RiskGate.check_order` before it can reach the confirm-gate
(references/jito-bundles.md, references/safety-rails.md). No flag bypasses it.

## What it enforces
- max_notional_lamports: cap on the size of any SINGLE order. Blocks a fat-finger /
  bad-price order from sending huge size.
- max_position_lamports: cap on TOTAL net exposure after this order would fill.
  Caller passes `position_after_lamports`; the gate rejects if it exceeds the cap.
- max_daily_loss_lamports: cumulative realized loss for the session/day. Once
  breached, the gate refuses all further orders until reset (a daily stop-loss).
- max_consecutive_reverts: circuit breaker. N reverts in a row (no fill between)
  trips the breaker and blocks orders - reverts usually mean stale state, a broken
  assumption, or you are losing the inclusion race; stop digging.
- kill: manual hard stop. Once killed, every order is blocked until `reset`.

## State machine
- Healthy -> trades allowed if within caps.
- Breaker tripped (consecutive reverts >= max) -> all orders blocked.
- Killed (manual) -> all orders blocked.
- Daily-loss breached -> all orders blocked.
- `reset()` clears breaker/kill/counters for a new session (operator action, not
  automatic). A clean fill (`record_fill`) resets the consecutive-revert counter.

## API (risk.rs)
```rust
pub struct RiskLimits {
    pub max_notional_lamports: u64,
    pub max_position_lamports: u64,
    pub max_daily_loss_lamports: u64,
    pub max_consecutive_reverts: u32,
}

pub struct RiskGate {
    limits: RiskLimits,
    killed: bool,
    breaker_tripped: bool,        // sticky: stays tripped until reset()
    consecutive_reverts: u32,
    daily_loss_lamports: u64,
}

impl RiskGate {
    pub fn new(limits: RiskLimits) -> Self {
        Self {
            limits,
            killed: false,
            breaker_tripped: false,
            consecutive_reverts: 0,
            daily_loss_lamports: 0,
        }
    }

    /// Called BEFORE building/sending any order. Err(reason) blocks the order.
    pub fn check_order(
        &self,
        notional_lamports: u64,
        position_after_lamports: u64,
    ) -> Result<(), String> {
        if self.killed {
            return Err("kill-switch active".to_string());
        }
        if self.breaker_tripped {
            return Err("circuit breaker tripped (consecutive reverts)".to_string());
        }
        if self.daily_loss_lamports >= self.limits.max_daily_loss_lamports {
            return Err("daily loss limit reached".to_string());
        }
        if notional_lamports > self.limits.max_notional_lamports {
            return Err("order exceeds max notional".to_string());
        }
        if position_after_lamports > self.limits.max_position_lamports {
            return Err("order exceeds max position".to_string());
        }
        Ok(())
    }

    /// A bundle/tx landed. `realized_pnl_lamports` may be negative.
    /// A fill clears the revert STREAK but does NOT un-trip a tripped breaker.
    pub fn record_fill(&mut self, realized_pnl_lamports: i64) {
        self.consecutive_reverts = 0; // a fill breaks a revert streak
        if realized_pnl_lamports < 0 {
            let loss = realized_pnl_lamports.unsigned_abs();
            self.daily_loss_lamports = self.daily_loss_lamports.saturating_add(loss);
        }
    }

    /// A bundle/tx reverted, dropped, or failed to land. Trips the sticky breaker
    /// at the threshold; once tripped it stays tripped until reset().
    pub fn record_revert(&mut self) {
        self.consecutive_reverts = self.consecutive_reverts.saturating_add(1);
        if self.consecutive_reverts >= self.limits.max_consecutive_reverts {
            self.breaker_tripped = true;
        }
    }

    pub fn kill(&mut self) { self.killed = true; }
    pub fn is_killed(&self) -> bool { self.killed }
    pub fn is_breaker_tripped(&self) -> bool { self.breaker_tripped }

    /// Operator-initiated new session: clears breaker, kill, counters, daily loss.
    pub fn reset(&mut self) {
        self.killed = false;
        self.breaker_tripped = false;
        self.consecutive_reverts = 0;
        self.daily_loss_lamports = 0;
    }
}
```

## Wiring (main.rs)
1. Build `RiskLimits` from `Config` (env caps, references/safety-rails.md). All caps
   required; refuse to start if any is zero/unset for a live run.
2. For each detected opportunity: compute notional + resulting position, call
   `check_order`. On `Err`, log and DROP the order - never send.
3. On `Ok`: simulate (references/transaction-landing.md) -> confirm-gate
   (references/jito-bundles.md) -> (DRY_RUN logs only; live needs typed confirm).
4. After outcome: `record_fill(pnl)` on land, `record_revert()` on revert/drop.
5. Expose `kill()` to an operator signal (e.g. SIGINT handler / control input) so a
   human can hard-stop instantly.

## Dry-run interplay
- `DRY_RUN=true` (default) means no order is ever sent regardless of RiskGate. The
  RiskGate still runs so its decisions and counters are exercised and logged - you
  validate the risk logic before risking funds.
- Going live requires flipping DRY_RUN off AND passing the confirm-gate AND
  satisfying the RiskGate. All three independently. No single flag bypasses the set.

## Tuning notes
- Set max_consecutive_reverts low (e.g. 3-5). In a contested arb arena a revert
  streak almost always means you are late or your state is stale; the breaker saves
  fees and prevents bleeding.
- Daily-loss cap should be a small fraction of working capital; treat hitting it as
  "done for the day," not "raise the cap."
- These are guards, not a strategy. They cap downside; they do not create edge. The
  durable edge is correct landing + discipline + less-contested opportunities
  (see README economics note), not loosened limits.

## Checklist
- [ ] All four caps set from Config; refuse live start if any missing.
- [ ] check_order called before EVERY order; Err -> drop, never send.
- [ ] record_fill / record_revert called on every outcome.
- [ ] Breaker trips at max_consecutive_reverts; fill resets the streak.
- [ ] kill() reachable from an operator signal; reset() is manual only.
- [ ] DRY_RUN still runs the gate; no flag bypasses the gate.
