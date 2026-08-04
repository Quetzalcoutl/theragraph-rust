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
    compute_locks: MokaCache<String, Arc<AsyncMutex<()>>>,
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
        lock_key: String,
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
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::recommendation::scoring::RecommendationReason;

    fn nft(score: f32) -> ScoredNft {
        ScoredNft {
            nft_id: "test".into(),
            token_id: 0,
            contract_address: "0x0".into(),
            score,
            reason: RecommendationReason::Discovery,
            contract_type: "snap".into(),
            creator_address: "0x0".into(),
            tags: vec![],
        }
    }

    fn nfts(count: usize) -> Vec<ScoredNft> {
        (0..count).map(|i| nft(i as f32 / 100.0)).collect()
    }

    fn read_fn(
        data: Vec<ScoredNft>,
    ) -> impl Fn() -> BoxFuture<'static, Result<Option<Vec<ScoredNft>>>> + Send {
        move || {
            let d = data.clone();
            Box::pin(async move { Ok(Some(d)) })
        }
    }

    fn cold_read(
    ) -> impl Fn() -> BoxFuture<'static, Result<Option<Vec<ScoredNft>>>> + Send {
        || Box::pin(async { Ok(None) })
    }

    /// Jon Gjengset / Dan Lickly:
    /// Cache with exactly `offset` items must trigger a miss, not a hit.
    /// `min_cached = offset + 1` is the production invariant — fixes permanent-miss loop.
    #[tokio::test]
    async fn cache_miss_when_exactly_offset_items() {
        let c = StampedeCoalescer::new(2);
        let offset: usize = 19;
        let limit: usize = 20;
        let misses = Arc::new(AtomicUsize::new(0));
        let hits = Arc::new(AtomicUsize::new(0));
        let miss_c = misses.clone();
        let hit_c = hits.clone();

        let _result = c
            .run(
                "key1".into(),
                offset + 1, // min_cached — exactly one more than offset
                offset,     // slice_skip
                limit,      // slice_take
                move || { hit_c.fetch_add(1, Ordering::Relaxed); },
                move || { miss_c.fetch_add(1, Ordering::Relaxed); },
                read_fn(nfts(offset)), // cache has exactly offset items — one short
                || async { Ok(nfts(limit)) },
                |_| Box::pin(async {}),
            )
            .await
            .unwrap();

        assert_eq!(
            misses.load(Ordering::Relaxed), 1,
            "cache with exactly offset items must be a miss (offset={offset}, min_cached={})",
            offset + 1
        );
        assert_eq!(hits.load(Ordering::Relaxed), 0);
    }

    /// Cache with `offset + 1` items is a hit; slice returns items[offset..].
    #[tokio::test]
    async fn cache_hit_when_min_cached_met() {
        let c = StampedeCoalescer::new(2);
        let offset: usize = 5;
        let limit: usize = 10;
        let hits = Arc::new(AtomicUsize::new(0));
        let hit_c = hits.clone();

        let result = c
            .run(
                "key2".into(),
                offset + 1, // min_cached = 6
                offset,     // slice_skip = 5
                limit,      // slice_take = 10
                move || { hit_c.fetch_add(1, Ordering::Relaxed); },
                || {},
                read_fn(nfts(offset + 1)), // 6 items — exactly min_cached
                || async { panic!("compute must not run on a cache hit") },
                |_| Box::pin(async {}),
            )
            .await
            .unwrap();

        assert_eq!(hits.load(Ordering::Relaxed), 1);
        // skip 5 from 6 items → 1 item returned
        assert_eq!(result.len(), 1, "slice(skip={offset}, take={limit}) of 6 items → 1");
    }

    /// Non-paginated path: min_cached = limit, slice_skip = 0.
    #[tokio::test]
    async fn non_paginated_full_cache_hit() {
        let c = StampedeCoalescer::new(2);
        let limit = 20;
        let hits = Arc::new(AtomicUsize::new(0));
        let hit_c = hits.clone();

        let result = c
            .run(
                "key3".into(),
                limit, 0, limit,
                move || { hit_c.fetch_add(1, Ordering::Relaxed); },
                || {},
                read_fn(nfts(limit)),
                || async { panic!("compute must not run") },
                |_| Box::pin(async {}),
            )
            .await
            .unwrap();

        assert_eq!(hits.load(Ordering::Relaxed), 1);
        assert_eq!(result.len(), limit);
    }

    /// Cold cache triggers compute; write_cache is called with result.
    #[tokio::test]
    async fn cold_cache_triggers_compute_and_write() {
        let c = StampedeCoalescer::new(2);
        let written = Arc::new(AtomicUsize::new(0));
        let written_c = written.clone();

        let result = c
            .run(
                "key4".into(),
                10, 0, 10,
                || {},
                || {},
                cold_read(),
                || async { Ok(nfts(10)) },
                move |items| {
                    written_c.fetch_add(items.len(), Ordering::Relaxed);
                    Box::pin(async {})
                },
            )
            .await
            .unwrap();

        assert_eq!(result.len(), 10);
        assert_eq!(
            written.load(Ordering::Relaxed), 10,
            "write_cache must fire after compute with the computed items"
        );
    }
}
