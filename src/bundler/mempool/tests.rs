use std::{future::Future, sync::Arc};

use alloy::primitives::{Address, Bytes, FixedBytes, B256, U256};
use tokio::sync::Mutex;

use super::{
    batch_processor_loop, Mempool, PendingOp, BatchSubmitter,
    BATCH_WINDOW_MS, MAX_BATCH_SIZE,
};
use crate::bundler::{
    reputation::SenderReputation,
    error::BatchSimOutcome,
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
