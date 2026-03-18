// ─── UserOp Mempool + Batch Processor ────────────────────────────────────────
//
// Why batching matters (ERC-4337 §7):
//   A single `EntryPoint.handleOps(ops[])` call can process up to ~25 UserOps
//   in one L1 transaction.  Without batching every UserOp pays a full L1 base
//   cost (21 000 gas overhead + EIP-4337 ~20 000 overhead).  With batching
//   that overhead is amortised across the whole batch.
//
//   More importantly for correctness: batching means the bundler signer only
//   needs ONE nonce increment per batch, eliminating nonce races entirely.
//
// Architecture (Flashbots alto / ERC-4337 reference bundler pattern):
//   1. Each incoming /submit request pushes a `PendingOp` onto a bounded
//      mpsc channel (the "mempool").
//   2. The background `run_batch_processor` task drains the channel:
//        • Waits for the first op.
//        • Collects additional ops for up to BATCH_WINDOW_MS or until
//          MAX_BATCH_SIZE is reached (whichever comes first).
//        • Submits the entire batch via `BundlerService::submit_batch`.
//        • Routes the resulting tx_hash (or error) back to every caller
//          via its oneshot channel.
//   3. The submit route awaits the oneshot, so its HTTP response still
//      contains a real tx_hash — the same API contract as before.
//
// Tuning knobs (adjust for block time of target chain):
//   MAX_BATCH_SIZE   – max UserOps per handleOps call  (gas ceiling ~3M)
//   BATCH_WINDOW_MS  – how long to wait collecting ops before forcing flush
//   CHANNEL_CAPACITY – back-pressure limit; callers block when full

use std::sync::Arc;

use alloy::primitives::B256;
use eyre::{eyre, Result};
use tokio::{
    sync::{mpsc, oneshot},
    time::{timeout, Duration, Instant},
};
use tracing::{error, info, warn};

use super::{service::{BatchSimOutcome, BundlerService}, types::PackedUserOperation};

// ─── Tuning constants ─────────────────────────────────────────────────────────

/// Maximum number of UserOps bundled into a single handleOps transaction.
/// Each op costs ~200 k gas; 3 M gas limit ÷ 200 k ≈ 15 ops safe ceiling.
const MAX_BATCH_SIZE: usize = 10;

/// How long (ms) the processor waits after the first op before flushing.
/// Set to one L2 block time or lower for snappy UX.
const BATCH_WINDOW_MS: u64 = 200;

/// mpsc channel depth — provides back-pressure under extreme load.
const CHANNEL_CAPACITY: usize = 2_000;

// ─── Types ────────────────────────────────────────────────────────────────────

pub struct PendingOp {
    pub user_op:   PackedUserOperation,
    /// Resolved with `Ok(tx_hash)` on success or `Err(msg)` on failure.
    pub result_tx: oneshot::Sender<Result<B256>>,
}

// ─── Mempool handle ───────────────────────────────────────────────────────────

/// Cheap-to-clone handle for pushing UserOps into the batch processor.
#[derive(Clone)]
pub struct Mempool {
    tx: mpsc::Sender<PendingOp>,
}

impl Mempool {
    pub fn new() -> (Self, mpsc::Receiver<PendingOp>) {
        let (tx, rx) = mpsc::channel(CHANNEL_CAPACITY);
        (Self { tx }, rx)
    }

    /// Push a UserOp and wait for the batch result.
    ///
    /// Returns the L1 tx hash when the op's batch has been broadcast, or an
    /// error if the bundle fails after all retries.
    pub async fn push(&self, user_op: PackedUserOperation) -> Result<B256> {
        let (result_tx, result_rx) = oneshot::channel();

        self.tx
            .send(PendingOp { user_op, result_tx })
            .await
            .map_err(|_| eyre!("mempool channel closed — batch processor has crashed"))?;

        result_rx
            .await
            .map_err(|_| eyre!("batch processor dropped the result sender"))?
    }

    /// Current queue depth (approximate).
    pub fn len(&self) -> usize {
        CHANNEL_CAPACITY - self.tx.capacity()
    }
}

// ─── Background batch processor ───────────────────────────────────────────────

