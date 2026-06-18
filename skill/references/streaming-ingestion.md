# Streaming Ingestion (Yellowstone gRPC)

Maps to `skill/templates/src/stream.rs`. Goal: get pool/account state and slot
progress into the detector with the lowest possible latency, and never silently
stall. Crates: `yellowstone-grpc-client = "13.1"`, `yellowstone-grpc-proto = "12.5"`
(see references/sdk-versions.md).

## Why gRPC, not WebSocket/polling
- `getProgramAccounts` polling is slow and rate-limited; JSON-RPC `accountSubscribe`
  is per-account and lags under load.
- Yellowstone (Geyser plugin) pushes account writes and slot updates as they are
  processed by the validator, off the RPC hot path. This is the standard low-latency
  ingestion path for execution bots in 2026.
- You still need a separate RPC for `simulateTransaction` and `sendTransaction`
  (see references/transaction-landing.md) and Jito for bundle submit
  (see references/jito-bundles.md).

## Commitment: use `processed` for signal
- `processed` (CommitmentLevel = Processed) gives the earliest account state, ~1
  slot ahead of `confirmed`. For DETECTION you want this.
- `processed` state can be rolled back on a fork. Never treat a `processed` read as
  settled truth: re-check economics at simulate time, and let the RiskGate
  (references/risk-and-killswitch.md) cap exposure. Use `confirmed`/`finalized`
  only for reconciliation/PnL, not for the hot path.

## SubscribeRequest shape
Subscribe to exactly the accounts you need plus slots. Do not subscribe to whole
programs unless required; narrow filters = less bandwidth = less deserialization in
the inner loop.

- `accounts`: map name -> `SubscribeRequestFilterAccounts { account: [<pubkey>...],
  owner: [<program_id>...], filters: [] }`. Prefer explicit `account` pubkeys
  (the specific pool/vault accounts) over `owner`-wide subscriptions.
- `slots`: map name -> `SubscribeRequestFilterSlots { filter_by_commitment: Some(true) }`
  to drive a slot clock for leader timing (references/jito-bundles.md) and staleness.
- `commitment`: `Some(CommitmentLevel::Processed as i32)`.
- Leave `transactions`, `blocks`, `entry`, `accounts_data_slice` empty unless a
  detector strategy needs them; each adds deserialization cost.

```rust
use std::collections::HashMap;
use yellowstone_grpc_proto::geyser::{
    subscribe_request_filter_accounts_filter as _, // brought in as needed
    CommitmentLevel, SubscribeRequest, SubscribeRequestFilterAccounts,
    SubscribeRequestFilterSlots,
};

fn build_request(pool_accounts: Vec<String>) -> SubscribeRequest {
    let mut accounts = HashMap::new();
    accounts.insert(
        "pools".to_string(),
        SubscribeRequestFilterAccounts {
            account: pool_accounts, // explicit pubkeys you trade against
            owner: vec![],
            filters: vec![],
            nonempty_txn_signature: None,
        },
    );

    let mut slots = HashMap::new();
    slots.insert(
        "slots".to_string(),
        SubscribeRequestFilterSlots { filter_by_commitment: Some(true), interslot_updates: None },
    );

    SubscribeRequest {
        accounts,
        slots,
        commitment: Some(CommitmentLevel::Processed as i32),
        ..Default::default()
    }
}
```

Note: proto field sets shift between yellowstone-grpc-proto minors. If a field above
does not exist in 12.5, use `..Default::default()` and set only the fields the
compiler confirms. Do not invent fields; run `cargo build` and follow the errors.

## Connect, subscribe, consume
```rust
use anyhow::{Context, Result};
use futures::{sink::SinkExt, stream::StreamExt};
use yellowstone_grpc_client::GeyserGrpcClient;

async fn run(endpoint: String, x_token: Option<String>, req: SubscribeRequest) -> Result<()> {
    let mut client = GeyserGrpcClient::build_from_shared(endpoint)?
        .x_token(x_token)?            // None for unauthenticated endpoints
        .connect()
        .await
        .context("geyser connect")?;

    let (mut subscribe_tx, mut stream) = client.subscribe().await?;
    subscribe_tx.send(req).await.context("send subscribe request")?;

    while let Some(message) = stream.next().await {
        let update = message.context("stream error")?;
        // dispatch on update.update_oneof: Account(_) | Slot(_) | Ping(_) | ...
        // keep this branch allocation-free; hand off to detector (references/opportunity-detection.md)
        let _ = update; // see stream.rs for the real match
    }
    Ok(()) // stream ended -> caller reconnects with backoff
}
```

## Reconnect / backoff (mandatory)
The stream WILL drop (validator restart, network blip, server-side timeout). Treat
end-of-stream and errors identically: reconnect.
- Exponential backoff with jitter and a cap: e.g. 250ms -> x2 -> cap 5s, +/-20% jitter.
- Reset the delay to the floor after a successful run of N seconds.
- Respond to server `Ping` updates to keep the connection alive.
- Track last-update timestamps; if no account/slot update for a watchdog interval
  (e.g. 2-3 slots, ~1s+) force-reconnect even without an error -> stale streams
  must not feed the detector.
- On reconnect, mark all cached pool state STALE until the first fresh update for
  each account arrives; the detector must refuse to fire on stale state.

## ShredStream (earliest signal)
- Jito ShredStream delivers shreds (pre-confirmation tx data) earlier than any
  account-level feed, because you see transactions before the validator has applied
  them to account state. It is the lowest-latency signal available.
- Cost/complexity: you reconstruct/parse partial block data yourself; output is
  noisier and not guaranteed (shreds can be missing/out of order). Treat it as an
  EARLY HINT to pre-warm a candidate, never as confirmed state.
- Recommended path: ship v1 on Yellowstone account/slot streaming (above). Add
  ShredStream later only if measured latency to detection is your bottleneck and you
  can validate every hint against account state + simulation before acting.

## Latency discipline
- Avoid allocation in the per-update branch (see rules/rust-style.md). Pre-allocate
  buffers and decode into reused structs.
- Keep heavy work (logging, metrics flush) off the consume loop; use a bounded
  channel to a worker so a slow consumer applies backpressure instead of unbounded
  memory growth.
- Do all signing/sending elsewhere; this module only ingests.

## Checklist
- [ ] `processed` commitment for detection; `confirmed`/`finalized` only for PnL.
- [ ] Narrow account filters; subscribe to slots for the slot clock.
- [ ] Backoff+jitter reconnect; respond to Ping; watchdog on staleness.
- [ ] Cached state flagged STALE on reconnect until refreshed.
- [ ] No allocation / no signing in the inner loop.
