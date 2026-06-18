# Delegation Boundary

This skill (`solana-arb-executor`) owns the execution hot path. Program development, Anchor work, IDL handling, transaction signing internals, and keypair management are **delegated to `../solana-dev/`**. Do not duplicate that material here; route to it.

## This skill owns

- **Ingestion**: yellowstone gRPC streaming of accounts/slots at `processed` commitment, reconnect/backoff (`stream.rs`).
- **Detection**: decoding pool/account state and computing post-fee/post-slippage spread vs threshold (`detector.rs`, `opportunity-detection.md`).
- **Execution + landing**: Jito bundle construction, tip sizing, simulate-first, priority-fee math, retries (`jito.rs`, `fees.rs`, `jito-bundles.md`, `transaction-landing.md`).
- **Risk**: caps, circuit breaker, kill-switch, DRY_RUN/confirm-gate (`risk.rs`, `config.rs`, `risk-and-killswitch.md`, `safety-rails.md`).

## Delegated to ../solana-dev/

Route the following there; this skill assumes the caller already has them in hand:

- **Anchor programs / on-chain logic**: writing, building, and deploying programs.
- **IDL**: generating, parsing, and using an Anchor IDL to build instructions.
- **Instruction construction for custom programs**: the `solana-dev` skill is the source for how to assemble program-specific instructions; this skill consumes the resulting instructions inside a bundle.
- **Transaction signing internals**: how signing works, signer composition, durable nonces.
- **Keypair management**: generation, storage, loading, and secret hygiene. Keep secrets out of this repo and out of `config.rs`; obtain a ready signer via the delegated tooling.

## Why the split

Keeping signing and keypair custody in `solana-dev` enforces the safety boundary: this skill never embeds key material and never owns the signing primitive. It builds and simulates transactions, then hands off to the human confirm-gate for the live send (`safety-rails.md`). One source of truth for each concern, no drift between skills.

## When a task spans both

If a request needs a new on-chain program or custom instruction AND execution/landing, do the program/IDL/signing part via `../solana-dev/`, then return here for streaming, detection, bundling, landing, and risk. State the handoff explicitly rather than reimplementing the delegated half.
