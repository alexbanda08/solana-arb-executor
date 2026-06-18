# solana-arb-executor

**Scaffold the Rust low-latency execution hot path for Solana arbitrage -- the on-chain `detect -> simulate -> bundle -> land` loop that analysis-only and TypeScript arb skills leave out.**

A Claude Code / Codex skill that generates the *execution* layer: Yellowstone gRPC / ShredStream ingestion, spread/threshold detection, simulate-first Jito bundles with tip and leader timing, transaction landing with priority-fee sizing, and a real risk gate (exposure caps, circuit breaker, kill-switch) behind a typed human confirm-gate.

---

## The problem

Solana arbitrage skills today stop before the hard part. The Python prior art *analyzes* -- it finds signals, backtests spreads, and prints a chart. The TypeScript arb skills quote venues and hand you a transaction, then punt the latency-sensitive landing path back to you. Nobody scaffolds the layer that actually has to run fast on-chain: ingest realtime account state, decide in microseconds, simulate, build a Jito bundle, size the tip, and land before the opportunity is consumed -- without blowing the account when a stale read or a bad streak hits.

There is no Rust low-latency execution skill. That is the white space this fills.

## What it does

Routes an execution-infra request to exactly one focused leaf and emits real, importable Rust against pinned 2026 crates:

- **Ingestion** -- `yellowstone-grpc-client` account/slot subscription at `processed` commitment, with reconnect and backoff.
- **Detection** -- post-fee, post-slippage spread vs threshold over streamed pool state.
- **Execution** -- simulate-first Jito bundles (<=5 tx, tip in the LAST tx, tip accounts fetched at runtime), priority-fee / CU-limit sizing, retry discipline.
- **Risk** -- a `RiskGate` with max-notional / max-position / daily-loss caps, a consecutive-revert circuit breaker, and a kill-switch -- checked on every order before it can reach the confirm-gate.
- **Plumbing it does NOT reinvent** -- Anchor, IDLs, program calls, and transaction signing are delegated to the sibling `solana-dev` skill.

## Prior art and how this differs

The closest prior art is **agiprolabs/claude-trading-skills**: a Python *analysis* toolkit -- signal generation, backtests, spread inspection. It tells you *whether* an edge might exist; it does not execute. The TypeScript arb skills stop at "here is a swap."

This skill *is* the execution layer those omit -- the Rust hot path that ingests, decides, simulates, bundles, and lands, with the risk and safety plumbing wired in. Analysis answers *is there an edge*; this answers *how do I act on it without losing the account*. It complements the TS arb/quant skills rather than duplicating them, and delegates program/Anchor work to `solana-dev`.

## Honest economics

Retail spatial DEX arb is **infrastructure-dominated**. Raw price gaps are taken by parties with co-located bare-metal, private leader paths, and sub-10ms bundle budgets that a single-box bot will not match; Jito tips eat a large share of the gross edge on contested atomic ops. Shaving microseconds off decode changes nothing when network RTT dominates.

Where retail edge actually survives: **correct landing** (simulate-first, right CU price, tip sizing, retry discipline so the txns you send actually land instead of burning fees on reverts), **risk discipline** (caps, breaker, kill-switch), and **less-contested opportunities** (funding-basis, cross-venue rate gaps, longer-horizon plays where latency is not the deciding factor). That is what this skill leads with. It is scaffolding + knowledge, **not** a hosted bot and **not** a guaranteed profit machine.

## How it works

1. Activation loads `skill/SKILL.md` -- a **router**, not a wall of content.
2. The router reads `references/safety-rails.md` **first**, then maps your intent to **one** leaf via the Task Routing Guide.
3. The agent opens that single leaf and emits minimal-but-correct Rust from `skill/templates/`.
4. Program / tx / signing concerns delegate to `../solana-dev/`.

Progressive disclosure keeps context lean: pointers in the router, density in the leaves. The pure math/logic modules (`fees.rs`, `risk.rs`) are std-only and self-contained, so they compile and run standalone -- `rustc --edition 2021 --test fees.rs`. The full crate compiles with `cargo build` against the pinned crates (needs network to fetch them).

## Install

```bash
./install.sh        # copies the skill + sibling solana-dev into your skills dir
# or, in Claude Code:
/plugin install solana-arb-executor
```

Requires the sibling **solana-dev** skill (Anchor / IDL / tx-signing plumbing). `install.sh` places it alongside this one.

## Use cases

Prompt the agent naturally; the router does the rest:

- *"Scaffold a Solana arb executor that streams these pool accounts over Yellowstone gRPC and detects spreads above a threshold, dry-run by default."*
- *"Build a simulate-first Jito bundle with the tip in the last tx and a confirm-gate -- never auto-send."*
- *"Add a risk gate: max-notional, max-position, daily-loss caps, a circuit breaker on consecutive reverts, and a kill-switch."*

## Safety

Load-bearing, enforced in every execution template and in `rules/no-auto-execute.md`:

- **Never** auto-execute a fund-moving or signing tx without an explicit **typed human confirmation**; no flag defeats the confirm-gate.
- **Always** simulate before submit.
- Mandatory **max-notional + max-position + daily-loss** caps, a **circuit breaker** (trips on consecutive reverts), and a **kill-switch**.
- Configs default to `DRY_RUN=true` and `REQUIRE_CONFIRM=true`.
- This is scaffolding + knowledge, **never** a hosted or auto-running bot.

## Stack (2026, last-verified 2026-06)

`solana-sdk 4.0.1` ("4.0") - `solana-client 4.0.0` ("4.0") - `yellowstone-grpc-client 13.1.1` ("13.1") - `yellowstone-grpc-proto 12.5.0` ("12.5") - `jito-sdk-rust 0.3.2` ("0.3", **early 0.x -- API may shift; pin and review**) - `tokio 1` (full) - `anyhow 1` - `serde 1` (derive). Rust toolchain 1.96, edition 2021. Run `cargo search <crate>` or `cargo add <crate>` to confirm latest. See `skill/references/sdk-versions.md`.

## License

MIT (c) 2026 Alexandre Bandarra.
