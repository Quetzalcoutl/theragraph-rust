//! Recommendation Engine
//!
//! Core algorithm for generating personalized NFT recommendations.
//! Combines user preferences, content features, social signals, and trending data.
//!
//! Pure scoring logic lives in `super::scoring`.
//! Feed-serving methods live in `feeds.rs`.
//! FoF boost helpers live in `boost.rs`.
//!
//! ## Module layout
//!
//! | sub-module | contents |
//! |-----------|----------|
//! | `boost`   | FOF weight constants, apply_cache_boosts, filter_not_interested, cache read/write free fns |
//! | `feeds`   | get_enhanced_feed, get_recommendations, get_following_feed, coalesced wrappers |

mod boost;
mod candidate_store;
mod feeds;

use candidate_store::CandidateStore;

use anyhow::Result;
use sqlx::PgPool;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::timeout as tokio_timeout;
use tracing::{instrument, warn};

use super::cache::RecCache;
use super::candidate_repository::{self as repo, CandidateNft};
use super::features::{NftFeatures, ScoringFeatures};
// Re-export ScoredNft so existing `use crate::recommendation::engine::ScoredNft`
// call sites continue to resolve without changes.
pub use super::scoring::ScoredNft;

use super::scoring::{ScoringSession, ScoringStrategy, ScoringWeights, WeightedScoring};
use super::preferences::UserPreferences;
use super::graph_client::GraphTraversal;

/// Main recommendation engine.
#[derive(Clone)]
pub struct RecommendationEngine {
    // Private (not pub(super)): CandidateStore itself is only pub(super) to
    // its own module (visible within `engine` and descendants), so a field
    // visible to all of `crate::recommendation` was already wider than the
    // type it exposes could actually be named at — nothing outside `engine`
    // ever accessed `.store` directly (confirmed: all access goes through
    // `self.store.pool()`/`self.store.cache_ref()` from within `engine`'s own
    // submodules). Plain private matches CandidateStore's own visibility.
    store: CandidateStore,
    // Arc<RwLock<>> allows live weight updates from the score-updater task
    // without restarting the engine. Clone is cheap (Arc refcount bump).
    pub(super) weights: std::sync::Arc<std::sync::RwLock<ScoringWeights>>,
    /// Optional graph client — when present, the engine writes `recommended_to`
    /// edges after each serve so the feedback loop is closed at the graph layer.
    pub(super) graph_client: Option<Arc<dyn GraphTraversal>>,
    /// RS-03: optional TaskTracker shared with AppState.
    /// When present, fire-and-forget graph writes use task_tracker.spawn() so
    /// shutdown can drain them via task_tracker.wait().
    pub(super) task_tracker: Option<Arc<tokio_util::task::TaskTracker>>,
    /// Stampede-guard kernel: owns per-key mutexes and the scoring semaphore.
    /// Testable without a live PgPool or Redis connection.
    pub(super) coalescer: super::coalescer::StampedeCoalescer,
    /// Sherman Ye: buffered writer for `recommended_to` edges.
    /// When Some, `spawn_feedback_write` sends to this channel instead of
    /// spawning per-user tasks — serialises all Nebula writes through one
    /// background flusher.
    pub(super) recommended_to_tx: Option<super::recommended_to_buffer::RecommendedToSender>,
}

impl RecommendationEngine {
    pub fn new(pool: PgPool) -> Self {
        Self::with_weights(pool, ScoringWeights::default())
    }

    /// Attach a Redis cache layer to the engine.
    pub fn with_cache(mut self, cache: Option<RecCache>) -> Self {
        self.store = self.store.with_cache(cache);
        self
    }

    /// Attach a graph client — enables `recommended_to` feedback writes after each serve.
    pub fn with_graph_client(mut self, gc: Arc<dyn GraphTraversal>) -> Self {
        self.graph_client = Some(gc);
        self
    }

    /// Expose the pool so callers (e.g. updater) can query active users without re-accepting &PgPool.
    pub fn pool(&self) -> &PgPool {
        self.store.pool()
    }

    /// Expose the cache handle so API handlers can invalidate entries after mutations.
    pub fn cache(&self) -> Option<&RecCache> {
        self.store.cache_ref()
    }

