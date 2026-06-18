---
description: Scaffold the solana-arb-executor Rust crate from templates, set environment defaults, and verify offline-testable modules compile standalone.
---

# /scaffold-executor

Scaffold the solana-arb-executor Rust crate from the skill templates, apply safe
defaults (DRY_RUN=true, REQUIRE_CONFIRM=true), and verify the two std-only modules
compile offline before attempting a full network build.

**Before starting**: read solana-arb-executor / references/safety-rails.md in full.
The confirm-gate and DRY_RUN flag are load-bearing; do not modify the defaults.

---

## Steps

### 1. Read safety rails

Open and read `references/safety-rails.md` from the solana-arb-executor skill.
Confirm you understand: DRY_RUN=true default, REQUIRE_CONFIRM=true default, and the
rule that no flag may bypass the confirm-gate on a live signing transaction.

### 2. Verify Rust toolchain

```sh
rustc --version
cargo --version
```

Expected: Rust 1.96 or later. If older, update:

```sh
rustup update stable
rustup default stable
```

### 3. Create the crate

```sh
cargo new solana-arb-executor --name solana-arb-executor
cd solana-arb-executor
```

### 4. Replace Cargo.toml

Overwrite the generated Cargo.toml with the pinned dependency set. All versions are
verified as of 2026-06 (run `cargo search <crate>` to confirm latest before pinning
in production):

```toml
[package]
name = "solana-arb-executor"
version = "0.1.0"
edition = "2021"

[dependencies]
solana-sdk    = "4.0"
solana-client = "4.0"
yellowstone-grpc-client = "13.1"
yellowstone-grpc-proto  = "12.5"
jito-sdk-rust = "0.3"
tokio  = { version = "1", features = ["full"] }
anyhow = "1"
serde  = { version = "1", features = ["derive"] }

[profile.release]
opt-level = 3
lto = "thin"
codegen-units = 1
```

Note: jito-sdk-rust is a 0.x crate (early stage). API surface may shift between
patch releases. Pin to "0.3" and review the changelog before upgrading.

### 5. Copy source templates

Copy every file from the skill templates directory into src/:

```sh
# From the skill root:
cp templates/src/fees.rs    solana-arb-executor/src/fees.rs
cp templates/src/risk.rs    solana-arb-executor/src/risk.rs
cp templates/src/config.rs  solana-arb-executor/src/config.rs
cp templates/src/stream.rs  solana-arb-executor/src/stream.rs
cp templates/src/detector.rs solana-arb-executor/src/detector.rs
cp templates/src/jito.rs    solana-arb-executor/src/jito.rs
cp templates/src/main.rs    solana-arb-executor/src/main.rs
```

### 6. Verify offline-testable modules compile without network

fees.rs and risk.rs are std-only with no external crate dependencies. Compile and
run their inline tests immediately, before any cargo fetch:

```sh
cd solana-arb-executor
rustc --edition 2021 --test src/fees.rs -o fees_test && ./fees_test
rustc --edition 2021 --test src/risk.rs -o risk_test && ./risk_test
```

Both must exit 0 with "test result: ok". If either fails, fix the module before
proceeding. Do not continue to step 7 with a failing test.

### 7. Create the .env file with safe defaults

All env vars are read by `Config::from_env()` (src/config.rs) and `main.rs`; they
use the `ARB_` prefix and SOL-denominated caps. Match these names exactly or the
binary ignores them and falls back to defaults.

```sh
cat > .env << 'EOF'
# solana-arb-executor environment configuration
# SAFETY: ARB_DRY_RUN and ARB_REQUIRE_CONFIRM default to true in Config::from_env().
# Overriding to false requires deliberate action and typed confirmation at runtime.

ARB_DRY_RUN=true
ARB_REQUIRE_CONFIRM=true

# Risk caps (SOL-denominated; conservative defaults; tune per your risk tolerance)
ARB_MAX_NOTIONAL_SOL=0.5
ARB_MAX_POSITION_SOL=2.0
ARB_MAX_DAILY_LOSS_SOL=0.1
ARB_MAX_CONSECUTIVE_REVERTS=5

# Required endpoints (binary exits at startup if any is unset)
ARB_RPC_URL=https://api.mainnet-beta.solana.com
ARB_GRPC_URL=
ARB_GRPC_TOKEN=
ARB_JITO_URL=

# Keypair path (read at call time, never committed; required for live submission)
ARB_KEYPAIR_PATH=~/.config/solana/id.json

# Pool accounts to watch (base58; required by main.rs)
ARB_POOL_A=
ARB_POOL_B=

# Spread detection threshold (bps)
ARB_SPREAD_THRESHOLD_BPS=30
EOF
```

Add .env to .gitignore immediately:

```sh
echo ".env" >> .gitignore
echo "*.json" >> .gitignore
```

### 8. Add .env to .gitignore and verify

```sh
git init
git add .gitignore Cargo.toml src/
git status
```

Confirm that .env does NOT appear in the staged files. If it does, run:

```sh
git rm --cached .env
```

### 9. Build the full crate (requires network to fetch pinned crates)

This step fetches crates from crates.io and will fail without a network connection.
Only run when online:

```sh
cargo build 2>&1 | head -80
```

Expected: crates download and compile. Errors in stream.rs / jito.rs / detector.rs
may require endpoint configuration; fees.rs and risk.rs errors indicate a logic
regression (they passed step 6, so a regression is a file-copy issue).

### 10. Run the full test suite

```sh
cargo test 2>&1
```

All unit tests in fees.rs and risk.rs must pass. Integration tests in other modules
may require mock endpoints; consult references/streaming-ingestion.md for test
harness options (litesvm 0.12 for on-chain simulation).

### 11. Confirm safe defaults before any live run

Before pointing at mainnet:
- ARB_GRPC_URL and ARB_JITO_URL must be set to real endpoints (ARB_GRPC_TOKEN too
  if your provider requires auth).
- ARB_DRY_RUN=true for the first N runs (recommended: at least 50 detect cycles).
- ARB_REQUIRE_CONFIRM=true always; type "YES" (exact, uppercase) at each
  confirmation prompt and review the bundle summary before confirming.
- ARB_MAX_NOTIONAL_SOL must be set to an amount you can afford to lose entirely.

Re-read references/safety-rails.md if any of the above is unclear.
