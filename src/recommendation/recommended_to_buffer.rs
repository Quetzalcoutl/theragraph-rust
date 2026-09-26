//! Buffered writer for `recommended_to` graph edges.
//!
//! Sherman Ye: at 3300 recommendation serves/sec, spawning one
//! `write_recommended_to_batch` task per serve creates 3300 concurrent Nebula
//! connections. This module serialises all writes through a single background
//! task that drains the channel every 500ms or 100 items — whichever comes first.
//!
//! One Nebula round trip per flush: `do_flush` calls `write_recommended_to_batch_multi`
//! which concatenates all users' nGQL into a single script and fires one `execute_write`.
//! Min-max score normalisation is applied per-user before concatenation so relative
//! rankings are preserved across users.
//! At 3300 rps and 500ms flush interval = at most 1650 items per flush, one TCP round trip.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use tracing::{warn};

use super::graph_client::GraphTraversal;

pub type WriteItem = (String, Box<[(Box<str>, f32)]>);

/// Channel-side handle used by `RecommendationEngine::spawn_feedback_write`.
pub type RecommendedToSender = mpsc::Sender<WriteItem>;

const CHANNEL_CAPACITY: usize = 8192;
const FLUSH_INTERVAL:   Duration = Duration::from_millis(500);
const FLUSH_BATCH_SIZE: usize = 100;

/// Start a background flush task and return the sender side of the channel.
///
/// The task serialises all `recommended_to` writes through a single goroutine-
/// equivalent Tokio task, capping Nebula connection concurrency at 1 regardless
/// of how many feed computations run concurrently.
///
/// On channel overflow (buffer full — happens when Nebula is slow), writes are
/// dropped and a counter incremented. The feedback loop degrades gracefully:
/// some edges are missing, but the service stays healthy.
pub fn start_flusher(
    graph_client: Arc<dyn GraphTraversal>,
    task_tracker: Option<Arc<tokio_util::task::TaskTracker>>,
) -> RecommendedToSender {
    let (tx, rx) = mpsc::channel::<WriteItem>(CHANNEL_CAPACITY);

    let task = flush_loop(rx, graph_client);

    if let Some(ref tt) = task_tracker {
        tt.spawn(task);
    } else {
        tokio::spawn(task);
    }

    tx
}

async fn flush_loop(
    mut rx: mpsc::Receiver<WriteItem>,
    graph_client: Arc<dyn GraphTraversal>,
) {
    loop {
        // Wait for the first item so we don't spin on an empty channel.
        let first = match rx.recv().await {
            Some(item) => item,
            None => return, // channel closed — engine dropped
        };

        let mut batch: Vec<WriteItem> = Vec::with_capacity(FLUSH_BATCH_SIZE);
        batch.push(first);
        let deadline = Instant::now() + FLUSH_INTERVAL;

        // Drain remaining items until flush interval or batch limit.
        loop {
            if batch.len() >= FLUSH_BATCH_SIZE {
                break;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            match tokio::time::timeout(remaining, rx.recv()).await {
                Ok(Some(item)) => batch.push(item),
                Ok(None)       => {
                    // Channel closed — flush what we have then exit
                    do_flush(&batch, &*graph_client).await;
                    return;
                }
                Err(_elapsed)  => break,
            }
        }

        do_flush(&batch, &*graph_client).await;
    }
}

async fn do_flush(batch: &[WriteItem], graph_client: &dyn GraphTraversal) {
    if batch.is_empty() {
        return;
    }

    metrics::counter!("rec_recommended_to_buffer_flushes_total").increment(1);
    metrics::histogram!("rec_recommended_to_buffer_batch_size").record(batch.len() as f64);

    // Single Nebula round-trip for all users in this flush batch:
    // write_recommended_to_batch_multi concatenates per-user nGQL bodies (each
    // min-max normalised independently) behind one USE statement, then calls
    // execute_write once rather than once per user.
    if let Err(e) = graph_client.write_recommended_to_batch_multi(batch).await {
        warn!("recommended_to buffer flush failed ({} users): {e}", batch.len());
        metrics::counter!("rec_recommended_to_buffer_write_failures_total").increment(1);
    }
}
