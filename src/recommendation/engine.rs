//! Recommendation Engine
//!
//! Core algorithm for generating personalized NFT recommendations.
//! Combines user preferences, content features, social signals, and trending data.
//!
//! Pure scoring logic (weights, strategies, helpers) lives in `scoring.rs`.

use anyhow::Result;
use futures::future::BoxFuture;
use sqlx::PgPool;
use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::timeout as tokio_timeout;
use tracing::{debug, instrument, warn};

use super::graph_client::GraphTraversal;

use super::cache::RecCache;
use super::schema_consts::{FEED_TYPE_PERSONALIZED, FEED_TYPE_ENHANCED};
use super::candidate_repository::{self as repo, CandidateNft};
use super::features::{NftFeatures, ScoringFeatures};
use super::preferences::UserPreferences;
use super::scoring::{
    apply_diversity_shuffle_static, nan_safe_sort_desc, ScoringSession,
    ScoringStrategy, ScoringWeights, WeightedScoring,
};

// Re-export so existing callers (`use crate::recommendation::engine::ScoredNft`, etc.)
// continue to work without changes.
// Re-export scoring output types so existing `use crate::recommendation::engine::*`
// call sites continue to resolve without changes.
#[allow(unused_imports)]
pub use super::scoring::{FollowingScoring, RecommendationReason, ScoredNft};

// ── FoF boost weights ─────────────────────────────────────────────────────────

/// Follow-based FoF boost weight (follows → liked content).
/// Extracted as module-level constants so `apply_cache_boosts` is the single
/// source of truth shared by `get_enhanced_feed` and `get_recommendations`.
const FOF_FOLLOW_WEIGHT: f32 = 0.10;
/// View-event FoF boost weight (follows → viewed content).
const FOF_VIEW_WEIGHT: f32 = 0.05;
/// Comment FoF boost weight (follows → commented content).
const FOF_COMMENT_WEIGHT: f32 = 0.05;
/// Purchase FoF boost weight (follows → purchased content, 30-day half-life).
/// Highest of the FoF weights: a purchase is a 1000 THERA economic commitment.
/// ByteGraph tier: purchase signal outweighs like 3:1 in raw score; 0.15 here
/// reflects that same ratio over the other FoF boosts.
const FOF_PURCHASE_WEIGHT: f32 = 0.15;
/// Share FoF boost weight (follows → shared content, 15-day half-life).
/// Between purchase and follow-like: sharing is the strongest social broadcast signal
/// — it costs attention but not THERA — so 0.12 sits between purchase (0.15) and like (0.10).
const FOF_SHARE_WEIGHT: f32 = 0.12;
/// Bookmark FoF boost weight (follows → bookmarked content, 10-day half-life).
/// Below share (0.12): bookmarks are private intent, no social amplification, so
/// the FoF signal is weaker than an explicit broadcast. Still above view/comment (0.05)
/// because saving something is a more deliberate gesture than dwelling on it.
const FOF_BOOKMARK_WEIGHT: f32 = 0.08;