/// Spawns the background task that drains the mempool.
///
/// Must be called once during bundler init.  The returned `JoinHandle` can be
/// awaited for clean shutdown or simply detached with `tokio::spawn`.
pub fn spawn_batch_processor(
    rx:      mpsc::Receiver<PendingOp>,
    bundler: Arc<BundlerService>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(batch_processor_loop(rx, bundler))
}

async fn batch_processor_loop(
    mut rx:  mpsc::Receiver<PendingOp>,
    bundler: Arc<BundlerService>,
) {
    let batch_window = Duration::from_millis(BATCH_WINDOW_MS);

    info!(
        "Batch processor started (window={}ms, max_size={})",
        BATCH_WINDOW_MS, MAX_BATCH_SIZE
    );

    loop {
        // ── Wait for the first op ─────────────────────────────────────────
        let first = match rx.recv().await {
            Some(op) => op,
            None => {
                info!("Batch processor: channel closed, shutting down.");
                return;
            }
        };

        let mut batch: Vec<PendingOp> = vec![first];

        // ── Collect more ops within BATCH_WINDOW ──────────────────────────
        let deadline = Instant::now() + batch_window;

        while batch.len() < MAX_BATCH_SIZE {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            match timeout(remaining, rx.recv()).await {
                Ok(Some(op)) => batch.push(op),
                Ok(None)     => break,  // channel closed
                Err(_)       => break,  // window expired
            }
        }

        let batch_size = batch.len();
        info!("Flushing batch of {batch_size} UserOps");

        // ── Extract ops + result senders into parallel vecs ───────────────
        let (mut ops, mut result_txs): (Vec<_>, Vec<_>) = batch
            .into_iter()
            .map(|e| (e.user_op, e.result_tx))
            .unzip();

        // ── Simulate the batch; quarantine any bad op and retry ───────────
        //
        // We simulate the assembled batch (not individual ops) so that the
        // EntryPoint processes them sequentially in the eth_call — meaning
        // op[0] advancing the sender nonce makes op[1] with nonce+1 valid
        // within the same simulation call.
        //
        // If any op fails we send it an error, remove it from the batch, and
        // re-simulate the remainder.  We cap iterations at `batch_size` so
        // a batch composed entirely of bad ops still terminates.
        let mut sim_ok = false;
        for _round in 0..=batch_size {
            if ops.is_empty() {
                break;
            }
            match bundler.simulate_batch(&ops).await {
                BatchSimOutcome::Ok => {
                    sim_ok = true;
                    break;
                }
                BatchSimOutcome::BadOp { index, reason } if index < ops.len() => {
                    warn!(
                        "Quarantining op[{index}] ({}): {reason}",
                        format!("{:#x}", ops[index].sender)
                    );
                    ops.remove(index);
                    let bad_tx = result_txs.remove(index);
                    let _ = bad_tx.send(Err(eyre!("UserOp rejected by EntryPoint: {reason}")));
                }
                BatchSimOutcome::BadOp { index, reason } => {
                    // opIndex out of range — should not happen; bail the batch
                    error!("simulate_batch returned out-of-range opIndex={index}: {reason}");
                    break;
                }
                BatchSimOutcome::RpcError(msg) => {
                    // RPC down / gas limit exceeded — don't quarantine, just
                    // log and fall through to submit_batch which will retry
                    warn!("simulate_batch RPC error (proceeding): {msg}");
                    sim_ok = true; // let submit_batch decide
                    break;
                }
            }
        }

        if ops.is_empty() {
            continue; // every op was quarantined
        }

        // ── Broadcast ─────────────────────────────────────────────────────
        let clean_size = ops.len();
        match bundler.submit_batch(&ops).await {
            Ok(tx_hash) => {
                info!("Batch({clean_size}/{batch_size}) → tx {tx_hash:#x}");
                for tx in result_txs {
                    let _ = tx.send(Ok(tx_hash));
                }
            }
            Err(e) => {
                let msg = e.to_string();
                error!("Batch({clean_size}/{batch_size}) failed: {msg}");
                for tx in result_txs {
                    let _ = tx.send(Err(eyre!("{msg}")));
                }
            }
        }

        let _ = sim_ok; // used only for clarity above
    }
}
