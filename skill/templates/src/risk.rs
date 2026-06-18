//! Risk gate, circuit breaker, and kill-switch for the arbitrage hot path.
//!
//! STD-ONLY and SELF-CONTAINED on purpose: no external crates, no cross-module
//! `use`. This lets the module compile and run its tests offline with:
//!
//!     rustc --edition 2021 --test src/risk.rs && ./risk
//!
//! SAFETY CANON: every candidate order MUST pass `RiskGate::check_order` before
//! it can be simulated and (behind a separate human confirm-gate) submitted.
//! The gate enforces hard caps (max notional per order, max open position,
//! max daily realized loss), a consecutive-revert circuit breaker, and a manual
//! kill-switch. Nothing here moves funds or signs; it only says yes or no, and
//! a "no" is final until explicitly reset. No flag bypasses these checks.

/// Hard caps for the risk gate. All values are in lamports / counts so the gate
/// has no floating-point ambiguity. Construct once from validated config.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RiskLimits {
    /// Max notional (size) of a single order, in lamports.
    pub max_notional_lamports: u64,
    /// Max absolute open position allowed after a fill, in lamports.
    pub max_position_lamports: u64,
    /// Max cumulative realized loss allowed within the current day, in lamports.
    pub max_daily_loss_lamports: u64,
    /// Number of consecutive reverts that trips the circuit breaker.
    pub max_consecutive_reverts: u32,
}

/// Reason an order was blocked. Returned as the `Err` payload string so the
/// caller can log a precise, non-ambiguous cause without parsing prose.
fn over_notional(notional: u64, cap: u64) -> String {
    format!("blocked: notional {notional} exceeds max_notional {cap}")
}
fn over_position(position: u64, cap: u64) -> String {
    format!("blocked: position_after {position} exceeds max_position {cap}")
}

/// Stateful risk gate: holds immutable limits plus mutable runtime state
/// (kill-switch, circuit breaker, consecutive reverts, daily realized loss).
#[derive(Debug, Clone)]
pub struct RiskGate {
    limits: RiskLimits,
    killed: bool,
    breaker_tripped: bool,
    consecutive_reverts: u32,
    daily_loss_lamports: u64,
}

impl RiskGate {
    /// Create a gate from the given limits in a clean (armed) state.
    pub fn new(limits: RiskLimits) -> Self {
        RiskGate {
            limits,
            killed: false,
            breaker_tripped: false,
            consecutive_reverts: 0,
            daily_loss_lamports: 0,
        }
    }

    /// Decide whether an order may proceed. Returns `Ok(())` only if the gate is
    /// armed (not killed, breaker not tripped, daily-loss budget intact) AND the
    /// order is within the per-order notional and resulting-position caps.
    ///
    /// `notional_lamports` is the size of this order; `position_after_lamports`
    /// is the absolute open position that would result if it fills. This is a
    /// pure check: it mutates nothing. The caller records the outcome after the
    /// fact via `record_fill` / `record_revert`.
    pub fn check_order(
        &self,
        notional_lamports: u64,
        position_after_lamports: u64,
    ) -> Result<(), String> {
        if self.killed {
            return Err("blocked: kill-switch engaged".to_string());
        }
        if self.breaker_tripped {
            return Err(format!(
                "blocked: circuit breaker tripped after {} consecutive reverts",
                self.consecutive_reverts
            ));
        }
        if self.daily_loss_lamports >= self.limits.max_daily_loss_lamports {
            return Err(format!(
                "blocked: daily loss {} reached max_daily_loss {}",
                self.daily_loss_lamports, self.limits.max_daily_loss_lamports
            ));
        }
        if notional_lamports > self.limits.max_notional_lamports {
            return Err(over_notional(notional_lamports, self.limits.max_notional_lamports));
        }
        if position_after_lamports > self.limits.max_position_lamports {
            return Err(over_position(position_after_lamports, self.limits.max_position_lamports));
        }
        Ok(())
    }

    /// Record a successful fill. Realized PnL is in lamports; a negative value
    /// (a loss) adds to the daily-loss tally, which can later trip the daily cap.
    /// A profitable fill clears the consecutive-revert streak.
    pub fn record_fill(&mut self, realized_pnl_lamports: i64) {
        self.consecutive_reverts = 0;
        if realized_pnl_lamports < 0 {
            let loss = realized_pnl_lamports.unsigned_abs();
            self.daily_loss_lamports = self.daily_loss_lamports.saturating_add(loss);
        }
    }

    /// Record a reverted / failed transaction. Increments the consecutive-revert
    /// streak and trips the circuit breaker when it reaches the configured
    /// threshold. A tripped breaker blocks all further orders until `reset`.
    pub fn record_revert(&mut self) {
        self.consecutive_reverts = self.consecutive_reverts.saturating_add(1);
        if self.consecutive_reverts >= self.limits.max_consecutive_reverts {
            self.breaker_tripped = true;
        }
    }

    /// Engage the kill-switch. Once killed, every `check_order` fails until
    /// `reset`. This is the human emergency stop and is intentionally sticky.
    pub fn kill(&mut self) {
        self.killed = true;
    }

    /// Whether the kill-switch is currently engaged.
    pub fn is_killed(&self) -> bool {
        self.killed
    }

