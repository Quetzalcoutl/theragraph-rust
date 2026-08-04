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

use super::{reputation::SenderReputation, service::BatchSimOutcome, types::PackedUserOperation};

/// Seam for batch simulation and submission.
/// Generic over this trait so the processor loop can be tested with a mock.
pub trait BatchSubmitter: Send + Sync {
    fn simulate_batch<'a>(
        &'a self,
        ops: &'a [PackedUserOperation],
    ) -> impl std::future::Future<Output = BatchSimOutcome> + Send + 'a;

    fn submit_batch<'a>(
        &'a self,
        ops: &'a [PackedUserOperation],
    ) -> impl std::future::Future<Output = Result<B256>> + Send + 'a;
}

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
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        CHANNEL_CAPACITY - self.tx.capacity()
    }
}

// ─── Background batch processor ───────────────────────────────────────────────

/// Spawns the background task that drains the mempool.
///
/// `processor` is generic over `BatchSubmitter` so the loop can be tested with
/// a mock without requiring a live RPC connection.
pub fn spawn_batch_processor<P>(
    rx:         mpsc::Receiver<PendingOp>,
    processor:  Arc<P>,
    reputation: SenderReputation,
) -> tokio::task::JoinHandle<()>
where
    P: BatchSubmitter + 'static,
{
    tokio::spawn(batch_processor_loop(rx, processor, reputation))
}