/// EFF-001: Apply FoF signal boosts and topic-affinity boosts to a scored list.
///
/// Concurrently reads four Redis FoF caches (follows-FoF, view-event-FoF,
/// comment-FoF, purchase-FoF) via `tokio::join!`, merges them into one additive
/// boost map, applies the boosts in a single O(n) pass, then applies topic-affinity
/// boosts from board-dwell signals written by Elixir.
///
/// ByteGraph signal hierarchy (highest to lowest weight):
///   purchase (0.15) → follow-like (0.10) → comment (0.05) → view (0.05)
///
/// Extracted from the identical 63-line blocks that previously appeared in both
/// `get_enhanced_feed` and `get_recommendations` — all tuning now happens here.
async fn apply_cache_boosts(
    scored: &mut Vec<ScoredNft>,
    cache: &RecCache,
    user_address: &str,
) {
    let (fof_opt, view_opt, comment_opt, purchase_opt, share_opt, bookmark_opt) = tokio::join!(
        cache.get_fof_recommendations(user_address),
        cache.get_view_fof_recommendations(user_address),
        cache.get_comment_fof_recommendations(user_address),
        cache.get_purchase_fof_recommendations(user_address),
        cache.get_share_fof_recommendations(user_address),
        cache.get_bookmark_fof_recommendations(user_address),
    );

    let mut boost_map: HashMap<String, f32> = HashMap::new();
    // Cap each raw FoF score at 1.0 before applying weights. Nebula returns power-law
    // distributed composite scores (e.g. 200.0 for a viral post with 20 FoF vertices).
    // Without the cap, `(score + FOF_WEIGHT * 200).clamp(0, 1) = 1.0` for every top item,
    // causing the min-max normalizer in write_recommended_to_batch to see score_range ≈ 0
    // and write 1.0 for all — destroying rank differentiation in the feedback loop.
    if let Some(fof_scores) = fof_opt {
        for (id, s) in fof_scores {
            *boost_map.entry(id).or_insert(0.0) += FOF_FOLLOW_WEIGHT * s.min(1.0);
        }
    }
    if let Some(view_fof_scores) = view_opt {
        for (id, s) in view_fof_scores {
            *boost_map.entry(id).or_insert(0.0) += FOF_VIEW_WEIGHT * s.min(1.0);
        }
    }
    if let Some(comment_fof_scores) = comment_opt {
        for (id, s) in comment_fof_scores {
            *boost_map.entry(id).or_insert(0.0) += FOF_COMMENT_WEIGHT * s.min(1.0);
        }
    }
    if let Some(purchase_fof_scores) = purchase_opt {
        for (id, s) in purchase_fof_scores {
            *boost_map.entry(id).or_insert(0.0) += FOF_PURCHASE_WEIGHT * s.min(1.0);
        }
    }
    if let Some(share_fof_scores) = share_opt {
        for (id, s) in share_fof_scores {
            *boost_map.entry(id).or_insert(0.0) += FOF_SHARE_WEIGHT * s.min(1.0);
        }
    }
    if let Some(bookmark_fof_scores) = bookmark_opt {
        for (id, s) in bookmark_fof_scores {
            *boost_map.entry(id).or_insert(0.0) += FOF_BOOKMARK_WEIGHT * s.min(1.0);
        }
    }

    if !boost_map.is_empty() {
        let mut fof_changed = false;
        for s in scored.iter_mut() {
            if let Some(&b) = boost_map.get(&s.nft_id) {
                s.score = (s.score + b).clamp(0.0, 1.0);
                fof_changed = true;
            }
        }
        if fof_changed {
            nan_safe_sort_desc(scored);
        }
    }

    // Topic affinity boost (+15% max) — board dwell signals written by Elixir.
    // EFF-007: collect Vec<&str> instead of Vec<String> — no per-tag heap allocs.
    let all_tags: Vec<&str> = {
        let mut seen = std::collections::HashSet::new();
        scored
            .iter()
            .flat_map(|s| s.tags.iter().map(String::as_str))
            .filter(|t| seen.insert(*t))
            .collect()
    };
    if !all_tags.is_empty() {
        let affinity_map = cache.mget_topic_affinities(user_address, &all_tags).await;
        if !affinity_map.is_empty() {
            for s in scored.iter_mut() {
                let best = s
                    .tags
                    .iter()
                    .filter_map(|t| affinity_map.get(t.as_str()))
                    .cloned()
                    .fold(0.0f32, f32::max);
                // Affinities are on a 0-10 scale; a new user engaging with a topic
                // may have best=0.3-0.8 which never clears 1.0 — use > 0.0 so the
                // signal fires for any dwell-derived affinity, not just power users.
                if best > 0.0 {
                    s.score = (s.score + (best / 10.0).min(1.0) * 0.15).clamp(0.0, 1.0);
                }
            }
            nan_safe_sort_desc(scored);
        }
    }
}

/// A-03: Remove candidates the user has permanently rejected.
///
/// `not_interested_ids` is pre-fetched by the caller so this is a pure
/// synchronous filter — no DB or cache I/O. id-less (malformed) rows are
/// dropped unconditionally since they can never appear in any set.
fn filter_not_interested(
    candidates: Vec<(CandidateNft, Option<NftFeatures>)>,
    not_interested_ids: &std::collections::HashSet<String>,
) -> Vec<(CandidateNft, Option<NftFeatures>)> {
    if not_interested_ids.is_empty() {
        return candidates;
    }
    candidates
        .into_iter()
        .filter(|(nft, _)| {
            nft.id
                .as_ref()
                .map(|id| !not_interested_ids.contains(id))
                .unwrap_or(false)
        })
        .collect()
}

// ── Cache free functions ───────────────────────────────────────────────────────
//
// Extracted from engine methods so they can be captured as closures by
// StampedeCoalescer::run() — closures cannot borrow `self` directly.

/// Read Redis then PG. Returns `Ok(None)` on miss OR transient DB error (BUG-002).
async fn try_get_cached_free(
    cache: Option<&RecCache>,
    pool: &PgPool,
    user_address: &str,
    feed_type: &str,
) -> Result<Option<Vec<ScoredNft>>> {
    if let Some(c) = cache {
        if let Some(cached) = c
            .get_recommendations::<Vec<ScoredNft>>(user_address, feed_type)
            .await
        {
            return Ok(Some(cached));
        }
    }
    match repo::get_cached_recommendations_pg(pool, user_address, feed_type).await {
        Ok(result) => Ok(result),
        Err(e) => {
            tracing::warn!(user_address, feed_type, "PG cache read failed — treating as miss: {e}");
            metrics::counter!("rec_pg_cache_read_failures_total").increment(1);
            Ok(None)
        }
    }
}

/// Write PG first (durable), then Redis (fast). On PG failure purge Redis (BUG-003).
async fn write_to_caches_free(
    cache: Option<&RecCache>,
    pool: &PgPool,
    user_address: &str,
    feed_type: &str,
    items: &[ScoredNft],
    ttl_minutes: i64,
) {
    let pg_ok = match repo::cache_recommendations_pg(pool, user_address, feed_type, items, ttl_minutes).await {
        Ok(()) => true,
        Err(e) => {
            tracing::error!(user_address, feed_type, "PG cache write failed: {e}");
            metrics::counter!("rec_pg_cache_write_failures_total").increment(1);
            false
        }
    };

    if let Some(c) = cache {
        if pg_ok {
            c.set_recommendations(user_address, feed_type, items).await;
        } else {
            c.delete_recommendations(user_address).await;
            metrics::counter!("rec_redis_cache_write_skipped_total").increment(1);
        }
    }
}

