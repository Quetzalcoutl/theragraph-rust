//! Nebula circuit breaker — shared state and execution guard.
//!
//! Independent read and write breakers on `GraphClient` mean a slow FoF
//! traversal that trips the read breaker does not block edge writes.

use anyhow::Result;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use tracing::{debug, warn};

/// After this many consecutive write *or* query failures the circuit opens
/// and Nebula calls are skipped until the next successful probe.
pub(super) const CIRCUIT_OPEN_THRESHOLD: u32 = 3;

// ── CircuitBreaker ────────────────────────────────────────────────────────────

/// Shared, cheaply-cloneable circuit-breaker state.
///
/// Reads and writes each hold their own instance so they never interfere:
/// a slow bulk read traversal that trips the read breaker does not block
/// write events from reaching Nebula (and going to the DLQ on failure),
/// and a write-backpressure spike does not degrade the read path.
#[derive(Clone)]
pub(super) struct CircuitBreaker {
    pub(crate) consecutive_failures: Arc<AtomicU32>,
    pub(crate) circuit_open: Arc<AtomicBool>,
    pub(crate) last_opened_at: Arc<AtomicU64>,
}

impl Default for CircuitBreaker {
    fn default() -> Self {
        Self {
            consecutive_failures: Arc::new(AtomicU32::new(0)),
            circuit_open: Arc::new(AtomicBool::new(false)),
            last_opened_at: Arc::new(AtomicU64::new(0)),
        }
    }
}

impl CircuitBreaker {
    pub(super) fn is_open(&self) -> bool {
        self.circuit_open.load(Ordering::Acquire)
    }
}

// ── run_circuit_breaker ───────────────────────────────────────────────────────

/// Execute `op` through `cb`, updating failure counts and circuit state.
///
/// Called by `execute_write` (write_cb) and `execute_query*` (read_cb) so
/// read and write failures trip independent breakers. The caller selects
/// the correct breaker — this function is purely mechanical.
///
/// `success_counter` and `error_counter` are Prometheus counter names.
pub(super) async fn run_circuit_breaker<Fut>(
    cb: &CircuitBreaker,
    op: Fut,
    success_counter: &'static str,
    error_counter: &'static str,
) -> Result<String>
where
    Fut: std::future::Future<Output = Result<String>> + Send,
{
    // CB-01: Acquire on the flag load so the Release store of last_opened_at
    // (written before setting circuit_open=true) is visible in this thread.
    let was_already_open = cb.circuit_open.load(Ordering::Acquire);
    if was_already_open {
        let last = cb.last_opened_at.load(Ordering::Relaxed);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        if now.saturating_sub(last) < 30 {
            metrics::counter!("nebula_circuit_skipped_total").increment(1);
            anyhow::bail!("Nebula circuit open — consecutive failures exceeded threshold");
        }
        // CB-02: CAS true→false to atomically claim the half-open probe slot.
        // Only one thread wins; all others bail so we don't thundering-herd the
        // recovering Nebula process.
        if cb.circuit_open
            .compare_exchange(true, false, Ordering::AcqRel, Ordering::Relaxed)
            .is_err()
        {
            anyhow::bail!("Nebula circuit open — probe already in flight");
        }
        debug!("Nebula circuit half-open: probe claimed (sole prober)");
    }

    match op.await {
        Ok(stdout) => {
            cb.consecutive_failures.store(0, Ordering::Relaxed);
            if was_already_open {
                metrics::counter!("nebula_circuit_closed_total").increment(1);
                warn!("Nebula circuit CLOSED — connection restored");
            } else if cb.circuit_open.swap(false, Ordering::Release) {
                metrics::counter!("nebula_circuit_closed_total").increment(1);
                warn!("Nebula circuit CLOSED — connection restored");
            }
            metrics::counter!(success_counter).increment(1);
            Ok(stdout)
        }
        Err(e) => {
            let failures = cb.consecutive_failures.fetch_add(1, Ordering::Relaxed) + 1;
            metrics::counter!(error_counter).increment(1);
            if failures >= CIRCUIT_OPEN_THRESHOLD || was_already_open {
                // CB-01: write last_opened_at BEFORE releasing circuit_open=true
                // so any concurrent Acquire load of circuit_open sees a valid timestamp.
                let ts = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                cb.last_opened_at.store(ts, Ordering::Relaxed);
                // Release store: all prior Relaxed stores are visible to threads
                // that Acquire-load circuit_open.
                cb.circuit_open.store(true, Ordering::Release);
                metrics::counter!("nebula_circuit_opened_total").increment(1);
                warn!("Nebula circuit OPENED after {} consecutive failures", failures);
            }
            Err(e)
        }
    }
}
