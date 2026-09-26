//! GraphSync — centralised Nebula graph write layer with bounded exponential-backoff retry.
//!
//! All event handlers that need to write to the Nebula social graph go through
//! this module instead of calling `GraphClient` write helpers directly.
//!
//! Benefits over the previous scattered approach:
//!   • Errors are returned to callers instead of being swallowed.
//!   • Transient connection/timeout failures are retried up to 3 times with
//!     exponential backoff (100 ms → 200 ms → 400 ms, capped at 4 s).
//!   • Logic errors (invalid input, not-found) are never retried.
//!   • Named operations (`sync_follow`, `sync_unfollow`, etc.) make the intent
//!     obvious in call-sites; raw nGQL strings are not visible outside this file.

use std::time::Duration;

use crate::recommendation::graph_client::{GraphClient, GraphTransport, NebulaConsoleTransport};
use thiserror::Error;
use tracing::warn;

mod social_ops;
mod engagement_ops;
mod media_ops;
mod content_ops;

// ── Error type ────────────────────────────────────────────────────────────────

/// Errors that `GraphSync` can surface to callers.
///
/// Variants distinguish the two root causes so callers can decide whether to
/// retry, alert, or just log:
///  - `ConnectionError` wraps Nebula transport/circuit-breaker failures.
///  - `InvalidInput` means the supplied addresses/IDs failed the safety check
///    and the write was never attempted.
#[derive(Debug, Error)]
pub enum GraphSyncError {
    #[error("Nebula connection error during {operation}: {source}")]
    ConnectionError {
        operation: &'static str,
        #[source]
        source: anyhow::Error,
    },

    #[error("Invalid input for {operation}: {detail}")]
    InvalidInput {
        operation: &'static str,
        detail: String,
    },
}

impl GraphSyncError {
    /// Returns `true` for errors that are worth retrying.
    ///
    /// Connection refused, timeouts, and circuit-open transients are retryable.
    /// Logic errors (`InvalidInput`) and permanent failures are not.
    pub fn is_transient(&self) -> bool {
        match self {
            GraphSyncError::InvalidInput { .. } => false,
            GraphSyncError::ConnectionError { source, .. } => {
                let msg = source.to_string().to_lowercase();
                msg.contains("connection refused")
                    || msg.contains("timed out")
                    || msg.contains("timeout")
                    || msg.contains("reset by peer")
                    || msg.contains("broken pipe")
                    || msg.contains("circuit open")
                    || msg.contains("os error")
            }
        }
    }
}

// ── GraphSync ─────────────────────────────────────────────────────────────────

/// Centralised write layer over `GraphClient` with built-in retry.
///
/// Generic over `T: GraphTransport` so tests can inject a mock transport
/// without spawning a real nebula-console process.
///
/// Holds the client by value (which is `Clone`-cheap because it wraps `Arc`s
/// internally) and exposes named methods for every social-graph mutation the
/// event processor needs.
pub struct GraphSync<T: GraphTransport = NebulaConsoleTransport> {
    client: GraphClient<T>,
}

/// Manual `Clone` impl so that `GraphSync<T>` only requires `GraphClient<T>: Clone`,
/// not `T: Clone` directly. `GraphClient<T>` already implements `Clone` via its own
/// manual impl (it holds `Arc<T>`, so only `Arc` needs to be cloned, not `T`).
impl<T: GraphTransport> Clone for GraphSync<T> {
    fn clone(&self) -> Self {
        Self { client: self.client.clone() }
    }
}

impl<T: GraphTransport> GraphSync<T> {
    /// Create a `GraphSync` from any typed `GraphClient<T>`.
    ///
    /// Generic so tests can pass a mock-transport client and production code
    /// can pass either `GraphClient<NebulaConsoleTransport>` (subprocess) or
    /// `GraphClient<DynGraphTransport>` (trait-object pool adapter).
    pub fn new(client: GraphClient<T>) -> Self {
        Self { client }
    }

    /// Create a `GraphSync` from a client backed by a custom transport.
    ///
    /// Intended for tests: pass a `GraphClient::with_transport(mock)` here.
    #[allow(dead_code)]
    pub fn with_client(client: GraphClient<T>) -> Self {
        Self { client }
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

    /// Execute one nGQL write, mapping transport errors to `ConnectionError`.
    ///
    /// All `inner_sync_*` methods end with `self.run_write(OP, &query)` rather
    /// than duplicating the `execute_write + map_err` block.
    async fn run_write(&self, op_name: &'static str, query: &str) -> Result<(), GraphSyncError> {
        self.client
            .execute_write(query)
            .await
            .map(|_| ())
            .map_err(|e| GraphSyncError::ConnectionError { operation: op_name, source: e })
    }

    /// Execute `f` with bounded exponential backoff.
    ///
    /// * Up to 3 retries (4 attempts total).
    /// * Initial delay: 100 ms, doubles each attempt, capped at 4 s.
    /// * Only `GraphSyncError::is_transient()` errors are retried; logic errors
    ///   and the final attempt propagate immediately.
    async fn with_retry<F, Fut, V>(&self, op_name: &str, f: F) -> Result<V, GraphSyncError>
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = Result<V, GraphSyncError>>,
    {
        let mut delay = Duration::from_millis(100);
        for attempt in 0..=3u32 {
            match f().await {
                Ok(v) => return Ok(v),
                Err(e) if attempt < 3 && e.is_transient() => {
                    warn!(
                        op = op_name,
                        attempt,
                        "graph write retry after {:?}", delay
                    );
                    tokio::time::sleep(delay).await;
                    delay = std::cmp::min(delay * 2, Duration::from_secs(4));
                }
                Err(e) => return Err(e),
            }
        }
        // PANIC-006: return Err instead of unreachable!() so a loop-invariant
        // violation surfaces as a GraphSyncError rather than an unrecoverable panic.
        Err(GraphSyncError::ConnectionError {
            operation: "with_retry",
            source: anyhow::anyhow!(
                "with_retry: loop exited without returning (op={op_name}) — this is a bug"
            ),
        })
    }
}


#[cfg(test)]
mod tests;