/// Main recommendation engine
#[derive(Clone)]
pub struct RecommendationEngine {
    pool: PgPool,
    // Arc<RwLock<>> allows live weight updates from the score-updater task
    // without restarting the engine. Clone is cheap (Arc refcount bump).
    weights: std::sync::Arc<std::sync::RwLock<ScoringWeights>>,
    cache: Option<RecCache>,
    /// Optional graph client — when present, the engine writes `recommended_to`
    /// edges after each serve so the feedback loop is closed at the graph layer.
    graph_client: Option<Arc<dyn GraphTraversal>>,
    /// RS-03: optional TaskTracker shared with AppState.
    /// When present, fire-and-forget graph writes use task_tracker.spawn() instead
    /// of tokio::spawn() so shutdown can drain them via task_tracker.wait().
    task_tracker: Option<Arc<tokio_util::task::TaskTracker>>,
    /// Stampede-guard kernel: owns per-key mutexes and the scoring semaphore.
    /// Extracted from engine fields so the coalescing protocol is testable
    /// without a live PgPool or Redis connection.
    coalescer: super::coalescer::StampedeCoalescer,
    /// Sherman Ye: buffered writer for `recommended_to` edges.
    /// When Some, `spawn_feedback_write` sends to this channel instead of
    /// spawning a per-user task — serialises all Nebula writes through one
    /// background flusher, capping concurrency at 1 regardless of feed qps.
    recommended_to_tx: Option<super::recommended_to_buffer::RecommendedToSender>,
}

impl RecommendationEngine {
    pub fn new(pool: PgPool) -> Self {
        Self::with_weights(pool, ScoringWeights::default())
    }

    /// Attach a Redis cache layer to the engine.
    pub fn with_cache(mut self, cache: Option<RecCache>) -> Self {
        self.cache = cache;
        self
    }

    /// Attach a graph client — enables `recommended_to` feedback writes after each serve.
    pub fn with_graph_client(mut self, gc: Arc<dyn GraphTraversal>) -> Self {
        self.graph_client = Some(gc);
        self
    }

    /// Expose the pool so callers (e.g. updater) can query active users without re-accepting &PgPool.
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Expose the cache handle so API handlers can invalidate entries after mutations.
    pub fn cache(&self) -> Option<&RecCache> {
        self.cache.as_ref()
    }

