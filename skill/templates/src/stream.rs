// stream.rs -- Yellowstone gRPC account/slot subscriber with reconnect.
//
// Consumes a Yellowstone gRPC stream at `processed` commitment (fastest
// finality tier; appropriate for MEV/arb latency budgets). Subscribes to:
//   - Account updates for a caller-supplied list of pool accounts.
//   - Slot updates (so the detector can track tip-account leader windows).
//
// On connection drop or stream error the task sleeps with exponential backoff
// (250 ms base, 2x, cap 30 s) then reconnects. The outer tokio task runs until
// the returned sender is dropped by the caller.
//
// The decoded update is sent to the detector via an mpsc channel. Back-pressure
// is intentional: if the detector is slow the channel fills and we drop
// individual slot-update messages (SlotUpdate) to prefer account updates.
//
// Latency note: gRPC subscribe at `processed` gives sub-slot account changes
// (~400 ms before confirmed). ShredStream (UDP multicast) is faster still but
// requires co-location; see references/streaming-ingestion.md for that path.

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::sleep;

// Yellowstone gRPC re-exports from the pinned crates.
use yellowstone_grpc_client::{ClientTlsConfig, GeyserGrpcClient};
use yellowstone_grpc_proto::geyser::{
    subscribe_request_filter_accounts_filter::Filter as AccountFilter,
    subscribe_request_filter_accounts_filter_memcmp::Data as MemcmpData,
    CommitmentLevel, SubscribeRequest, SubscribeRequestFilterAccounts,
    SubscribeRequestFilterAccountsFilter, SubscribeRequestFilterAccountsFilterMemcmp,
    SubscribeRequestFilterSlots, SubscribeUpdate, SubscribeUpdateAccount,
    SubscribeUpdateSlot,
};
use yellowstone_grpc_proto::tonic::Status;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Decoded update delivered to the detector/main pipeline.
#[derive(Debug, Clone)]
pub enum StreamUpdate {
    /// An account changed. Carries the raw account bytes and the slot it landed in.
    Account(AccountSnapshot),
    /// A slot was processed. Used by the detector to time Jito leader windows.
    Slot(u64),
}

/// Minimal snapshot of a pool account as observed on the stream.
#[derive(Debug, Clone)]
pub struct AccountSnapshot {
    pub pubkey: [u8; 32],
    pub slot: u64,
    pub lamports: u64,
    /// Raw account data bytes (pool-specific layout; detector parses its own schema).
    pub data: Vec<u8>,
    pub owner: [u8; 32],
    pub executable: bool,
    pub rent_epoch: u64,
}

// ---------------------------------------------------------------------------
// Subscriber
// ---------------------------------------------------------------------------

/// Configuration for the stream subscriber.
pub struct StreamConfig {
    /// Yellowstone gRPC endpoint, e.g. "https://my-rpc:10000"
    pub grpc_url: String,
    /// Bearer token for gRPC auth. Pass empty string if endpoint is unauthenticated.
    pub grpc_token: String,
    /// Base-58 encoded pubkeys of accounts to subscribe to (pool vaults, AMM state, etc.)
    pub account_keys: Vec<String>,
    /// mpsc channel capacity. Tune based on detector throughput.
    pub channel_capacity: usize,
}

/// Spawn the gRPC subscriber as a background tokio task. Returns the receiving end
/// of the update channel. Dropping the receiver signals the background task to stop.
///
/// # Errors
/// The task itself retries indefinitely on recoverable stream errors. Startup errors
/// (e.g. bad endpoint URL) are returned via the first message being absent after a
/// timeout; the caller's channel recv will return None when the task exits.
pub fn subscribe(cfg: StreamConfig) -> mpsc::Receiver<StreamUpdate> {
    let (tx, rx) = mpsc::channel::<StreamUpdate>(cfg.channel_capacity);
    tokio::spawn(subscriber_loop(cfg, tx));
    rx
}

// ---------------------------------------------------------------------------
// Internal: reconnect loop
// ---------------------------------------------------------------------------

async fn subscriber_loop(cfg: StreamConfig, tx: mpsc::Sender<StreamUpdate>) {
    const BASE_BACKOFF_MS: u64 = 250;
    const MAX_BACKOFF_MS: u64 = 30_000;
    let mut backoff_ms = BASE_BACKOFF_MS;

    loop {
        match run_stream(&cfg, &tx).await {
            Ok(()) => {
                // Stream ended cleanly (rare). Short pause then reconnect.
                eprintln!("[stream] gRPC stream closed cleanly; reconnecting in {}ms", backoff_ms);
            }
            Err(e) => {
                eprintln!("[stream] gRPC stream error: {:#}; reconnecting in {}ms", e, backoff_ms);
            }
        }

        // If the downstream receiver has been dropped, exit the loop.
        if tx.is_closed() {
            eprintln!("[stream] downstream receiver dropped; stopping subscriber task");
            return;
        }

        sleep(Duration::from_millis(backoff_ms)).await;
        backoff_ms = (backoff_ms * 2).min(MAX_BACKOFF_MS);
    }
}

