---
globs:
  - "**/*.rs"
---

# Rule: Rust Style and Hot-Path Standards

These standards apply to all Rust files in this repository. They are
enforced in code review and exist to keep the execution hot path correct,
observable, and maintainable.

## Edition and toolchain

- Use Rust edition 2021 in Cargo.toml (`edition = "2021"`).
- Target the toolchain pinned via `rust-version = "1.96"` in Cargo.toml (stable);
  add a `rust-toolchain.toml` if you want to hard-pin the toolchain locally.
- Do not use nightly-only features; the crate must build on stable.

## Error handling - no panics in the hot path

- `unwrap()` and `expect()` are forbidden in any function reachable from
  the main execution loop (`stream -> detector -> risk -> jito`).
- Use `?` with `anyhow::Result` for fallible operations in async code.
- Use `Result<T, String>` in std-only modules (fees.rs, risk.rs) so they
  remain self-contained and compile without external crates.
- Reserve `unwrap()` only for initialization code that runs once at startup
  (e.g., parsing a static constant from a known-good literal).

```rust
// CORRECT - hot path
let price = pool.price().context("pool price unavailable")?;

// WRONG - hot path
let price = pool.price().unwrap();
```

## Zero-allocation inner loop

- Do not allocate (`Vec::new()`, `String::new()`, `Box::new()`, format!
  with heap strings) inside the per-slot / per-account update inner loop.
- Pre-allocate buffers at startup and reuse them.
- Prefer stack-allocated arrays or fixed-capacity `arrayvec` types for
  per-iteration data.
- Exception: the one-time bundle assembly path (outside the tight detect
  loop) may allocate normally.

## Commitment level

- Use `CommitmentConfig::processed()` for account subscriptions and
  opportunity detection (lowest latency).
- Use `CommitmentConfig::confirmed()` or `finalized()` only for
  post-submission confirmation checks, never for the ingestion hot path.
- Document the commitment level in a comment at each RPC/gRPC call site.

## Latency-sensitive section annotation

Mark every function that lies on the latency-critical path with a doc
comment that includes a `# Latency` section stating the budget and any
known sources of jitter:

```rust
/// Detect spread opportunity from a pool state update.
///
/// # Latency
/// Budget: <500 us from account update to opportunity struct ready.
/// Allocations: none. Async: none (pure sync computation).
pub fn detect(state: &PoolState, threshold: u64) -> Option<Opportunity> {
    // ...
}
```

## Async runtime

- Use `tokio` with the `full` feature set (already in Cargo.toml).
- Spawn blocking CPU work with `tokio::task::spawn_blocking`; do not
  block the async executor with sync I/O or heavy computation.
- Use `tokio::select!` for concurrent branch cancellation in the stream
  consumer; always include a shutdown/kill-switch arm.

## Logging

- Use the `tracing` crate for structured logging (add as a dependency if
  not already present; do not use `println!` in production paths).
- Hot-path log calls must be at `trace!` level to allow zero-cost
  disabling in release mode via `RUST_LOG`.
- Error and warning conditions (revert, cap breach, kill) must use
  `error!` or `warn!` with structured fields (`opportunity_id`, `reason`,
  `lamports`).

## Module separation for offline testability

- `fees.rs` and `risk.rs` MUST remain std-only and self-contained.
  No `use crate::...` cross-module references, no external crate imports.
  This allows `rustc --edition 2021 --test <file>.rs` to run without a
  network or cargo registry access.
- All other modules (`main.rs`, `stream.rs`, `jito.rs`, `detector.rs`,
  `config.rs`) may use the pinned crates freely.

## Tests

- Every std-only module (fees.rs, risk.rs) MUST include an inline
  `#[cfg(test)] mod tests` block with assertions that cover:
  - normal/expected inputs
  - boundary conditions (zero, u64::MAX / cap values)
  - expected error paths (Result::Err cases)
- Integration tests that require network or the full crate build live in
  `tests/` and are not expected to pass offline.

## Security

- Never log private keys, seed phrases, or wallet keypair bytes at any
  log level.
- Do not embed keypair bytes or secrets in source files; load from
  environment variables or a keypair file path set by the operator.
- Follow the `no-auto-execute.md` rule for all fund-moving operations;
  this style rule does not supersede it.