    /// Canonical constructor. `new` delegates here with `ScoringWeights::default()`.
    // RS-15: capacity = 2 × rayon thread count prevents blocking-thread exhaustion
    // under a flash crowd while keeping rayon fully saturated.
    // Override at runtime with REC_SCORING_CONCURRENCY (see RecommendationConfig).
    #[allow(dead_code)]
    pub fn with_weights(pool: PgPool, weights: ScoringWeights) -> Self {
        let auto = (rayon::current_num_threads() * 2).max(4);
        let scoring_capacity = std::env::var("REC_SCORING_CONCURRENCY")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|&n| n > 0)
            .unwrap_or(auto);
        Self {
            pool,
            weights: std::sync::Arc::new(std::sync::RwLock::new(weights)),
            cache: None,
            graph_client: None,
            task_tracker: None,
            coalescer: super::coalescer::StampedeCoalescer::new(scoring_capacity),
            recommended_to_tx: None,
        }
    }

    /// Attach a TaskTracker for graceful-shutdown tracking of fire-and-forget graph writes.
    /// Use a buffered writer for `recommended_to` edges (Sherman Ye's concurrency fix).
    ///
    /// Call `recommended_to_buffer::start_flusher` to obtain the sender, then pass it here.
    /// When set, `spawn_feedback_write` sends to the channel instead of spawning per-user tasks.
    pub fn with_recommended_to_sender(
        mut self,
        tx: super::recommended_to_buffer::RecommendedToSender,
    ) -> Self {
        self.recommended_to_tx = Some(tx);
        self
    }

    pub fn with_task_tracker(mut self, tracker: Arc<tokio_util::task::TaskTracker>) -> Self {
        self.task_tracker = Some(tracker);
        self
    }

    /// Hot-reload scoring weights without restarting — called by the score-updater task.
    #[allow(dead_code)]
    pub fn update_weights(&self, new_weights: ScoringWeights) {
        // BUG-005: recover from a poisoned RwLock (a previous thread panicked while
        // holding the write guard).  PoisonError::into_inner() gives back the MutexGuard
        // so we can overwrite the stale value and clear the poison flag.
        match self.weights.write() {
            Ok(mut w) => *w = new_weights,
            Err(poisoned) => {
                let mut w = poisoned.into_inner();
                *w = new_weights;
                tracing::warn!(
                    "scoring weights RwLock was poisoned — recovered and wrote fresh weights"
                );
            }
        }
    }

    /// Begin a scoring session using the default `WeightedScoring` strategy.
    ///
    /// Accepts pre-computed session boost maps from `load_session_boosts` so both
    /// maps are captured at construction time — eliminating the 3-step
    /// `begin_session → struct-update` dance that previously required `pub(crate)`
    /// field access on `ScoringSession`.
    ///
    /// Captures a consistent weight snapshot at call time so any concurrent
    /// `update_weights` call does not affect this pass.
    pub fn begin_session(
        &self,
        prefs: UserPreferences,
        session_tag_boosts: HashMap<String, f32>,
        session_creator_boosts: HashMap<String, f32>,
    ) -> ScoringSession {
        let weights = self
            .weights
            .read()
            .map(|g| g.clone())
            .unwrap_or_else(|_| ScoringWeights::default());
        ScoringSession::new(
            Box::new(WeightedScoring { weights }),
            prefs,
            session_tag_boosts,
            session_creator_boosts,
        )
    }

    /// Begin a scoring session with a custom `ScoringStrategy`.
    ///
    /// The `session_tag_boosts` and `session_creator_boosts` parameters are
    /// forwarded directly to `ScoringSession::new`, eliminating the struct-update
    /// pattern that previously required `pub(crate)` field visibility.
    ///
    /// The weight snapshot from `self.weights` is intentionally not used here —
    /// the supplied strategy is solely responsible for its own parameters.
    pub fn begin_session_with_strategy(
        &self,
        prefs: UserPreferences,
        strategy: impl ScoringStrategy + 'static,
        session_tag_boosts: HashMap<String, f32>,
        session_creator_boosts: HashMap<String, f32>,
    ) -> ScoringSession {
        ScoringSession::new(
            Box::new(strategy),
            prefs,
            session_tag_boosts,
            session_creator_boosts,
        )
    }

    /// Fetch session signals from Redis and return pre-computed boost maps.
    ///
    /// Called before each `ScoringSession` so the rayon blocking pass carries
    /// a weight snapshot instead of hitting Redis per-NFT.
    async fn load_session_boosts(
        &self,
        user_address: &str,
    ) -> (HashMap<String, f32>, HashMap<String, f32>) {
        if let Some(ref cache) = self.cache {
            let signals = cache.get_session_signals(user_address).await;
            if !signals.is_empty() {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs() as i64;
                return super::scoring::compute_session_boost_maps(&signals, now);
            }
        }
        (HashMap::new(), HashMap::new())
    }

    // ── A-03: shared scoring + filtering helpers ──────────────────────────────
    // Three feed paths (get_enhanced_feed, get_recommendations, get_following_feed)
    // share identical score_candidates + filter_not_interested blocks.
    // Centralise here: one fix or weight tweak touches all three.

    /// Convert raw candidates through the scoring session into sorted ScoredNft list.
    ///
    /// Semaphore-bounded so the blocking rayon pool never exhausts Tokio threads.
    async fn score_candidates(
        &self,
        session: ScoringSession,
        candidates: Vec<(CandidateNft, Option<NftFeatures>)>,
    ) -> Result<Vec<ScoredNft>> {
        // Don Eyles: semaphore must not block the feed pipeline indefinitely under load.
        // 500 ms budget — exceeding it means the rayon pool is saturated; return an
        // empty feed immediately rather than queueing behind a backlog.
        let _permit = match tokio_timeout(
            Duration::from_millis(500),
            self.coalescer.scoring_semaphore.clone().acquire_owned(),
        )
        .await
        {
            Ok(Ok(permit)) => permit,
            Ok(Err(e)) => return Err(e.into()),
            Err(_elapsed) => {
                warn!(
                    "scoring_semaphore timeout after 500ms — rayon pool saturated, returning empty feed"
                );
                metrics::counter!("rec_scoring_semaphore_timeout_total").increment(1);
                return Ok(vec![]);
            }
        };
        let scoring_candidates: Vec<(CandidateNft, Option<ScoringFeatures>)> = candidates
            .into_iter()
            .map(|(c, f)| (c, f.as_ref().map(ScoringFeatures::from)))
            .collect();
        let scored = tokio::task::spawn_blocking(move || {
            let _permit = _permit;
            session.score(scoring_candidates)
        })
        .await?;
        Ok(scored)
    }

    /// Get personalized enhanced feed for a user
    /// Optimized by Niko Matsakis (async) + Andrew Gallant (parallel performance)
    ///
    /// Performance improvements:
    /// - Rayon parallel scoring for candidates (3-4x speedup)
    /// - Zero-allocation iterators where possible
    /// - SIMD-friendly scoring with aligned data structures
    /// - Memory pooling for repeated allocations
    #[instrument(skip(self), fields(address = %user_address, limit, offset))]
    pub async fn get_enhanced_feed(
        &self,
        user_address: &str,
        limit: usize,
        offset: usize,
        contract_type_filter: Option<&str>,
    ) -> Result<Vec<ScoredNft>> {
        use super::metrics::PerformanceTimer;
        let _timer = PerformanceTimer::new("get_enhanced_feed");

        let prefs = super::preferences::get_or_create_preferences(
            &self.pool,
            self.cache.as_ref(),
            user_address,
        )
        .await?;

        // Andrew Gallant: Fetch more candidates for better diversity filtering
        // Use multiplicative factor based on request size
        let fetch_multiplier = if limit < 20 { 5 } else { 3 };
        let candidates = self
            .get_candidates(contract_type_filter, limit * fetch_multiplier, offset)
            .await?;

        // Always suppress NFTs the user has explicitly rejected ("not interested").
        // Applied in get_enhanced_feed too so every feed surface respects the signal.
        let not_interested_ids = self
            .get_not_interested_nft_ids(user_address, &candidates)
            .await
            .unwrap_or_else(|e| {
                warn!("get_not_interested_nft_ids failed for {user_address} — filter bypassed: {e}");
                Default::default()
            });
        let candidates = filter_not_interested(candidates, &not_interested_ids);

        // A-03: score_candidates holds the semaphore, converts features, and
        // runs the ScoringSession inside spawn_blocking (RS-15 pattern).
        let (session_tag_boosts, session_creator_boosts) =
            self.load_session_boosts(user_address).await;
        let session = self.begin_session(prefs, session_tag_boosts, session_creator_boosts);
        let mut scored = self.score_candidates(session, candidates).await?;

        // EFF-001: apply FoF and topic-affinity boosts via the shared helper —
        // eliminates the 63-line block duplicated in get_recommendations.
        // Weights and implementation live in `apply_cache_boosts` above.
        if let Some(ref cache) = self.cache {
            apply_cache_boosts(&mut scored, cache, user_address).await;
        }

        // Apply diversity shuffle on already-sorted results
        let result = apply_diversity_shuffle_static(scored, limit);

        debug!(
            "Generated {} recommendations for user {} (parallel scoring)",
            result.len(),
            user_address
        );

        // Close the feedback loop: write recommended_to edges for every served NFT.
        self.spawn_feedback_write(user_address, &result);

        Ok(result)
    }

    /// Check Redis then PostgreSQL for a cached recommendation list.
    ///
    /// Thin wrapper over `try_get_cached_free` — delegates to the free function
    /// so the same logic can be injected into `StampedeCoalescer::run` as a closure.
    #[instrument(skip(self), fields(address = %user_address, feed_type))]
    async fn try_get_cached(
        &self,
        user_address: &str,
        feed_type: &str,
    ) -> Result<Option<Vec<ScoredNft>>> {
        try_get_cached_free(self.cache.as_ref(), &self.pool, user_address, feed_type).await
    }

    /// Write `items` to both Redis (fast) and PostgreSQL (durable).
    ///
    /// Thin wrapper over `write_to_caches_free`.
    #[instrument(skip(self, items), fields(address = %user_address, feed_type, count = items.len()))]
    async fn write_to_caches(
        &self,
        user_address: &str,
        feed_type: &str,
        items: &[ScoredNft],
        ttl_minutes: i64,
    ) {
        write_to_caches_free(self.cache.as_ref(), &self.pool, user_address, feed_type, items, ttl_minutes).await;
    }

    /// Get personalized recommendations for a user
    /// This is the main method called by the Elixir GraphQL API
    #[instrument(skip(self), fields(address = %user_address, limit, exclude_seen))]
    pub async fn get_recommendations(
        &self,
        user_address: &str,
        limit: usize,
        contract_type_filter: Option<&str>,
        exclude_seen: bool,
    ) -> Result<Vec<ScoredNft>> {
        // Redis → PostgreSQL cache check
        if let Some(cached) = self.try_get_cached(user_address, FEED_TYPE_PERSONALIZED).await? {
            if cached.len() >= limit {
                return Ok(cached.into_iter().take(limit).collect());
            }
        }

        // Get user preferences — Redis hit or DB fallback with write-through.
        // The cache logic now lives entirely in get_or_create_preferences so
        // every feed path (get_enhanced_feed, get_following_feed, here) is consistent.
        let prefs = super::preferences::get_or_create_preferences(
            &self.pool,
            self.cache.as_ref(),
            user_address,
        )
        .await?;

        // Get candidate NFTs (more than needed for diversity)
        let candidates = self
            .get_candidates(contract_type_filter, limit * 4, 0)
            .await?;

        // Bulk-load seen NFT IDs to avoid N+1 per-candidate queries
        let seen_nft_ids: std::collections::HashSet<String> = if exclude_seen {
            self.get_seen_nft_ids(user_address, &candidates).await?
        } else {
            std::collections::HashSet::new()
        };

        // Filter out already-seen NFTs before scoring
        let candidates: Vec<_> = if exclude_seen {
            candidates
                .into_iter()
                .filter(|(nft, _)| {
                    nft.id
                        .as_ref()
                        .map(|id| !seen_nft_ids.contains(id))
                        .unwrap_or(false)
                })
                .collect()
        } else {
            candidates
        };

        // Always suppress NFTs the user has explicitly rejected ("not interested").
        // This is unconditional — not gated on exclude_seen — because not_interested
        // is a permanent signal, not a temporary "already seen" state.
        let not_interested_ids = self
            .get_not_interested_nft_ids(user_address, &candidates)
            .await
            .unwrap_or_else(|e| {
                warn!("get_not_interested_nft_ids failed for {user_address} — filter bypassed: {e}");
                Default::default()
            });
        let candidates = filter_not_interested(candidates, &not_interested_ids);

        // A-03: score_candidates (RS-15 semaphore + spawn_blocking + ScoringSession).
        let (session_tag_boosts, session_creator_boosts) =
            self.load_session_boosts(user_address).await;
        let session = self.begin_session(prefs, session_tag_boosts, session_creator_boosts);
        let mut scored = self.score_candidates(session, candidates).await?;

        // EFF-001: apply FoF and topic-affinity boosts via the shared helper —
        // eliminates the 63-line block duplicated from get_enhanced_feed.
        // FoF scores are pre-computed asynchronously; None = cache cold (safe skip).
        if let Some(ref cache) = self.cache {
            apply_cache_boosts(&mut scored, cache, user_address).await;
        }

        // Apply diversity and discovery
        let result = apply_diversity_shuffle_static(scored, limit);

        // Write through to Redis (fast) + PostgreSQL (durable)
        self.write_to_caches(user_address, FEED_TYPE_PERSONALIZED, &result, 10)
            .await;

        // Emit recommendation quality metrics to Prometheus
        {
            use super::metrics::QualityAnalyzer;
            use std::collections::HashSet;
            let unique_creators = result
                .iter()
                .map(|s| s.creator_address.as_str())
                .collect::<HashSet<_>>()
                .len();
            let unique_tags = result
                .iter()
                .flat_map(|s| s.tags.iter().map(String::as_str))
                .collect::<HashSet<_>>()
                .len();
            let total = result.len();
            let tag_matches = result
                .iter()
                .filter(|s| matches!(s.reason, RecommendationReason::TagMatch { .. }))
                .count();
            let creator_matches = result
                .iter()
                .filter(|s| matches!(s.reason, RecommendationReason::CreatorAffinity { .. }))
                .count();
            let content_type_matches = result
                .iter()
                .filter(|s| matches!(s.reason, RecommendationReason::ContentTypeMatch { .. }))
                .count();
            let diversity = QualityAnalyzer::diversity_score(unique_creators, unique_tags, total);
            let personalization = QualityAnalyzer::personalization_score(
                tag_matches,
                creator_matches,
                content_type_matches,
                total,
            );
            metrics::histogram!("rec_diversity_score").record(diversity as f64);
            metrics::histogram!("rec_personalization_score").record(personalization as f64);
            metrics::gauge!("rec_candidates_scored").set(total as f64);

            // MET-S23-01: surface quality regressions via tracing so they appear in
            // Grafana/Loki without requiring a separate polling job.
            let avg_score = if total > 0 {
                result.iter().map(|s| s.score).sum::<f32>() / total as f32
            } else {
                0.0
            };
            let discovery_count = result
                .iter()
                .filter(|s| matches!(s.reason, super::scoring::RecommendationReason::Discovery))
                .count();
            let quality_metrics = super::metrics::RecommendationMetrics {
                unique_creators,
                unique_tags,
                recommendations_returned: total,
                avg_score,
                discovery_count,
                ..Default::default()
            };
            for issue in QualityAnalyzer::detect_issues(&quality_metrics) {
                tracing::warn!(issue, "recommendation quality issue detected");
            }
        }

        // Close the feedback loop: write recommended_to edges for every served NFT.
        // Fires in a background task so it never blocks the API response path.
        // When the user clicks/purchases, the interaction API calls
        // mark_recommendation_served to flip served=true on the edge.
        // RS-03: use task_tracker.spawn() when available so the shutdown path can
        // drain in-flight writes via task_tracker.wait().await before exiting.
        self.spawn_feedback_write(user_address, &result);

        debug!(
            "Generated {} personalized recommendations for user {}",
            result.len(),
            user_address
        );

        Ok(result)
    }

    /// Stampede-coalescing cache-or-compute for feed requests.
    ///
    /// Thin wrapper: builds closures that capture `self.pool` + `self.cache` by
    /// clone, then delegates to `StampedeCoalescer::run`. The coalescer owns the
    /// double-checked-lock protocol and is testable without a live pool.
    ///
    /// `min_cached`      — minimum cached length to accept as a hit.
    ///   Non-paginated: `limit`. Paginated: `offset + 1`.
    /// `slice_skip` / `slice_take` — applied only to cached results.
    /// `cache_ttl_mins`  — `Some(n)` writes through after compute; `None` when
    ///   the compute closure writes through itself.
    async fn coalesced_cached<F, Fut, Hit, Miss>(
        &self,
        user_address: &str,
        feed_type: &'static str,
        lock_key: String,
        min_cached: usize,
        slice_skip: usize,
        slice_take: usize,
        on_hit: Hit,
        on_miss: Miss,
        compute: F,
        cache_ttl_mins: Option<i64>,
    ) -> Result<Vec<ScoredNft>>
    where
        F:    FnOnce() -> Fut + Send,
        Fut:  Future<Output = Result<Vec<ScoredNft>>> + Send,
        Hit:  Fn() + Send,
        Miss: Fn() + Send,
    {
        let pool       = self.pool.clone();
        let cache      = self.cache.clone();
        let pool2      = pool.clone();
        let cache2     = cache.clone();
        let addr: Arc<str> = user_address.into();
        let addr2      = addr.clone();

        self.coalescer.run(
            lock_key,
            min_cached,
            slice_skip,
            slice_take,
            on_hit,
            on_miss,
            move || {
                let pool  = pool.clone();
                let cache = cache.clone();
                let addr  = addr.clone();
                Box::pin(async move {
                    try_get_cached_free(cache.as_ref(), &pool, &addr, feed_type).await
                }) as BoxFuture<'static, Result<Option<Vec<ScoredNft>>>>
            },
            compute,
            move |items| {
                Box::pin(async move {
                    if let Some(ttl) = cache_ttl_mins {
                        write_to_caches_free(cache2.as_ref(), &pool2, &addr2, feed_type, &items, ttl).await;
                    }
                })
            },
        ).await
    }

    /// Like `get_recommendations` but coalesces concurrent requests for the same
    /// address so only one scoring pass runs at a time (stampede guard).
    // RS-08: instrument the production stampede-guarded entry point.
    #[instrument(skip(self), fields(address = %user_address, limit, exclude_seen))]
    pub async fn get_recommendations_coalesced(
        &self,
        user_address: &str,
        limit: usize,
        contract_type_filter: Option<&str>,
        exclude_seen: bool,
    ) -> Result<Vec<ScoredNft>> {
        self.coalesced_cached(
            user_address,
            FEED_TYPE_PERSONALIZED,
            user_address.to_string(),
            limit,   // min_cached: need full page before serving from cache
            0,       // slice_skip: personalized feed has no offset
            limit,   // slice_take
            || {},   // no per-hit metric on the personalized path
            || {},
            || self.get_recommendations(user_address, limit, contract_type_filter, exclude_seen),
            None,    // get_recommendations writes-through internally
        ).await
    }

    /// `get_enhanced_feed` with a cache layer and stampede coalescing.
    ///
    /// Check cache first (offset-aware), compute on miss, write through, return.
    /// API handlers must not call the raw `cache_*` functions directly.
    pub async fn get_enhanced_feed_cached(
        &self,
        user_address: &str,
        limit: usize,
        offset: usize,
        contract_type_filter: Option<&str>,
    ) -> Result<Vec<ScoredNft>> {
        // Cache key omits offset — only safe for offset=0 callers. All current
        // production callers (EnhancedFeed, TrendingFeed adapters) use offset=0.
        // A non-zero offset would read a stale page-N result as page-1 data.
        debug_assert_eq!(offset, 0, "get_enhanced_feed_cached cache key does not include offset; non-zero offset pollutes page-1 cache");

        // Accept partial cache hits: `offset + 1` means "any item after `offset`
        // is enough". The prior `>= offset + limit` caused a permanent miss loop
        // for single-content-type users capped by diversity shuffle (6× DB load).
        self.coalesced_cached(
            user_address,
            FEED_TYPE_ENHANCED,
            format!("ef:{user_address}"),   // namespaced to avoid locking against personalized
            offset + 1,                     // min_cached
            offset,                         // slice_skip
            limit,                          // slice_take
            || metrics::counter!("rec_enhanced_feed_cache_hits_total").increment(1),
            || metrics::counter!("rec_enhanced_feed_cache_misses_total").increment(1),
            || self.get_enhanced_feed(user_address, limit, offset, contract_type_filter),
            Some(5),                         // 5-min TTL for on-demand entries
        ).await
    }

    /// Pre-warm the enhanced feed cache with a TTL that outlasts the background
    /// update interval (default 1 h). Unlike `get_enhanced_feed_cached` which
    /// uses a 5-min TTL suited for on-demand requests, this writes a 70-min TTL
    /// so cold-start 28 s recomputes only happen when the background job hasn't
    /// run recently — not on every feed open after 5 minutes.
    // RS-08: add span so background pre-warm is visible in distributed traces.
    #[instrument(skip(self), fields(address = %user_address))]
    pub async fn warmup_enhanced_feed(&self, user_address: &str) -> Result<()> {
        let items = self
            .get_enhanced_feed(user_address, 100, 0, None)
            .await?;
        self.write_to_caches(user_address, FEED_TYPE_ENHANCED, &items, 70)
            .await;
        Ok(())
    }

    /// Get feed from followed users only.
    ///
    /// Scoring uses `FollowingScoring` (recency × 0.7 + engagement × 0.3) via
    /// the same `ScoringSession` path as `get_recommendations` and
    /// `get_enhanced_feed` — no duplicate sort comparator or inline score loop.
    // RS-08: add span so the following-feed path is visible in traces.
    #[instrument(skip(self), fields(address = %user_address, limit, offset))]
    pub async fn get_following_feed(
        &self,
        user_address: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<ScoredNft>> {
        // Get list of addresses this user follows (Redis → DB fallback)
        let following = if let Some(ref cache) = self.cache {
            if let Some(cached) = cache.get_following(user_address).await {
                cached
            } else {
                let addrs = self.get_following_addresses(user_address).await?;
                cache.set_following(user_address, &addrs).await;
                addrs
            }
        } else {
            self.get_following_addresses(user_address).await?
        };

        if following.is_empty() {
            return Ok(Vec::new());
        }

        // Get NFTs from followed creators and batch-load their features in one pass
        // (single Redis MGET + one DB ANY($1) query via repo::load_features).
        let nfts = self
            .get_nfts_from_creators(&following, limit, offset)
            .await?;
        let candidates =
            repo::load_features(&self.pool, self.cache.as_ref(), nfts).await?;

        // Suppress NFTs the user has explicitly rejected. This is unconditional —
        // not_interested is a permanent signal and must be respected in all feed paths,
        // including the following feed (previously missing, meaning "Not Interested"
        // taps were ignored when the user returned to the following tab).
        let not_interested_ids = self
            .get_not_interested_nft_ids(user_address, &candidates)
            .await
            .unwrap_or_else(|e| {
                warn!("get_not_interested_nft_ids failed for {user_address} — filter bypassed: {e}");
                Default::default()
            });
        let candidates = filter_not_interested(candidates, &not_interested_ids);

        // FollowingScoring: recency × 0.7 + engagement × 0.3 (A-03 helpers).
        let prefs = super::preferences::get_or_create_preferences(
            &self.pool,
            self.cache.as_ref(),
            user_address,
        )
        .await?;
        let (session_tag_boosts, session_creator_boosts) =
            self.load_session_boosts(user_address).await;
        let session = self.begin_session_with_strategy(prefs, FollowingScoring, session_tag_boosts, session_creator_boosts);
        let mut scored = self.score_candidates(session, candidates).await?;

        if let Some(ref cache) = self.cache {
            apply_cache_boosts(&mut scored, cache, user_address).await;
        }

        let scored = apply_diversity_shuffle_static(scored, limit);

        // Close the feedback loop: write recommended_to edges for every served NFT.
        self.spawn_feedback_write(user_address, &scored);

        Ok(scored)
    }

    // ── Thin delegation wrappers to candidate_repository ──────────────────────

    async fn get_seen_nft_ids(
        &self,
        user_address: &str,
        candidates: &[(CandidateNft, Option<NftFeatures>)],
    ) -> Result<std::collections::HashSet<String>> {
        repo::get_seen_nft_ids(&self.pool, self.cache.as_ref(), user_address, candidates).await
    }

    /// Return the subset of `candidates` the user has permanently suppressed.
    ///
    /// Called unconditionally in every feed path — not_interested is a permanent
    /// block, unlike the `exclude_seen` flag which the caller controls.
    async fn get_not_interested_nft_ids(
        &self,
        user_address: &str,
        candidates: &[(CandidateNft, Option<NftFeatures>)],
    ) -> Result<std::collections::HashSet<String>> {
        repo::get_not_interested_nft_ids(&self.pool, user_address, candidates).await
    }

    async fn get_candidates(
        &self,
        contract_type_filter: Option<&str>,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<(CandidateNft, Option<NftFeatures>)>> {
        repo::get_candidates(
            &self.pool,
            self.cache.as_ref(),
            contract_type_filter,
            limit,
            offset,
        )
        .await
    }

    async fn get_following_addresses(&self, user_address: &str) -> Result<Vec<String>> {
        repo::get_following_addresses(&self.pool, user_address).await
    }

    async fn get_nfts_from_creators(
        &self,
        creators: &[String],
        limit: usize,
        offset: usize,
    ) -> Result<Vec<CandidateNft>> {
        repo::get_nfts_from_creators(&self.pool, creators, limit, offset).await
    }

    /// Fire-and-forget: write `recommended_to` edges for every served NFT.
    ///
    /// Called at the end of every feed path so the graph accumulates which items
    /// each user was shown — the click-through interaction closes the feedback loop
    /// when the user later likes/purchases.  Uses task_tracker.spawn() when
    /// available so the shutdown path can drain in-flight writes; falls back to
    /// tokio::spawn() otherwise.
    fn spawn_feedback_write(&self, user_address: &str, result: &[ScoredNft]) {
        if result.is_empty() {
            return;
        }
        let Some(ref gc) = self.graph_client else {
            return;
        };
        let pairs: Vec<(String, f32)> = result.iter().map(|n| (n.nft_id.clone(), n.score)).collect();

        // Sherman Ye: prefer buffered writer — serialises all Nebula writes through one
        // background task rather than spawning one per user serve.
        if let Some(ref tx) = self.recommended_to_tx {
            if let Err(e) = tx.try_send((user_address.to_string(), pairs)) {
                warn!("recommended_to buffer full — dropping write for {user_address}: {e}");
                metrics::counter!("rec_recommended_to_buffer_overflow_total").increment(1);
            }
            return;
        }

        // Fallback: legacy per-task spawn (used when buffer not configured).
        let gc = Arc::clone(gc);
        let addr = user_address.to_string();
        let task = async move {
            let refs: Vec<(&str, f32)> = pairs.iter().map(|(id, s)| (id.as_str(), *s)).collect();
            // S30-15: log failure at the spawn site so metrics/alerting can
            // detect systematic Nebula write-back degradation from this surface.
            if let Err(e) = gc.write_recommended_to_batch(&addr, &refs).await {
                tracing::warn!("spawn_feedback_write: write_recommended_to_batch failed for {addr}: {e}");
            }
        };

        if let Some(ref tt) = self.task_tracker {
            tt.spawn(task);
        } else {
            tokio::spawn(task);
        }
    }
}