async fn batch_processor_loop<P: BatchSubmitter>(
    mut rx:     mpsc::Receiver<PendingOp>,
    processor:  Arc<P>,
    reputation: SenderReputation,
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
        for _round in 0..=batch_size {
            if ops.is_empty() {
                break;
            }
            match processor.simulate_batch(&ops).await {
                BatchSimOutcome::Ok => {
                    break;
                }
                BatchSimOutcome::BadOp { index, reason } if index < ops.len() => {
                    let bad_sender = ops[index].sender;
                    warn!(
                        "Quarantining op[{index}] ({:#x}): {reason}",
                        bad_sender
                    );
                    // Record as a failure: this op passed simulation but was
                    // rejected by the EntryPoint dry-run, indicating the sender
                    // may be crafting simulation-collision ops.
                    reputation.record_failure(bad_sender);
                    ops.remove(index);
                    let bad_tx = result_txs.remove(index);
                    if bad_tx.send(Err(eyre!("UserOp rejected by EntryPoint: {reason}"))).is_err() {
                        warn!("EntryPoint rejection for {bad_sender:#x}: receiver dropped (caller timed out?)");
                    }
                }
                BatchSimOutcome::BadOp { index, reason } => {
                    // opIndex out of range — indicates a bug in simulate_batch.
                    // Reject the entire remaining batch rather than submitting.
                    error!("simulate_batch returned out-of-range opIndex={index}: {reason}; rejecting batch");
                    for tx in result_txs.drain(..) {
                        if tx.send(Err(eyre!("Batch aborted: simulate_batch out-of-range opIndex={index}"))).is_err() {
                            warn!("BadOp abort: a receiver was dropped (caller timed out?)");
                        }
                    }
                    ops.clear();
                    break;
                }
                BatchSimOutcome::RpcError(msg) => {
                    // RPC down / gas limit exceeded — don't quarantine, just
                    // log and fall through to submit_batch which will retry
                    warn!("simulate_batch RPC error (proceeding): {msg}");
                    break;
                }
            }
        }

        if ops.is_empty() {
            continue; // every op was quarantined
        }

        // ── Broadcast ─────────────────────────────────────────────────────
        let clean_size = ops.len();
        match processor.submit_batch(&ops).await {
            Ok(tx_hash) => {
                info!("Batch({clean_size}/{batch_size}) → tx {tx_hash:#x}");
                for tx in result_txs {
                    if tx.send(Ok(tx_hash)).is_err() {
                        warn!("Batch success tx={tx_hash:#x}: a receiver was dropped (caller timed out?)");
                    }
                }
            }
            Err(e) => {
                let msg = e.to_string();
                error!("Batch({clean_size}/{batch_size}) failed: {msg}");
                for tx in result_txs {
                    if tx.send(Err(eyre!("{msg}"))).is_err() {
                        warn!("Batch failure notification: a receiver was dropped (caller timed out?)");
                    }
                }
            }
        }

    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::{future::Future, sync::Arc};

    use alloy::primitives::{Address, Bytes, FixedBytes, B256, U256};
    use tokio::sync::Mutex;

    use super::{
        batch_processor_loop, Mempool, PendingOp, BatchSubmitter,
        BATCH_WINDOW_MS, MAX_BATCH_SIZE,
    };
    use crate::bundler::{
        reputation::SenderReputation,
        service::BatchSimOutcome,
        types::PackedUserOperation,
    };

    // ─── Helpers ──────────────────────────────────────────────────────────────

    fn dummy_op() -> PackedUserOperation {
        PackedUserOperation {
            sender:               Address::ZERO,
            nonce:                U256::ZERO,
            init_code:            Bytes::default(),
            call_data:            Bytes::default(),
            account_gas_limits:   FixedBytes::ZERO,
            pre_verification_gas: U256::ZERO,
            gas_fees:             FixedBytes::ZERO,
            paymaster_and_data:   Bytes::default(),
            signature:            Bytes::default(),
        }
    }

    fn dummy_op_with_sender(sender: Address) -> PackedUserOperation {
        PackedUserOperation { sender, ..dummy_op() }
    }

    // ─── MockBatchSubmitter ───────────────────────────────────────────────────

    /// Queue of outcomes to return on successive `submit_batch` calls.
    /// `simulate_result` drives `simulate_batch`; when `None` it returns `Ok`.
    struct MockBatchSubmitter {
        /// Results popped in FIFO order on each `submit_batch` call.
        submit_results: Mutex<Vec<eyre::Result<B256>>>,
        /// Optional sequence of simulate outcomes (FIFO).
        /// When the queue is empty, `simulate_batch` returns `BatchSimOutcome::Ok`.
        sim_results: Mutex<Vec<BatchSimOutcome>>,
    }

    impl MockBatchSubmitter {
        /// Always succeeds with `tx_hash`.
        fn always_ok(tx_hash: B256) -> Arc<Self> {
            Arc::new(Self {
                submit_results: Mutex::new(vec![Ok(tx_hash)]),
                sim_results:    Mutex::new(vec![]),
            })
        }

        /// Returns the given error on the first `submit_batch` call.
        fn always_err(msg: &'static str) -> Arc<Self> {
            Arc::new(Self {
                submit_results: Mutex::new(vec![Err(eyre::eyre!(msg))]),
                sim_results:    Mutex::new(vec![]),
            })
        }

        /// Simulate outcomes followed by a successful submit.
        fn with_sim_outcomes(sim: Vec<BatchSimOutcome>, tx_hash: B256) -> Arc<Self> {
            Arc::new(Self {
                submit_results: Mutex::new(vec![Ok(tx_hash)]),
                sim_results:    Mutex::new(sim),
            })
        }
    }

    impl BatchSubmitter for MockBatchSubmitter {
        fn simulate_batch<'a>(
            &'a self,
            _ops: &'a [PackedUserOperation],
        ) -> impl Future<Output = BatchSimOutcome> + Send + 'a {
            async move {
                let mut q = self.sim_results.lock().await;
                if q.is_empty() {
                    BatchSimOutcome::Ok
                } else {
                    q.remove(0)
                }
            }
        }

        fn submit_batch<'a>(
            &'a self,
            _ops: &'a [PackedUserOperation],
        ) -> impl Future<Output = eyre::Result<B256>> + Send + 'a {
            async move {
                let mut q = self.submit_results.lock().await;
                if q.is_empty() {
                    Ok(B256::ZERO)
                } else {
                    q.remove(0)
                }
            }
        }
    }

    // ─── Unit: Mempool struct ─────────────────────────────────────────────────

    /// `Mempool::new()` returns a (Mempool, Receiver) pair; ops pushed on the
    /// handle appear on the receiver.
    #[tokio::test]
    async fn mempool_new_creates_working_channel() {
        let (mempool, mut rx) = Mempool::new();

        // Spawn a task that pushes one op and awaits the result.
        let (result_tx, result_rx) = tokio::sync::oneshot::channel::<eyre::Result<B256>>();
        let pending = PendingOp { user_op: dummy_op(), result_tx };
        mempool.tx.send(pending).await.unwrap();

        let received = rx.recv().await;
        assert!(received.is_some(), "should receive the PendingOp");

        // Ack via result channel so no leaks.
        received.unwrap().result_tx.send(Ok(B256::ZERO)).ok();
        let _ = result_rx; // drop
    }

    /// `Mempool::push` sends an op and the caller gets the tx_hash back.
    #[tokio::test]
    async fn mempool_push_returns_tx_hash() {
        let tx_hash = B256::from([0xab; 32]);
        let (mempool, mut rx) = Mempool::new();

        // Simulate a processor: receive the op, send back the hash.
        tokio::spawn(async move {
            if let Some(op) = rx.recv().await {
                op.result_tx.send(Ok(tx_hash)).ok();
            }
        });

        let result = mempool.push(dummy_op()).await.unwrap();
        assert_eq!(result, tx_hash);
    }

    /// `Mempool::push` propagates errors sent by the processor.
    #[tokio::test]
    async fn mempool_push_propagates_error() {
        let (mempool, mut rx) = Mempool::new();

        tokio::spawn(async move {
            if let Some(op) = rx.recv().await {
                op.result_tx.send(Err(eyre::eyre!("batch failed"))).ok();
            }
        });

        let result = mempool.push(dummy_op()).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("batch failed"));
    }

    /// `Mempool::push` returns `Err` when the processor (receiver) has been
    /// dropped — the channel is effectively closed.
    #[tokio::test]
    async fn mempool_push_errors_when_channel_closed() {
        let (mempool, rx) = Mempool::new();
        drop(rx); // simulate crashed processor

        let result = mempool.push(dummy_op()).await;
        assert!(result.is_err(), "push to closed channel should fail");
    }

    // ─── Integration: batch_processor_loop ───────────────────────────────────

    /// Happy path: push one op, processor submits it, caller gets the hash.
    #[tokio::test]
    async fn processor_happy_path_single_op() {
        let tx_hash = B256::from([0x01; 32]);
        let (tx, rx)  = tokio::sync::mpsc::channel(16);
        let submitter = MockBatchSubmitter::always_ok(tx_hash);
        let rep       = SenderReputation::new();

        tokio::spawn(batch_processor_loop(rx, submitter, rep));

        let (result_tx, result_rx) = tokio::sync::oneshot::channel();
        tx.send(PendingOp { user_op: dummy_op(), result_tx }).await.unwrap();

        let got = result_rx.await.unwrap().unwrap();
        assert_eq!(got, tx_hash);
    }

    /// All ops in a batch receive the same tx_hash.
    #[tokio::test]
    async fn processor_multiple_ops_all_get_hash() {
        let tx_hash   = B256::from([0x02; 32]);
        let (tx, rx)  = tokio::sync::mpsc::channel(16);
        let submitter = MockBatchSubmitter::always_ok(tx_hash);
        let rep       = SenderReputation::new();

        tokio::spawn(batch_processor_loop(rx, submitter, rep));

        const N: usize = 5;
        let mut receivers = Vec::with_capacity(N);
        for _ in 0..N {
            let (result_tx, result_rx) = tokio::sync::oneshot::channel();
            tx.send(PendingOp { user_op: dummy_op(), result_tx }).await.unwrap();
            receivers.push(result_rx);
        }

        for result_rx in receivers {
            let got = result_rx.await.unwrap().unwrap();
            assert_eq!(got, tx_hash);
        }
    }

    /// When `submit_batch` returns `Err`, every op in the batch gets that error.
    #[tokio::test]
    async fn processor_submit_error_propagates_to_all_ops() {
        let (tx, rx)  = tokio::sync::mpsc::channel(16);
        let submitter = MockBatchSubmitter::always_err("rpc timeout");
        let rep       = SenderReputation::new();

        tokio::spawn(batch_processor_loop(rx, submitter, rep));

        const N: usize = 3;
        let mut receivers = Vec::with_capacity(N);
        for _ in 0..N {
            let (result_tx, result_rx) = tokio::sync::oneshot::channel();
            tx.send(PendingOp { user_op: dummy_op(), result_tx }).await.unwrap();
            receivers.push(result_rx);
        }

        for result_rx in receivers {
            let err = result_rx.await.unwrap().unwrap_err();
            assert!(err.to_string().contains("rpc timeout"), "expected rpc timeout, got: {err}");
        }
    }

    /// When MAX_BATCH_SIZE (10) ops are queued the batch flushes without
    /// waiting for BATCH_WINDOW_MS.  We verify by sending exactly 10 ops and
    /// confirming all 10 results arrive well under the window duration.
    #[tokio::test]
    async fn processor_flushes_at_max_batch_size() {
        let tx_hash   = B256::from([0x03; 32]);
        let (tx, rx)  = tokio::sync::mpsc::channel(32);
        let submitter = MockBatchSubmitter::always_ok(tx_hash);
        let rep       = SenderReputation::new();

        tokio::spawn(batch_processor_loop(rx, submitter, rep));

        let mut receivers = Vec::with_capacity(MAX_BATCH_SIZE);
        for _ in 0..MAX_BATCH_SIZE {
            let (result_tx, result_rx) = tokio::sync::oneshot::channel();
            tx.send(PendingOp { user_op: dummy_op(), result_tx }).await.unwrap();
            receivers.push(result_rx);
        }

        // All results should arrive well before the batch window expires.
        let deadline = tokio::time::Duration::from_millis(BATCH_WINDOW_MS / 2);
        for result_rx in receivers {
            let got = tokio::time::timeout(deadline, result_rx)
                .await
                .expect("result should arrive before half the batch window")
                .unwrap()
                .unwrap();
            assert_eq!(got, tx_hash);
        }
    }

    /// Fewer than MAX_BATCH_SIZE ops are flushed after BATCH_WINDOW_MS.
    #[tokio::test]
    async fn processor_flushes_after_batch_window() {
        let tx_hash   = B256::from([0x04; 32]);
        let (tx, rx)  = tokio::sync::mpsc::channel(16);
        let submitter = MockBatchSubmitter::always_ok(tx_hash);
        let rep       = SenderReputation::new();

        tokio::spawn(batch_processor_loop(rx, submitter, rep));

        let (result_tx, result_rx) = tokio::sync::oneshot::channel();
        tx.send(PendingOp { user_op: dummy_op(), result_tx }).await.unwrap();

        // Wait up to 3× the window — the single op must be flushed by then.
        let deadline = tokio::time::Duration::from_millis(BATCH_WINDOW_MS * 3);
        let got = tokio::time::timeout(deadline, result_rx)
            .await
            .expect("op should be flushed after the batch window")
            .unwrap()
            .unwrap();
        assert_eq!(got, tx_hash);
    }

    /// Shutdown: dropping the Mempool sender closes the channel; the processor
    /// loop exits cleanly (JoinHandle resolves).
    #[tokio::test]
    async fn processor_exits_cleanly_on_channel_close() {
        let (tx, rx)  = tokio::sync::mpsc::channel::<PendingOp>(16);
        let submitter = MockBatchSubmitter::always_ok(B256::ZERO);
        let rep       = SenderReputation::new();

        let handle = tokio::spawn(batch_processor_loop(rx, submitter, rep));

        drop(tx); // simulate Mempool being dropped / app shutting down

        let result = tokio::time::timeout(
            tokio::time::Duration::from_millis(200),
            handle,
        )
        .await;

        assert!(result.is_ok(), "processor should exit within 200 ms of channel close");
        assert!(result.unwrap().is_ok(), "processor task should not panic");
    }

    /// `BadOp` simulation: the bad op receives an error, the remaining valid
    /// ops are submitted and receive the tx_hash.
    #[tokio::test]
    async fn processor_bad_op_quarantined_rest_submitted() {
        let tx_hash = B256::from([0x05; 32]);
        let bad_addr = Address::from([0xff; 20]);

        // First simulate call returns BadOp at index 0; second returns Ok.
        let sim_outcomes = vec![
            BatchSimOutcome::BadOp { index: 0, reason: "AA25 invalid account nonce".to_string() },
            BatchSimOutcome::Ok,
        ];
        let submitter = MockBatchSubmitter::with_sim_outcomes(sim_outcomes, tx_hash);
        let rep       = SenderReputation::new();
        let (tx, rx)  = tokio::sync::mpsc::channel(16);

        tokio::spawn(batch_processor_loop(rx, submitter.clone(), rep.clone()));

        let (bad_tx, bad_rx)     = tokio::sync::oneshot::channel();
        let (good_tx, good_rx)   = tokio::sync::oneshot::channel();

        // Send bad op first so it lands at index 0.
        tx.send(PendingOp { user_op: dummy_op_with_sender(bad_addr), result_tx: bad_tx }).await.unwrap();
        tx.send(PendingOp { user_op: dummy_op(), result_tx: good_tx }).await.unwrap();

        let window = tokio::time::Duration::from_millis(BATCH_WINDOW_MS * 3);

        let bad_result = tokio::time::timeout(window, bad_rx).await
            .expect("bad op result should arrive").unwrap();
        assert!(bad_result.is_err(), "bad op should receive an error");
        assert!(bad_result.unwrap_err().to_string().contains("AA25"));

        let good_result = tokio::time::timeout(window, good_rx).await
            .expect("good op result should arrive").unwrap().unwrap();
        assert_eq!(good_result, tx_hash, "good op should receive the tx_hash");

        // The bad sender's reputation should have been dinged.
        assert_eq!(rep.failure_count(bad_addr), 1);
    }

    /// `RpcError` from simulate_batch: the processor falls through to
    /// submit_batch (best-effort broadcast), and callers get the tx_hash.
    #[tokio::test]
    async fn processor_rpc_error_in_sim_falls_through_to_submit() {
        let tx_hash = B256::from([0x06; 32]);

        let sim_outcomes = vec![
            BatchSimOutcome::RpcError("eth_call failed".to_string()),
        ];
        let submitter = MockBatchSubmitter::with_sim_outcomes(sim_outcomes, tx_hash);
        let rep       = SenderReputation::new();
        let (tx, rx)  = tokio::sync::mpsc::channel(16);

        tokio::spawn(batch_processor_loop(rx, submitter, rep));

        let (result_tx, result_rx) = tokio::sync::oneshot::channel();
        tx.send(PendingOp { user_op: dummy_op(), result_tx }).await.unwrap();

        let window = tokio::time::Duration::from_millis(BATCH_WINDOW_MS * 3);
        let got = tokio::time::timeout(window, result_rx).await
            .expect("result should arrive").unwrap().unwrap();
        assert_eq!(got, tx_hash);
    }

    /// Out-of-range BadOp index: the entire remaining batch is rejected.
    #[tokio::test]
    async fn processor_out_of_range_bad_op_rejects_whole_batch() {
        // 1 op in batch, but BadOp claims index 99 — that's out of range.
        let sim_outcomes = vec![
            BatchSimOutcome::BadOp { index: 99, reason: "impossible".to_string() },
        ];
        let submitter = MockBatchSubmitter::with_sim_outcomes(sim_outcomes, B256::ZERO);
        let rep       = SenderReputation::new();
        let (tx, rx)  = tokio::sync::mpsc::channel(16);

        tokio::spawn(batch_processor_loop(rx, submitter, rep));

        let (result_tx, result_rx) = tokio::sync::oneshot::channel();
        tx.send(PendingOp { user_op: dummy_op(), result_tx }).await.unwrap();

        let window = tokio::time::Duration::from_millis(BATCH_WINDOW_MS * 3);
        let err = tokio::time::timeout(window, result_rx).await
            .expect("result should arrive").unwrap().unwrap_err();
        assert!(
            err.to_string().contains("out-of-range") || err.to_string().contains("Batch aborted"),
            "expected batch-abort error, got: {err}"
        );
    }
}