async fn run_stream(
    cfg: &StreamConfig,
    tx: &mpsc::Sender<StreamUpdate>,
) -> Result<()> {
    // Build the gRPC client. Token is sent as a x-token metadata header by the client.
    //
    // TLS GOTCHA: every real Triton/Helius Yellowstone endpoint is `https://`,
    // and tonic will NOT negotiate TLS unless a tls_config is attached -- without
    // it `connect()` fails with a transport/TLS error. We attach native roots for
    // https URLs and skip TLS for plaintext (`http://`, local/dev) so both work.
    let mut builder = GeyserGrpcClient::build_from_shared(cfg.grpc_url.clone())
        .context("building GeyserGrpcClient")?
        .x_token(if cfg.grpc_token.is_empty() {
            None
        } else {
            Some(cfg.grpc_token.clone())
        })
        .context("setting x-token")?;

    if cfg.grpc_url.starts_with("https://") {
        builder = builder
            .tls_config(ClientTlsConfig::new().with_native_roots())
            .context("configuring TLS for https gRPC endpoint")?;
    }

    let mut client = builder.connect().await.context("gRPC connect")?;

    // Build account filter: subscribe to all listed pubkeys by literal address match.
    //
    // We set only the fields we rely on and fall back to `..Default::default()`
    // for the rest. yellowstone-grpc-proto adds fields between minor releases
    // (12.5 added `cuckoo_accounts_filter` here, `interslot_updates` on slots,
    // and `from_slot` on SubscribeRequest); spelling every field out would break
    // the build on the next bump. Every proto struct derives Default, so this is
    // the crate's own recommended pattern (see references/streaming-ingestion.md).
    let account_filter = SubscribeRequestFilterAccounts {
        account: cfg.account_keys.clone(),
        owner: vec![],
        filters: vec![],
        ..Default::default()
    };

    let mut accounts: HashMap<String, SubscribeRequestFilterAccounts> = HashMap::new();
    accounts.insert("arb_pools".to_string(), account_filter);

    let mut slots: HashMap<String, SubscribeRequestFilterSlots> = HashMap::new();
    slots.insert(
        "slots".to_string(),
        SubscribeRequestFilterSlots {
            filter_by_commitment: Some(true),
            ..Default::default()
        },
    );

    let request = SubscribeRequest {
        accounts,
        slots,
        commitment: Some(CommitmentLevel::Processed as i32),
        ..Default::default()
    };

    // Subscribe returns a streaming response.
    let (_, stream) = client
        .subscribe_with_request(Some(request))
        .await
        .context("subscribe_with_request")?;

    // Consume the stream. Each message is a SubscribeUpdate enum.
    use futures::StreamExt;
    let mut pinned = Box::pin(stream);
    while let Some(result) = pinned.next().await {
        let update: SubscribeUpdate = result.context("gRPC stream message error")?;

        let parsed = match &update.update_oneof {
            Some(yellowstone_grpc_proto::geyser::subscribe_update::UpdateOneof::Account(acct)) => {
                parse_account_update(acct)
                    .map(StreamUpdate::Account)
                    .ok()
            }
            Some(yellowstone_grpc_proto::geyser::subscribe_update::UpdateOneof::Slot(slot_upd)) => {
                Some(StreamUpdate::Slot(slot_upd.slot))
            }
            _ => None, // Ping frames, unsubscribed update types, etc.
        };

        if let Some(su) = parsed {
            // Use try_send so a slow detector does not block the gRPC read loop;
            // slot updates are dropped under back-pressure (account updates use
            // blocking send to preserve pool state correctness).
            match su {
                StreamUpdate::Slot(s) => {
                    // Best-effort; drop if channel is full.
                    let _ = tx.try_send(StreamUpdate::Slot(s));
                }
                StreamUpdate::Account(_) => {
                    // Block until the detector accepts the update.
                    if tx.send(su).await.is_err() {
                        // Receiver dropped.
                        return Ok(());
                    }
                    // Reset backoff on a successful account delivery.
                }
            }
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Parsing helpers
// ---------------------------------------------------------------------------

fn parse_account_update(upd: &SubscribeUpdateAccount) -> Result<AccountSnapshot> {
    let info = upd.account.as_ref().context("missing account info in update")?;
    let pubkey: [u8; 32] = info
        .pubkey
        .as_slice()
        .try_into()
        .context("pubkey is not 32 bytes")?;
    let owner: [u8; 32] = info
        .owner
        .as_slice()
        .try_into()
        .context("owner is not 32 bytes")?;

    Ok(AccountSnapshot {
        pubkey,
        slot: upd.slot,
        lamports: info.lamports,
        data: info.data.clone(),
        owner,
        executable: info.executable,
        rent_epoch: info.rent_epoch,
    })
}
