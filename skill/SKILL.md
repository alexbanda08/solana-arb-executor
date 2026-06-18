---
name: solana-arb-executor
description: Rust low-latency Solana arbitrage execution -- Yellowstone gRPC / ShredStream ingestion, opportunity/spread detection, simulate-first Jito bundles with tip and leader timing, transaction landing and priority-fee sizing, exposure caps + circuit breaker + kill-switch. Scaffolds the on-chain hot path (detect -> simulate -> bundle -> land) that analysis-only and TypeScript arb skills leave out.
user-invocable: true
---

# solana-arb-executor

Scaffolds the Rust **execution hot path** for Solana arbitrage: `stream -> detect -> simulate -> bundle -> land`, wrapped in a risk gate (caps, circuit breaker, kill-switch) and a confirm-gate. This is the on-chain layer that prior art only *analyzes*: agiprolabs/claude-trading-skills is Python *analysis* (signals, backtests, no execution), and TypeScript arb skills punt the latency-sensitive landing path. This is the genuine white space -- the part that has to be fast, correct, and safe.

**Honest economics (load-bearing):** spatial DEX arb on Solana is infrastructure-dominated and heavily contested; raw price gaps go to parties with better network position than a single box. A retail operator's durable edge is **not** raw speed -- it is **correct landing** (simulate-first, right CU price, tip sizing, retry discipline), **risk discipline** (caps/breaker/kill-switch), and **less-contested opportunities** (funding-basis, cross-venue rate gaps, longer-horizon plays). This is scaffolding + knowledge, **not** a hosted bot and **not** a guaranteed profit machine.

## PRECEDENCE (read before any code-gen)

1. **Read `references/safety-rails.md` FIRST.** Every emitted execution path obeys it.
2. **Never** emit code that sends a fund-moving / signing transaction without an explicit **typed human confirm-gate**; no flag defeats it. Default `DRY_RUN=true`, `REQUIRE_CONFIRM=true`. **Always** simulate before submit.
3. Use the **pinned crate versions** in `references/sdk-versions.md` (verified 2026-06). Flag `jito-sdk-rust` as early 0.x.
4. **Delegate** program / Anchor / IDL / transaction-signing work to `../solana-dev/` via `references/delegation.md`. Do not duplicate it here.
5. Mandatory caps (max-notional, max-position, daily-loss) + circuit breaker + kill-switch gate every order before it can reach the confirm-gate.

## Task Routing Guide

Map the user's intent to **exactly one** leaf.

| User asks about... | Open this leaf |
| --- | --- |
| hot-path design / latency budget | `references/architecture.md` |
| stream accounts/slots / ShredStream ingestion | `references/streaming-ingestion.md` |
| Jito bundle / tip / leader timing | `references/jito-bundles.md` |
| land tx / priority fee / simulate / retries | `references/transaction-landing.md` |
| detect opportunity / spread / threshold | `references/opportunity-detection.md` |
| risk / circuit breaker / kill-switch / caps | `references/risk-and-killswitch.md` |
| safety / confirm-gate | `references/safety-rails.md` |
| crate / version / install | `references/sdk-versions.md` |
| program / Anchor / signing | `references/delegation.md` |

## Progressive disclosure

- **Router (this file):** pointers only -- no bulk content.
- **Leaves (~60-200 lines each):** dense, correct, minimal. Code lives in `skill/templates/`.
- **Pure logic is offline-testable:** `templates/src/fees.rs` and `templates/src/risk.rs` are std-only and self-contained -- run `rustc --edition 2021 --test fees.rs` (and `risk.rs`) standalone. The full crate (`stream.rs`, `jito.rs`, `detector.rs`, `main.rs`) compiles with `cargo build` against the pinned crates.
- **Delegation:** `../solana-dev/` for program / tx / signing.

## Agents

| Agent | Role | Model |
| --- | --- | --- |
| `agents/arb-execution-architect.md` | Designs the latency budget, ingestion/landing topology, and risk envelope before code | opus |
| `agents/rust-perf-engineer.md` | Wires stream -> detector -> RiskGate -> simulate-first Jito bundle | sonnet |

## Commands

| Command | Does |
| --- | --- |
| `commands/scaffold-executor.md` | Emit the executor crate skeleton (DRY_RUN + REQUIRE_CONFIRM defaults) |
| `commands/wire-yellowstone.md` | Generate the Yellowstone gRPC ingestion module with reconnect/backoff |
| `commands/add-killswitch.md` | Drop in the RiskGate with caps + circuit breaker + kill-switch |
