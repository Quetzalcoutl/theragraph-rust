//! `StampedeCoalescer` — stampede-guard kernel extracted from `RecommendationEngine`.
//!
//! Owns per-key `AsyncMutex` registry and the scoring `Semaphore`.
//! All cache I/O is injected via closures so this struct is testable without a
//! live `PgPool` or Redis connection. Carl Lerche's hold condition.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use futures::future::BoxFuture;
use moka::future::Cache as MokaCache;
use tokio::sync::{Mutex as AsyncMutex, Semaphore};

use super::ScoredNft;

/// Stampede-coalescing execution kernel for async cache-or-compute patterns.
///
/// ## Protocol
///
///   1. **Fast path** — call `read_cache()`. Return a sliced view if `cached.len() >= min_cached`.
///   2. **Acquire lock** — one `AsyncMutex<()>` per `lock_key`, stored in `compute_locks`.
///      Concurrent requests for the same key serialise here.
///   3. **Double-check** — re-run `read_cache()` under lock; a waiter may have populated it.
///   4. **Compute** on a true miss.
///   5. **Write cache** — call `write_cache(items)` with the computed result.
///
/// ## Pagination contract
///
/// | Feed type      | `min_cached`  | `slice_skip` | `slice_take` |
/// |----------------|--------------|--------------|--------------|
/// | Non-paginated  | `limit`      | `0`          | `limit`      |
/// | Paginated      | `offset + 1` | `offset`     | `limit`      |
///
/// `offset + 1` (not `offset + limit`) allows partial cache hits: any cached
/// result with at least one item past `offset` is accepted. This fixed a
/// production permanent-miss loop for single-content-type users whose diversity
/// shuffle capped results below `offset + limit`.
#[derive(Clone)]
pub struct StampedeCoalescer {
    // Box<str>: lock_key entries are inserted once (get_with) and never mutated —
    // at max_capacity=20_000 the String capacity field alone would waste up to
    // ~160KB held for the cache's lifetime (time_to_idle=30s, but under sustained
    // load the cache stays near capacity continuously).
    compute_locks: MokaCache<Box<str>, Arc<AsyncMutex<()>>>,
    /// Bounds concurrent `spawn_blocking → rayon` scoring calls.
    /// Exposed so `RecommendationEngine::score_candidates` can acquire a permit
    /// without duplicating semaphore management.
    pub scoring_semaphore: Arc<Semaphore>,
}

impl StampedeCoalescer {
    /// `scoring_concurrency` — semaphore capacity; typically `2 × rayon threads`.
    pub fn new(scoring_concurrency: usize) -> Self {
        Self {
            compute_locks: MokaCache::builder()
                .max_capacity(20_000)
                .time_to_idle(Duration::from_secs(30))
                .build(),
            scoring_semaphore: Arc::new(Semaphore::new(scoring_concurrency.max(1))),
        }
    }

    /// Execute the stampede-coalescing protocol.
    ///
    /// - `lock_key`    — namespace-prefixed key. Use distinct prefixes per feed type
    ///                   to avoid cross-feed lock contention, e.g. `"ef:{addr}"`.
    /// - `min_cached`  — minimum cached length to accept as a hit.
    /// - `slice_skip` / `slice_take` — applied to cached results only.
    /// - `on_hit`      — metric callback; fires on every cache hit.
    /// - `on_miss`     — metric callback; fires on every compute.
    /// - `read_cache`  — injected async cache reader; called at most twice per invocation.
    /// - `compute`     — injected compute closure; called at most once per invocation.
    /// - `write_cache` — called with the compute result for write-through; pass `|_| Box::pin(async {})` to skip.
    pub async fn run<F, Fut>(
        &self,
        lock_key: Box<str>,
        min_cached: usize,
        slice_skip: usize,
        slice_take: usize,
        on_hit: impl Fn() + Send,
        on_miss: impl Fn() + Send,
        read_cache: impl Fn() -> BoxFuture<'static, Result<Option<Vec<ScoredNft>>>> + Send,
        compute: F,
        write_cache: impl FnOnce(Vec<ScoredNft>) -> Pin<Box<dyn Future<Output = ()> + Send>>
            + Send,
    ) -> Result<Vec<ScoredNft>>
    where
        F: FnOnce() -> Fut + Send,
        Fut: Future<Output = Result<Vec<ScoredNft>>> + Send,
    {
        // 1. Fast path — no lock overhead
        if let Some(cached) = read_cache().await? {
            if cached.len() >= min_cached {
                on_hit();
                return Ok(cached.into_iter().skip(slice_skip).take(slice_take).collect());
            }
        }

        // 2. Acquire per-key lock
        let lock = self
            .compute_locks
            .get_with(lock_key, async { Arc::new(AsyncMutex::new(())) })
            .await;
        let _guard = lock.lock().await;

        // 3. Double-check under lock
        if let Some(cached) = read_cache().await? {
            if cached.len() >= min_cached {
                on_hit();
                return Ok(cached.into_iter().skip(slice_skip).take(slice_take).collect());
            }
        }

        // 4. Compute
        on_miss();
        let items = compute().await?;

        // 5. Write through
        write_cache(items.clone()).await;

        Ok(items)
    }
}


#[cfg(test)]
mod tests;
