# Safety Rails

This is the load-bearing safety canon for the skill. It is enforced in code (`risk.rs`, `jito.rs`, `config.rs`), restated as rules (`rules/no-auto-execute.md`), and summarized here. No flag, env var, or config value may bypass it.

## The non-negotiables

1. **Never auto-send a fund-moving or signing transaction.** A live send requires an explicit, typed human confirmation at the confirm-gate. There is no flag that turns this off. An agent or script invoking this code path cannot self-approve.
2. **Always simulate before submit.** Every bundle is simulated first (`jito.rs`). If simulation fails or shows an unexpected result, the transaction is dropped, never sent.
3. **Caps are mandatory.** `RiskLimits` enforces `max_notional_lamports`, `max_position_lamports`, and `max_daily_loss_lamports`. An order over any cap is rejected by `RiskGate.check_order` before it can be built.
4. **Circuit breaker.** Consecutive reverts past `max_consecutive_reverts` trip the breaker; the gate then rejects all orders until a deliberate `reset()`. This stops a stale-read storm or a misconfigured route from bleeding fees.
5. **Kill-switch.** `RiskGate.kill()` halts all order acceptance immediately and stays killed until `reset()`. Wire it to a signal/operator command for instant shutdown.
6. **Safe defaults.** `config.rs` ships `DRY_RUN=true` and `REQUIRE_CONFIRM=true`. In DRY_RUN the pipeline logs the intended order and stops; it builds nothing live. You must consciously change the environment to go live, and even then the confirm-gate still fires.

## Scaffolding, not a bot

This skill is execution scaffolding plus knowledge. It is NOT a hosted service, NOT an auto-running bot, and NOT something you point at a funded keypair and walk away from. It deliberately requires a human in the loop on every live send. Anyone packaging it as a turnkey autonomous trader is removing the safety rails this skill exists to provide.

## Keypair and signing boundary

This skill does not own keypair management or transaction signing internals. Those belong to `../solana-dev/` (see `delegation.md`). Keep secrets out of config and out of the repo; load them through the delegated tooling, never hardcoded.

## Honest P&L reality

Retail spatial arbitrage on Solana is infrastructure-dominated and crowded. After pool fees, slippage, base + priority fees, and the Jito tip, most spreads that look profitable on a `processed` read are gone by land time or were never net-positive. Expect a high miss/revert rate. The realistic edge for an operator without co-located infra is correct landing, strict risk discipline, and steering toward less-contested or funding-basis opportunities, not winning a raw latency race. Treat any run as capital-at-risk experimentation, start in DRY_RUN, and size with the caps set conservatively. There is no guaranteed profit here.

## Routing

- Code-level caps/breaker/kill-switch -> `risk-and-killswitch.md`
- Simulate/priority fee/retries -> `transaction-landing.md`
- Pinned crate versions -> `sdk-versions.md`
