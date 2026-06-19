# SDK Versions

Pinned crate and toolchain matrix for `solana-arb-executor`. Last verified **2026-06**. Versions move fast in this ecosystem; run the verification commands below to confirm the latest before relying on these.

## Toolchain

- Rust toolchain **1.96**, edition **2021**.

## Rust crates (crates.io)

| Crate | Verified version | Cargo.toml caret |
|---|---|---|
| solana-sdk | 4.0.1 | `"4.0"` |
| solana-client | 4.0.0 | `"4.0"` |
| yellowstone-grpc-client | 13.1.1 | `"13.1"` |
| yellowstone-grpc-proto | 12.5.0 | `"12.5"` |
| jito-sdk-rust | 0.3.2 | `"0.3"` |
| anchor-lang | 1.0.2 | `"1.0"` |
| litesvm | 0.13.0 | `"0.13"` |
| carbon-core | 1.0.0 | `"1.0"` |
| base64 | 0.22.1 | `"0.22"` |
| cargo-audit | 0.22.2 | (dev tool) |

Re-verified **2026-06** against crates.io: solana-sdk 4.0.1, solana-client 4.0.0
(4.1.0-rc.1 is a prerelease -- do NOT pin it), yellowstone-grpc-client 13.1.1,
yellowstone-grpc-proto 12.5.0, jito-sdk-rust 0.3.2, litesvm 0.13.0, base64 0.22.1
are all still current stable releases.

The `Cargo.toml` in `templates/` also pins `tokio = { version = "1", features = ["full"] }`, `anyhow = "1"`, `serde = { version = "1", features = ["derive"] }`, `bincode = "1"`, `bs58 = "0.5"`, `base64 = "0.22"`, and `serde_json = "1"`.

`bincode` is pinned to `"1"` deliberately: jito-sdk-rust 0.3 depends on `bincode ^1.3`, and the `bincode::serialize(&tx)` call in `jito.rs` is the 1.x API. Do NOT bump to bincode 2/3 (their API differs and the SDK does not use them).

## Maturity flags (honesty)

- **jito-sdk-rust 0.3.2 is early 0.x.** Pre-1.0 means the API can break between minor releases and behavior may shift; pin exactly, read the changelog before bumping, and test bundle build/simulate against a known-good path after any upgrade. Do not assume API stability.
- **yellowstone-grpc-client / -proto** must move together: the client (13.1.1) and proto (12.5.0) versions are independent crates and a mismatch breaks decoding. Bump as a pair and re-verify against your gRPC provider's supported proto.
- **litesvm 0.13.0** is the offline test harness (still 0.x); use it for fast in-process tx tests where a validator is overkill.
- **solana-sdk 4 moved keypair construction to the `solana-keypair` crate and REMOVED `Keypair::from_bytes`.** Construct a keypair with the std `TryFrom` impl instead: `Keypair::try_from(&bytes[..])` (used in `main.rs::load_keypair`). The old `from_bytes` no longer exists and will not compile against 4.0.1.

## Offline-testability note

The pure-math modules `src/fees.rs` and `src/risk.rs` are std-only and self-contained (no external crates, no cross-module `use`), so they compile and run standalone without network access:

```
rustc --edition 2021 --test src/fees.rs && ./fees
rustc --edition 2021 --test src/risk.rs && ./risk
```

The rest of the crate (`main.rs`, `stream.rs`, `jito.rs`, `detector.rs`, `config.rs`) targets the pinned crates above and requires `cargo build`, which fetches dependencies and needs network access. It is not expected to compile in an offline sandbox.

## Verification commands

Confirm the current latest before pinning or upgrading:

```
# Rust
cargo search solana-sdk
cargo add solana-sdk --dry-run
rustup show               # confirm toolchain 1.96
cargo audit               # after `cargo install cargo-audit`

# Python (if using the analysis/delegation side)
pip index versions solders
pip index versions anchorpy
```

Reference Python pins (PyPI, verified 2026-06), for the delegated tooling rather than this crate: solders 0.27.1, anchorpy 0.21.0, driftpy 0.8.89, solana 0.36.12.