    /// Whether the consecutive-revert circuit breaker is currently tripped.
    pub fn is_breaker_tripped(&self) -> bool {
        self.breaker_tripped
    }

    /// Current consecutive-revert streak.
    pub fn consecutive_reverts(&self) -> u32 {
        self.consecutive_reverts
    }

    /// Current accumulated daily realized loss in lamports.
    pub fn daily_loss_lamports(&self) -> u64 {
        self.daily_loss_lamports
    }

    /// Clear runtime state back to armed: releases the kill-switch, resets the
    /// circuit breaker and revert streak, and zeroes the daily-loss tally. Use
    /// for a deliberate restart or a new trading day. Limits are unchanged.
    pub fn reset(&mut self) {
        self.killed = false;
        self.breaker_tripped = false;
        self.consecutive_reverts = 0;
        self.daily_loss_lamports = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits() -> RiskLimits {
        RiskLimits {
            max_notional_lamports: 1_000_000_000, // 1 SOL per order
            max_position_lamports: 5_000_000_000, // 5 SOL open
            max_daily_loss_lamports: 200_000_000, // 0.2 SOL/day
            max_consecutive_reverts: 3,
        }
    }

    #[test]
    fn allows_order_within_caps() {
        let gate = RiskGate::new(limits());
        assert!(gate.check_order(500_000_000, 2_000_000_000).is_ok());
        // Exactly at the caps is allowed (inclusive).
        assert!(gate.check_order(1_000_000_000, 5_000_000_000).is_ok());
    }

    #[test]
    fn blocks_over_notional() {
        let gate = RiskGate::new(limits());
        let err = gate.check_order(1_000_000_001, 0).unwrap_err();
        assert!(err.contains("notional"));
    }

    #[test]
    fn blocks_over_position() {
        let gate = RiskGate::new(limits());
        let err = gate.check_order(100_000_000, 5_000_000_001).unwrap_err();
        assert!(err.contains("position"));
    }

    #[test]
    fn kill_blocks_all_orders() {
        let mut gate = RiskGate::new(limits());
        assert!(!gate.is_killed());
        gate.kill();
        assert!(gate.is_killed());
        // Even a trivially-small, in-cap order is blocked once killed.
        let err = gate.check_order(1, 1).unwrap_err();
        assert!(err.contains("kill-switch"));
    }

    #[test]
    fn breaker_trips_at_threshold() {
        let mut gate = RiskGate::new(limits());
        // First two reverts: still armed.
        gate.record_revert();
        assert!(!gate.is_breaker_tripped());
        gate.record_revert();
        assert!(!gate.is_breaker_tripped());
        assert!(gate.check_order(1, 1).is_ok());
        // Third revert hits the threshold (3) -> breaker trips.
        gate.record_revert();
        assert!(gate.is_breaker_tripped());
        let err = gate.check_order(1, 1).unwrap_err();
        assert!(err.contains("circuit breaker"));
    }

    #[test]
    fn fill_clears_revert_streak() {
        let mut gate = RiskGate::new(limits());
        gate.record_revert();
        gate.record_revert();
        assert_eq!(gate.consecutive_reverts(), 2);
        // A successful (profitable) fill resets the streak so the breaker won't
        // trip on the next isolated revert.
        gate.record_fill(50_000);
        assert_eq!(gate.consecutive_reverts(), 0);
        gate.record_revert();
        assert!(!gate.is_breaker_tripped());
    }

    #[test]
    fn daily_loss_accumulates_and_blocks() {
        let mut gate = RiskGate::new(limits());
        // Two losing fills totaling exactly the daily cap.
        gate.record_fill(-150_000_000);
        gate.record_fill(-50_000_000);
        assert_eq!(gate.daily_loss_lamports(), 200_000_000);
        // At/over the cap, further orders are blocked.
        let err = gate.check_order(1, 1).unwrap_err();
        assert!(err.contains("daily loss"));
    }

    #[test]
    fn profitable_fill_does_not_add_loss() {
        let mut gate = RiskGate::new(limits());
        gate.record_fill(123_456);
        assert_eq!(gate.daily_loss_lamports(), 0);
    }

    #[test]
    fn reset_rearms_gate() {
        let mut gate = RiskGate::new(limits());
        gate.kill();
        gate.record_revert();
        gate.record_revert();
        gate.record_revert();
        gate.record_fill(-200_000_000);
        assert!(gate.is_killed());
        assert!(gate.is_breaker_tripped());
        gate.reset();
        assert!(!gate.is_killed());
        assert!(!gate.is_breaker_tripped());
        assert_eq!(gate.consecutive_reverts(), 0);
        assert_eq!(gate.daily_loss_lamports(), 0);
        // Fully re-armed: an in-cap order passes again.
        assert!(gate.check_order(500_000_000, 1_000_000_000).is_ok());
    }

    #[test]
    fn daily_loss_saturates() {
        let mut gate = RiskGate::new(RiskLimits {
            max_daily_loss_lamports: u64::MAX,
            ..limits()
        });
        gate.record_fill(i64::MIN); // largest representable single loss
        gate.record_fill(i64::MIN);
        // No wraparound; tally saturates at u64::MAX.
        assert_eq!(gate.daily_loss_lamports(), u64::MAX);
    }
}