    /// Canonical constructor. `new` delegates here with `ScoringWeights::default()`.
    // RS-15: capacity = 2 × rayon thread count prevents blocking-thread exhaustion
    // under a flash crowd while keeping rayon fully saturated.
    // Override at runtime with REC_SCORING_CONCURRENCY.
    #[allow(dead_code)]
    pub fn with_weights(pool: PgPool, weights: ScoringWeights) -> Self {
        let auto = (rayon::current_num_threads() * 2).max(4);
        let scoring_capacity = std::env::var("REC_SCORING_CONCURRENCY")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|&n| n > 0)
            .unwrap_or(auto);
        Self {
            store: CandidateStore::new(pool),
            weights: std::sync::Arc::new(std::sync::RwLock::new(weights)),
            graph_client: None,
            task_tracker: None,
            coalescer: super::coalescer::StampedeCoalescer::new(scoring_capacity),
            recommended_to_tx: None,
        }
    }

    /// Attach a buffered writer for `recommended_to` edges (Sherman Ye's concurrency fix).
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
        // BUG-005: recover from a poisoned RwLock — PoisonError::into_inner() gives
        // back the MutexGuard so we can overwrite the stale value and clear the flag.
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

    // ── Session construction ──────────────────────────────────────────────────

    /// Begin a scoring session using the default `WeightedScoring` strategy.
    ///
    /// Captures a consistent weight snapshot at call time so any concurrent
    /// `update_weights` call does not affect this pass.
    pub fn begin_session(
        &self,
        prefs: UserPreferences,
        session_tag_boosts: HashMap<Box<str>, f32>,
        session_creator_boosts: HashMap<Box<str>, f32>,
    ) -> ScoringSession {
        let weights = self
            .weights
            .read()
            .map(|g| *g)
            .unwrap_or_default();
        ScoringSession::new(
            Box::new(WeightedScoring { weights }),
            prefs,
            session_tag_boosts,
            session_creator_boosts,
        )
    }

    /// Begin a scoring session with a custom `ScoringStrategy`.
    ///
    /// The weight snapshot from `self.weights` is intentionally not used here —
    /// the supplied strategy is solely responsible for its own parameters.
    pub fn begin_session_with_strategy(
        &self,
        prefs: UserPreferences,
        strategy: impl ScoringStrategy + 'static,
        session_tag_boosts: HashMap<Box<str>, f32>,
        session_creator_boosts: HashMap<Box<str>, f32>,
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
    pub(super) async fn load_session_boosts(
        &self,
        user_address: &str,
    ) -> (HashMap<Box<str>, f32>, HashMap<Box<str>, f32>) {
        if let Some(cache) = self.store.cache_ref() {
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

    // ── Scoring ───────────────────────────────────────────────────────────────

    /// Convert raw candidates through the scoring session into a sorted ScoredNft list.
    ///
    /// Semaphore-bounded so the blocking rayon pool never exhausts Tokio threads.
    pub(super) async fn score_candidates(
        &self,
        session: ScoringSession,
        candidates: Vec<(CandidateNft, Option<NftFeatures>)>,
    ) -> Result<Vec<ScoredNft>> {
        // Don Eyles: 500 ms budget — exceeding it means the rayon pool is saturated.
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

    // ── Thin cache wrappers ───────────────────────────────────────────────────

    /// Check Redis then PostgreSQL for a cached recommendation list.
    ///
    /// Thin wrapper over `boost::try_get_cached_free` — delegates to the free function
    /// so the same logic can be injected into `StampedeCoalescer::run` as a closure.
    #[instrument(skip(self), fields(address = %user_address, feed_type))]
    pub(super) async fn try_get_cached(
        &self,
        user_address: &str,
        feed_type: &str,
    ) -> Result<Option<Vec<ScoredNft>>> {
        boost::try_get_cached_free(self.store.cache_ref(), self.store.pool(), user_address, feed_type).await
    }

    /// Write `items` to both Redis (fast) and PostgreSQL (durable).
    ///
    /// Thin wrapper over `boost::write_to_caches_free`.
    #[instrument(skip(self, items), fields(address = %user_address, feed_type, count = items.len()))]
    pub(super) async fn write_to_caches(
        &self,
        user_address: &str,
        feed_type: &str,
        items: &[ScoredNft],
        ttl_minutes: i64,
    ) {
        boost::write_to_caches_free(self.store.cache_ref(), self.store.pool(), user_address, feed_type, items, ttl_minutes).await;
    }

    /// Invalidate the genre-filtered recommendation cache entry (both Redis
    /// and the PG-durable tier) for exactly the given `genre_slugs` set —
    /// GENRE-02 follow-up.
    ///
    /// Called by `api::profile::write_genre_preferences` with the user's OLD
    /// declared `genre_preference` slugs, right before the new declaration is
    /// written, so a user who just edited their favorites gets a fresh
    /// genre-filtered feed on their very next "For You" request rather than
    /// waiting out `feeds::GENRE_ONDEMAND_TTL_MINS`/`feeds::GENRE_PREWARM_TTL_MINS`.
    /// This is defense-in-depth on top of that short TTL, not a replacement
    /// for it — see `feeds::GENRE_ONDEMAND_TTL_MINS`'s doc for why exhaustive
    /// per-genre-combination invalidation tracking was deliberately not built.
    ///
    /// No-op when `genre_slugs` is empty — nothing is ever cached under an
    /// empty slug set (see `get_recommendations_coalesced`).
    pub async fn invalidate_genre_feed(&self, user_address: &str, genre_slugs: &[String]) {
        if genre_slugs.is_empty() {
            return;
        }
        let feed_type = super::cache::genre_feed_type(genre_slugs);
        if let Some(cache) = self.store.cache_ref() {
            cache.delete_recommendations_for_feed_type(user_address, &feed_type).await;
        }
        if let Err(e) = repo::delete_cached_recommendations_pg(self.store.pool(), user_address, &feed_type).await {
            warn!("invalidate_genre_feed: PG delete failed for {user_address}: {e}");
        }
    }

    // ── Thin delegation wrappers to candidate_repository ─────────────────────

    pub(super) async fn get_seen_nft_ids(
        &self,
        user_address: &str,
        candidates: &[(CandidateNft, Option<NftFeatures>)],
    ) -> Result<std::collections::HashSet<Box<str>>> {
        repo::get_seen_nft_ids(self.store.pool(), self.store.cache_ref(), user_address, candidates).await
    }

    /// Return the subset of `candidates` the user has permanently suppressed.
    pub(super) async fn get_not_interested_nft_ids(
        &self,
        user_address: &str,
        candidates: &[(CandidateNft, Option<NftFeatures>)],
    ) -> Result<std::collections::HashSet<Box<str>>> {
        repo::get_not_interested_nft_ids(self.store.pool(), user_address, candidates).await
    }

    pub(super) async fn get_candidates(
        &self,
        contract_type_filter: Option<&str>,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<(CandidateNft, Option<NftFeatures>)>> {
        repo::get_candidates(
            self.store.pool(),
            self.store.cache_ref(),
            contract_type_filter,
            limit,
            offset,
        )
        .await
    }

    /// Candidates matching the user's strong tag/creator affinities, bypassing
    /// the recency window `get_candidates` applies. See
    /// `candidate_repository::list_affinity_candidates` for why this exists.
    pub(super) async fn get_affinity_candidates(
        &self,
        top_tags: &[String],
        top_creators: &[String],
        limit: usize,
    ) -> Result<Vec<(CandidateNft, Option<NftFeatures>)>> {
        repo::get_affinity_candidates(
            self.store.pool(),
            self.store.cache_ref(),
            top_tags,
            top_creators,
            limit,
        )
        .await
    }

    pub(super) async fn get_following_addresses(&self, user_address: &str) -> Result<Vec<String>> {
        repo::get_following_addresses(self.store.pool(), user_address).await
    }

    pub(super) async fn get_nfts_from_creators(
        &self,
        creators: &[String],
        limit: usize,
        offset: usize,
    ) -> Result<Vec<CandidateNft>> {
        repo::get_nfts_from_creators(self.store.pool(), creators, limit, offset).await
    }

    // ── Feedback loop ─────────────────────────────────────────────────────────

    /// Fire-and-forget: write `recommended_to` edges for every served NFT.
    ///
    /// Uses task_tracker.spawn() when available so the shutdown path can drain
    /// in-flight writes; falls back to tokio::spawn() otherwise.
    pub(super) fn spawn_feedback_write(&self, user_address: &str, result: &[ScoredNft]) {
        if result.is_empty() {
            return;
        }
        let Some(ref gc) = self.graph_client else {
            return;
        };
        let pairs: Box<[(Box<str>, f32)]> = result.iter().map(|n| (n.nft_id.clone(), n.score)).collect();

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
            if let Err(e) = gc.write_recommended_to_batch(&addr, &pairs).await {
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
