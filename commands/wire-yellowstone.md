---
description: Wire the yellowstone-grpc-client consumer from stream.rs into your executor, including reconnect/backoff logic and the channel handoff to the detector.
---

# /wire-yellowstone

Wire the yellowstone-grpc-client 13.1 consumer (src/stream.rs) into the
solana-arb-executor pipeline, connect it to the detection channel, and configure
exponential-backoff reconnect so ingestion survives transient gRPC disconnects.

Reference leaf: solana-arb-executor / references/streaming-ingestion.md

---

## Steps

### 1. Confirm the dependency is in Cargo.toml

```sh
grep "yellowstone" Cargo.toml
```

Expected output (both lines present):

```
yellowstone-grpc-client = "13.1"
yellowstone-grpc-proto  = "12.5"
```

If missing, add them and run `cargo fetch` to resolve.

### 2. Set ARB_GRPC_URL in your environment

The stream will not connect without a real gRPC endpoint. Triton One and Helius both
expose Yellowstone-compatible endpoints. Config::from_env() reads `ARB_GRPC_URL`
(and `ARB_GRPC_TOKEN` for authenticated endpoints). Set in .env:

```
ARB_GRPC_URL=https://your-grpc-endpoint:443
ARB_GRPC_TOKEN=your-x-token-if-required
```

Confirm the URL is reachable:

```sh
grpcurl -plaintext "${ARB_GRPC_URL}" list 2>&1 | head -5
```

If grpcurl is not installed, get it from https://github.com/fullstorydev/grpcurl
(e.g. `go install github.com/fullstorydev/grpcurl/cmd/grpcurl@latest` or your OS
package manager) or use a REST health check provided by your gRPC provider.

### 3. Review stream.rs for your pool accounts

Open src/stream.rs. The SubscribeRequest.accounts filter is a map of filter_name ->
SubscribeRequestFilterAccounts. You must populate the account pubkeys to watch:

```rust
// In stream.rs, replace POOL_ACCOUNTS_PLACEHOLDER with real pubkeys.
// Example for a two-pool watch:
let mut accounts = HashMap::new();
accounts.insert(
    "pool_watch".to_string(),
    SubscribeRequestFilterAccounts {
        account: vec![
            "POOL_A_PUBKEY_BASE58".to_string(),
            "POOL_B_PUBKEY_BASE58".to_string(),
        ],
        owner:   vec![],
        filters: vec![],
        nonempty_txn_signature: None,
    },
);
```

Replace the placeholder strings with the actual AMM pool account pubkeys you intend
to arbitrage. Do not leave placeholder pubkeys in a live deployment.

### 4. Confirm slot subscription uses processed commitment

In the SubscribeRequest.slots filter, commitment must be set to processed (fastest
tier). Verify in stream.rs:

```rust
let mut slots = HashMap::new();
slots.insert(
    "slot_watch".to_string(),
    SubscribeRequestFilterSlots {
        filter_by_commitment: Some(true),
    },
);
// And at the top-level request:
// commitment: Some(CommitmentLevel::Processed as i32),
```

Using confirmed or finalized commitment adds 400-800 ms of latency per slot.
Processed is mandatory for the hot path.

### 5. Verify the reconnect loop structure in stream.rs

The reconnect loop must be the outer loop. Open src/stream.rs and confirm this
structure exists (do not invert it):

```rust
// Outer: reconnect loop with backoff
let mut backoff_ms: u64 = 500;
loop {
    match connect_and_consume(&config, &tx).await {
        Ok(()) => {
            // Clean disconnect (shutdown signal); break outer loop.
            break;
        }
        Err(e) => {
            let jitter = backoff_ms / 5; // +/- 20%
            let sleep_ms = backoff_ms + (rand_jitter() % (2 * jitter + 1)).saturating_sub(jitter);
            tracing::warn!("gRPC stream error: {e:#}; reconnecting in {sleep_ms} ms");
            tokio::time::sleep(Duration::from_millis(sleep_ms)).await;
            backoff_ms = (backoff_ms * 2).min(30_000);
        }
    }
}
```

If the structure differs, refactor to match. A reconnect loop that is inner to the
consume loop will silently drop updates during the connection race.

### 6. Verify the channel send is non-blocking

Find the channel send in stream.rs. It must use try_send (non-blocking), never send
(blocking), because blocking ingestion on a full channel stalls the entire
slot-update pipeline:

```rust
// LATENCY: try_send is O(1) and never blocks ingestion.
// If the channel is full, the detector is falling behind; log and drop.
if let Err(e) = tx.try_send(update) {
    tracing::warn!("detector channel full, dropping update: {e}");
}
```

If you see a plain `tx.send(update).await` in the hot path, replace it with
try_send and add the warn log.

### 7. Wire the channel receiver into the detector

In src/main.rs, the channel is created with a bounded capacity of 64. Confirm:

```rust
let (tx, rx) = tokio::sync::mpsc::channel::<AccountUpdate>(64);
```

Pass tx to the stream task and rx to the detector task:

```rust
// Ingestion task
tokio::spawn(stream::run(config.clone(), tx));

// Detection task
tokio::spawn(detector::run(config.clone(), rx, risk_gate.clone()));
```

The channel capacity of 64 means the detector can lag at most 64 updates before
drops occur. If you observe frequent drop warnings, the detector is too slow; review
references/opportunity-detection.md for optimization options.

### 8. Test with a dry-run stream session

With ARB_DRY_RUN=true and ARB_GRPC_URL set, run the executor and observe that account
updates are arriving:

```sh
RUST_LOG=info cargo run 2>&1 | head -40
```

Expected log lines:
```
INFO  stream: connected to gRPC endpoint
INFO  stream: subscribed to 2 pool accounts, processed commitment
INFO  detector: received account update for POOL_A_PUBKEY
```

If you see only the connected line but no updates, the pool accounts may be inactive
or the pubkeys are wrong. Use `solana account <PUBKEY>` to verify on-chain state.

### 9. Test reconnect behavior

In a separate terminal, verify reconnect fires:

```sh
# Simulate a disconnect by blocking the gRPC port temporarily:
# (Linux) sudo iptables -I OUTPUT -p tcp --dport 443 -j DROP
# Observe: WARN reconnecting in 500 ms
# (Linux) sudo iptables -D OUTPUT -p tcp --dport 443 -j DROP
# Observe: INFO connected to gRPC endpoint (backoff reset to 500 ms on next error)
```

On Windows, use Windows Firewall rules or simply kill/restart the local gRPC proxy
if you are running one. Confirm backoff doubles on successive failures (500 -> 1000
-> 2000 -> ... -> 30000 ms cap).

### 10. Final checklist

- [ ] ARB_GRPC_URL is set and reachable.
- [ ] Pool account pubkeys are real (not placeholders).
- [ ] Commitment is processed in both SubscribeRequest and slot filter.
- [ ] Reconnect loop is outer; consume loop is inner.
- [ ] Channel send is try_send (non-blocking).
- [ ] ARB_DRY_RUN=true and ARB_REQUIRE_CONFIRM=true remain set.
- [ ] Observed account update log lines in a dry-run session.
