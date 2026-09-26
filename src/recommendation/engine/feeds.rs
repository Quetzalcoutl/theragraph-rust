//! Feed-serving methods for `RecommendationEngine`.
//!
//! Each public method is a distinct feed surface:
//!
//! | method | surface |
//! |--------|---------|
//! | `get_enhanced_feed` | Personalized grid with FoF + diversity |
//! | `get_recommendations` | Main API endpoint (coalesced via `get_recommendations_coalesced`) |
//! | `get_following_feed` | Feed from followed creators only |
//! | `get_enhanced_feed_cached` | Stampede-guarded `get_enhanced_feed` |
//! | `get_recommendations_coalesced` | Stampede-guarded `get_recommendations` (unfiltered AND genre-filtered) |
//! | `warmup_enhanced_feed` | Background pre-warm for enhanced feed |
//! | `warmup_genre_feed` | Background pre-warm for a user's primary genre-filtered feed |
//!
//! All scoring goes through `self.score_candidates` (RS-15 semaphore + rayon).
//! FoF/topic boosts go through `boost::apply_cache_boosts`.

use std::collections::HashSet;
use std::future::Future;
use std::sync::Arc;

use anyhow::Result;
use futures::future::BoxFuture;
use tracing::{debug, instrument, warn};

use crate::recommendation::cache::{genre_feed_type, RecCache as _RecCache};
use crate::recommendation::candidate_repository as repo;
use crate::recommendation::candidate_repository::CandidateNft;
use crate::recommendation::features::NftFeatures;
use crate::recommendation::preferences::UserPreferences;
use crate::recommendation::schema_consts::{FEED_TYPE_ENHANCED, FEED_TYPE_PERSONALIZED};
use crate::recommendation::scoring::{
    apply_diversity_shuffle_static, FollowingScoring, RecommendationReason, ScoredNft,
};

use super::boost::{apply_cache_boosts, filter_not_interested, try_get_cached_free, write_to_caches_free};
use super::RecommendationEngine;

/// GENRE-02 follow-up: PG-durable TTL for an on-demand genre-filtered
/// recommendation request (`get_recommendations_coalesced` when `genre_slugs`
/// is non-empty). Deliberately shorter than the personalized feed's 10-min
/// TTL — rather than tracking every genre-hash variant ever cached for a
/// user so preference-change invalidation can enumerate and delete them all,
/// a short TTL lets a stale entry self-heal quickly on its own. The Redis
/// tier still caps out at `RECOMMENDATION_TTL` (10 min, fixed for every feed
/// type — see `RecCache::set_recommendations`) regardless of this value; this
/// constant only governs the PG fallback tier.
const GENRE_ONDEMAND_TTL_MINS: i64 = 12;

/// GENRE-02 follow-up: PG-durable TTL written by the hourly pre-warm pass
/// (`warmup_genre_feed`, called from `updater::update_all_recommendations`).
/// Mirrors `warmup_enhanced_feed`'s 70-min pattern so the PG entry survives
/// the full hourly update interval — a live request within that window hits
/// the pre-warmed entry instead of scoring live.
const GENRE_PREWARM_TTL_MINS: i64 = 70;

impl RecommendationEngine {
    /// Get personalized enhanced feed for a user.
    ///
    /// Optimized by Niko Matsakis (async) + Andrew Gallant (parallel performance).
    /// Scoring: rayon parallel via `score_candidates` (RS-15 semaphore + spawn_blocking).
    /// FoF: `apply_cache_boosts` — see `boost.rs` for signal hierarchy.
    #[instrument(skip(self), fields(address = %user_address, limit, offset))]
    pub async fn get_enhanced_feed(
        &self,
        user_address: &str,
        limit: usize,
        offset: usize,
        contract_type_filter: Option<&str>,
    ) -> Result<Vec<ScoredNft>> {
        use crate::recommendation::metrics::PerformanceTimer;
        let _timer = PerformanceTimer::new("get_enhanced_feed");

        let prefs = crate::recommendation::preferences::get_or_create_preferences(
            self.store.pool(),
            self.store.cache_ref(),
            user_address,
        )
        .await?;

        // Andrew Gallant: Fetch more candidates for better diversity filtering
        let fetch_multiplier = if limit < 20 { 5 } else { 3 };
        let candidates = self
            .get_candidates(contract_type_filter, limit * fetch_multiplier, offset)
            .await?;
        let candidates = self.union_affinity_candidates(candidates, &prefs).await;

        let not_interested_ids = self
            .get_not_interested_nft_ids(user_address, &candidates)
            .await
            .unwrap_or_else(|e| {
                warn!("get_not_interested_nft_ids failed for {user_address} — filter bypassed: {e}");
                Default::default()
            });
        let candidates = filter_not_interested(candidates, &not_interested_ids);

        // A-03: semaphore-bounded blocking rayon pass.
        let (session_tag_boosts, session_creator_boosts) =
            self.load_session_boosts(user_address).await;
        let session = self.begin_session(prefs, session_tag_boosts, session_creator_boosts);
        let mut scored = self.score_candidates(session, candidates).await?;

        // EFF-001: FoF + topic-affinity boosts.
        if let Some(cache) = self.store.cache_ref() {
            apply_cache_boosts(&mut scored, cache, user_address, self.graph_client.as_ref()).await;
        }

        let result = apply_diversity_shuffle_static(scored, limit);

        debug!(
            "Generated {} recommendations for user {} (parallel scoring)",
            result.len(),
            user_address
        );

        self.spawn_feedback_write(user_address, &result);

        Ok(result)
    }

    /// Get personalized recommendations for a user.
    /// Main method called by the Elixir GraphQL API.
    ///
    /// `genre_slugs` (GENRE-02): optional per-request genre boost — a
    /// separate, ephemeral mechanism from the persisted `genre_preference`
    /// edges `apply_cache_boosts` reads (see `boost::apply_cache_boosts`).
    /// This is "show me content matching THESE genres right now"; that is
    /// "boost content matching MY DECLARED favorite genres" on every feed.
    ///
    /// This method itself never reads or writes the Redis/PG feed cache when
    /// `genre_slugs` is non-empty — that read/write now happens one layer up,
    /// in `get_recommendations_coalesced`, keyed by `(user_address,
    /// genre_feed_type(genre_slugs))` rather than by `user_address` alone (a
    /// genre-scoped request must never read a stale unfiltered result nor
    /// collide with a different genre combination's cached result). Call
    /// `get_recommendations_coalesced` — not this method directly — to get
    /// that caching + stampede-lock coverage for a live request.
    #[instrument(skip(self, genre_slugs), fields(address = %user_address, limit, exclude_seen))]
    pub async fn get_recommendations(
        &self,
        user_address: &str,
        limit: usize,
        contract_type_filter: Option<&str>,
        exclude_seen: bool,
        genre_slugs: &[String],
    ) -> Result<Vec<ScoredNft>> {
        let genre_filtered = !genre_slugs.is_empty();

        // Redis → PostgreSQL cache check — skipped entirely for genre-filtered
        // requests, see the doc comment above.
        if !genre_filtered {
            if let Some(cached) = self.try_get_cached(user_address, FEED_TYPE_PERSONALIZED).await? {
                if cached.len() >= limit {
                    return Ok(cached.into_iter().take(limit).collect());
                }
            }
        }

        let prefs = crate::recommendation::preferences::get_or_create_preferences(
            self.store.pool(),
            self.store.cache_ref(),
            user_address,
        )
        .await?;

        let candidates = self
            .get_candidates(contract_type_filter, limit * 4, 0)
            .await?;
        let candidates = self.union_affinity_candidates(candidates, &prefs).await;

        // GENRE-02: pull in candidates matching the requested genre_slugs from
        // outside the 30-day recency window too — not just re-rank whatever
        // happens to already be in the recency-windowed pool. Reuses the same
        // `nft_features.tags && $1` Postgres array-overlap query
        // `union_affinity_candidates` above uses for persisted tag/creator
        // affinity, rather than a parallel genre-specific query.
        //
        // GENRE-03: unlike `union_affinity_candidates`'s call (keyed by this
        // user's own top_tags/top_creators — genuinely per-user), this call
        // passes only `genre_slugs` with an empty creator list — a pure
        // function of the slug set alone, no per-user state. Every user
        // sharing a genre combination would otherwise re-run a byte-identical
        // Postgres query for it, so this goes through the shared (non-user-
        // keyed) genre-pool cache instead of `get_affinity_candidates`
        // directly — see `candidate_repository::list_genre_candidates_cached`.
        let candidates = if genre_filtered {
            let genre_hash = genre_feed_type(genre_slugs);
            let needed = candidates.len().max(20);
            let genre_affinity = match repo::list_genre_candidates_cached(
                self.store.pool(),
                self.store.cache_ref(),
                &genre_hash,
                genre_slugs,
                needed,
            )
            .await
            {
                Ok(nfts) => repo::load_features(self.store.pool(), self.store.cache_ref(), nfts)
                    .await
                    .unwrap_or_else(|e| {
                        warn!("load_features(genre pool) failed — continuing without genre-driven pull: {e}");
                        Vec::new()
                    }),
                Err(e) => {
                    warn!("list_genre_candidates_cached failed — continuing without genre-driven pull: {e}");
                    Vec::new()
                }
            };
            merge_candidates_dedup(candidates, genre_affinity)
        } else {
            candidates
        };

        // Bulk-load seen NFT IDs to avoid N+1 per-candidate queries.
        let seen_nft_ids: HashSet<Box<str>> = if exclude_seen {
            self.get_seen_nft_ids(user_address, &candidates).await?
        } else {
            HashSet::new()
        };

        let candidates: Vec<_> = if exclude_seen {
            candidates
                .into_iter()
                .filter(|(nft, _)| {
                    nft.id
                        .as_deref()
                        .map(|id| !seen_nft_ids.contains(id))
                        .unwrap_or(false)
                })
                .collect()
        } else {
            candidates
        };

        // not_interested is a permanent signal — unconditional, not gated on exclude_seen.
        let not_interested_ids = self
            .get_not_interested_nft_ids(user_address, &candidates)
            .await
            .unwrap_or_else(|e| {
                warn!("get_not_interested_nft_ids failed for {user_address} — filter bypassed: {e}");
                Default::default()
            });
        let candidates = filter_not_interested(candidates, &not_interested_ids);

        // A-03: RS-15 semaphore + spawn_blocking + ScoringSession.
        let (mut session_tag_boosts, session_creator_boosts) =
            self.load_session_boosts(user_address).await;
        // GENRE-02: fold the request-scoped genre_slugs into the same
        // session-tag-boost map `ScoringSession::score` already applies (up to
        // +0.125 score contribution per matching tag, capped) — reuses the
        // existing, tested boost-application code path instead of a parallel
        // genre-specific scorer. A requested genre gets the maximum boost
        // value (1.0) regardless of any weaker session-derived boost already
        // present for that same tag.
        for slug in genre_slugs {
            let key = slug.trim().to_lowercase();
            if key.is_empty() { continue; }
            session_tag_boosts
                .entry(key.into())
                .and_modify(|v| *v = v.max(1.0))
                .or_insert(1.0);
        }
        let session = self.begin_session(prefs, session_tag_boosts, session_creator_boosts);
        let mut scored = self.score_candidates(session, candidates).await?;

        // EFF-001: FoF + topic-affinity boosts.
        if let Some(cache) = self.store.cache_ref() {
            apply_cache_boosts(&mut scored, cache, user_address, self.graph_client.as_ref()).await;
        }

        let result = apply_diversity_shuffle_static(scored, limit);

        // Write through to Redis (fast) + PostgreSQL (durable) — skipped for
        // genre-filtered requests, see the doc comment on this method.
        if !genre_filtered {
            self.write_to_caches(user_address, FEED_TYPE_PERSONALIZED, &result, 10)
                .await;
        }

        // Emit recommendation quality metrics.
        {
            use crate::recommendation::metrics::QualityAnalyzer;
            use std::collections::HashSet;
            let unique_creators = result
                .iter()
                .map(|s| s.creator_address.as_ref())
                .collect::<HashSet<_>>()
                .len();
            let unique_tags = result
                .iter()
                .flat_map(|s| s.tags.iter().map(|t| t.as_ref()))
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

            // MET-S23-01: surface quality regressions via tracing.
            let avg_score = if total > 0 {
                result.iter().map(|s| s.score).sum::<f32>() / total as f32
            } else {
                0.0
            };
            let discovery_count = result
                .iter()
                .filter(|s| matches!(s.reason, RecommendationReason::Discovery))
                .count();
            let quality_metrics = crate::recommendation::metrics::RecommendationMetrics {
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

        self.spawn_feedback_write(user_address, &result);

        debug!(
            "Generated {} personalized recommendations for user {}",
            result.len(),
            user_address
        );

        Ok(result)
    }

    /// Union `get_affinity_candidates` into a recency-based candidate list,
    /// deduped by NFT id.
    ///
    /// `list_candidates`/`get_candidates` are recency-windowed (last 30 days) —
    /// personalization can only re-rank what's in that pool, so a user's
    /// established affinity for an older or niche tag/creator can never
    /// surface. This adds a second, affinity-driven retrieval pass and merges
    /// it in before scoring, so strong per-user preference can pull in older
    /// matches rather than only re-rank recent ones.
    async fn union_affinity_candidates(
        &self,
        candidates: Vec<(CandidateNft, Option<NftFeatures>)>,
        prefs: &UserPreferences,
    ) -> Vec<(CandidateNft, Option<NftFeatures>)> {
        let top_tags = prefs.top_tags(10);
        let top_creators = prefs.top_creators(5);
        if top_tags.is_empty() && top_creators.is_empty() {
            return candidates;
        }

        let affinity = self
            .get_affinity_candidates(&top_tags, &top_creators, candidates.len().max(20))
            .await
            .unwrap_or_else(|e| {
                warn!("get_affinity_candidates failed — continuing with recency-only pool: {e}");
                Vec::new()
            });

        merge_candidates_dedup(candidates, affinity)
    }

    /// Get feed from followed users only.
    ///
    /// Scoring: `FollowingScoring` (recency × 0.7 + engagement × 0.3).
    #[instrument(skip(self), fields(address = %user_address, limit, offset))]
    pub async fn get_following_feed(
        &self,
        user_address: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<ScoredNft>> {
        // Get list of addresses this user follows (Redis → DB fallback).
        let following = if let Some(cache) = self.store.cache_ref() {
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

        // Get NFTs from followed creators and batch-load features in one pass.
        let nfts = self
            .get_nfts_from_creators(&following, limit, offset)
            .await?;
        let candidates =
            repo::load_features(self.store.pool(), self.store.cache_ref(), nfts).await?;

        // not_interested must be respected in all feed paths (previously missing).
        let not_interested_ids = self
            .get_not_interested_nft_ids(user_address, &candidates)
            .await
            .unwrap_or_else(|e| {
                warn!("get_not_interested_nft_ids failed for {user_address} — filter bypassed: {e}");
                Default::default()
            });
        let candidates = filter_not_interested(candidates, &not_interested_ids);

        let prefs = crate::recommendation::preferences::get_or_create_preferences(
            self.store.pool(),
            self.store.cache_ref(),
            user_address,
        )
        .await?;
        let (session_tag_boosts, session_creator_boosts) =
            self.load_session_boosts(user_address).await;
        let session = self.begin_session_with_strategy(prefs, FollowingScoring, session_tag_boosts, session_creator_boosts);
        let mut scored = self.score_candidates(session, candidates).await?;

        if let Some(cache) = self.store.cache_ref() {
            apply_cache_boosts(&mut scored, cache, user_address, self.graph_client.as_ref()).await;
        }

        let scored = apply_diversity_shuffle_static(scored, limit);
        self.spawn_feedback_write(user_address, &scored);

        Ok(scored)
    }

    // ── Stampede-coalesced public entry points ────────────────────────────────

    /// Stampede-coalescing cache-or-compute for feed requests.
    ///
    /// Thin wrapper: builds closures that capture `self.pool` + `self.cache` by
    /// clone, then delegates to `StampedeCoalescer::run`. The coalescer owns the
    /// double-checked-lock protocol and is testable without a live pool.
    ///
    /// `min_cached`     — minimum cached length to accept as a hit.
    ///   Non-paginated: `limit`. Paginated: `offset + 1`.
    /// `slice_skip`/`slice_take` — applied only to cached results.
    /// `cache_ttl_mins` — `Some(n)` writes through after compute; `None` when
    ///   the compute closure writes through itself.
    /// `feed_type` — `Arc<str>` (not `&'static str`) so a genre-filtered
    ///   caller can pass a dynamically-hashed feed type (see
    ///   `genre_feed_type`) alongside the fixed `FEED_TYPE_*` constants —
    ///   both are cheap to convert via `.into()`. Owned (rather than
    ///   borrowed) because it must outlive this call inside the `'static`
    ///   closures handed to `StampedeCoalescer::run`.
    async fn coalesced_cached<F, Fut, Hit, Miss>(
        &self,
        user_address: &str,
        feed_type: Arc<str>,
        lock_key: Box<str>,
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
        let store      = self.store.clone();
        let store2     = store.clone();
        let addr: Arc<str> = user_address.into();
        let addr2      = addr.clone();
        let feed_type2 = feed_type.clone();

        self.coalescer.run(
            lock_key,
            min_cached,
            slice_skip,
            slice_take,
            on_hit,
            on_miss,
            move || {
                let store = store.clone();
                let addr  = addr.clone();
                let feed_type = feed_type.clone();
                Box::pin(async move {
                    try_get_cached_free(store.cache_ref(), store.pool(), &addr, &feed_type).await
                }) as BoxFuture<'static, Result<Option<Vec<ScoredNft>>>>
            },
            compute,
            move |items| {
                Box::pin(async move {
                    if let Some(ttl) = cache_ttl_mins {
                        write_to_caches_free(store2.cache_ref(), store2.pool(), &addr2, &feed_type2, &items, ttl).await;
                    }
                })
            },
        ).await
    }

    /// Like `get_recommendations` but coalesces concurrent requests for the
    /// same cache key so only one scoring pass runs at a time (stampede
    /// guard) — covers BOTH the unfiltered feed and, as of GENRE-02
    /// follow-up, genre-filtered requests.
    ///
    /// Unfiltered (`genre_slugs` empty): cache key is `user_address` alone,
    /// `FEED_TYPE_PERSONALIZED`, no explicit TTL here — `get_recommendations`
    /// writes through internally for this path (unchanged behavior).
    ///
    /// Genre-filtered (`genre_slugs` non-empty): cache key folds in
    /// `genre_feed_type(genre_slugs)` — a deterministic, order/case-insensitive
    /// hash of the slug set — into both the Redis/PG cache key AND the
    /// stampede lock key, so (a) two different slug sets for the same user
    /// cache independently rather than colliding, and (b) concurrent requests
    /// for a *different* slug set don't serialize behind each other's lock.
    /// Uses `GENRE_ONDEMAND_TTL_MINS` (short — see that constant's doc for
    /// why a short self-healing TTL was chosen over exhaustive
    /// per-combination invalidation tracking).
    // RS-08: instrument the production stampede-guarded entry point.
    #[instrument(skip(self, genre_slugs), fields(address = %user_address, limit, exclude_seen))]
    pub async fn get_recommendations_coalesced(
        &self,
        user_address: &str,
        limit: usize,
        contract_type_filter: Option<&str>,
        exclude_seen: bool,
        genre_slugs: &[String],
    ) -> Result<Vec<ScoredNft>> {
        let genre_filtered = !genre_slugs.is_empty();

        let (feed_type, lock_key, cache_ttl_mins): (Arc<str>, Box<str>, Option<i64>) = if genre_filtered {
            let ft = genre_feed_type(genre_slugs);
            // Namespaced lock key: same address, different genre combos must
            // not serialize behind one another's stampede lock.
            let lk: Box<str> = format!("{}:{}", user_address.to_lowercase(), ft).into();
            (ft.into(), lk, Some(GENRE_ONDEMAND_TTL_MINS))
        } else {
            (FEED_TYPE_PERSONALIZED.into(), user_address.into(), None)
        };

        self.coalesced_cached(
            user_address,
            feed_type,
            lock_key,
            limit,   // min_cached: need full page before serving from cache
            0,       // slice_skip: neither feed has an offset
            limit,   // slice_take
            || {
                if genre_filtered {
                    metrics::counter!("rec_genre_feed_cache_hits_total").increment(1);
                }
            },
            || {
                if genre_filtered {
                    metrics::counter!("rec_genre_feed_cache_misses_total").increment(1);
                }
            },
            || self.get_recommendations(user_address, limit, contract_type_filter, exclude_seen, genre_slugs),
            cache_ttl_mins,
        ).await
    }

    /// `get_enhanced_feed` with a cache layer and stampede coalescing.
    ///
    /// Check cache first (offset-aware), compute on miss, write through, return.
    pub async fn get_enhanced_feed_cached(
        &self,
        user_address: &str,
        limit: usize,
        offset: usize,
        contract_type_filter: Option<&str>,
    ) -> Result<Vec<ScoredNft>> {
        // Cache key omits offset — only safe for offset=0 callers. All current
        // production callers use offset=0.
        debug_assert_eq!(offset, 0, "get_enhanced_feed_cached cache key does not include offset; non-zero offset pollutes page-1 cache");

        // Accept partial cache hits: `offset + 1` means "any item after `offset` is enough".
        // The prior `>= offset + limit` caused a permanent miss loop for single-content-type
        // users capped by diversity shuffle (6× DB load).
        self.coalesced_cached(
            user_address,
            FEED_TYPE_ENHANCED.into(),
            format!("ef:{user_address}").into(),   // namespaced to avoid locking against personalized
            offset + 1,                     // min_cached
            offset,                         // slice_skip
            limit,                          // slice_take
            || metrics::counter!("rec_enhanced_feed_cache_hits_total").increment(1),
            || metrics::counter!("rec_enhanced_feed_cache_misses_total").increment(1),
            || self.get_enhanced_feed(user_address, limit, offset, contract_type_filter),
            Some(5),                        // 5-min TTL for on-demand entries
        ).await
    }

    /// Pre-warm the enhanced feed cache.
    ///
    /// Writes a 70-min TTL (vs the 5-min TTL for on-demand requests) so cold-start
    /// recomputes only happen when the background job hasn't run recently.
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

    /// Pre-warm a user's PRIMARY genre-filtered feed — GENRE-02 follow-up
    /// sibling to `warmup_enhanced_feed`.
    ///
    /// "Primary" means the feed scored against the user's current declared
    /// `genre_preference` favorites (`graph_client::get_genre_preferences`) —
    /// the same slug set the "For You" page's genre-filtered request is
    /// expected to send, so this pre-warm lands in the exact cache entry that
    /// request will read. Deliberately NOT extended to pre-compute every
    /// possible single-genre "radio" combination (`useGenreRadio.ts`) for
    /// every user — that's covered adequately by `get_recommendations_coalesced`'s
    /// cache-aside-on-first-request plus `GENRE_ONDEMAND_TTL_MINS`, and
    /// batch-pre-computing 30 radios × every user would be wasted compute for
    /// content most users never touch (same "don't pre-compute what most
    /// users won't use" rationale as the Redis-memory note on
    /// `candidate_repository::load_features`).
    ///
    /// No-op (`Ok(())`, no scoring pass) when the user has no `graph_client`
    /// configured or no `genre_preference` edges set — avoids wasted work for
    /// users who never declared genre favorites.
    ///
    /// Writes a `GENRE_PREWARM_TTL_MINS`-min PG TTL — longer than the
    /// on-demand path's `GENRE_ONDEMAND_TTL_MINS`, mirroring
    /// `warmup_enhanced_feed`'s 70-vs-5-min split — so the pre-warmed entry
    /// survives the full hourly `update_all_recommendations` interval.
    #[instrument(skip(self), fields(address = %user_address))]
    pub async fn warmup_genre_feed(&self, user_address: &str) -> Result<()> {
        let Some(ref graph) = self.graph_client else {
            return Ok(());
        };
        let genre_prefs = graph.get_genre_preferences(user_address).await;
        if genre_prefs.is_empty() {
            return Ok(());
        }
        let genre_slugs: Vec<String> = genre_prefs.keys().map(|s| s.to_string()).collect();

        let items = self
            .get_recommendations(user_address, 50, None, true, &genre_slugs)
            .await?;
        let feed_type = genre_feed_type(&genre_slugs);
        self.write_to_caches(user_address, &feed_type, &items, GENRE_PREWARM_TTL_MINS)
            .await;
        Ok(())
    }
}

/// Merge `extra` into `base`, skipping any item whose NFT id already appears
/// in `base` (or in an earlier-kept `extra` item). id-less items are always
/// kept (never deduped against, matching the pre-extraction behavior in
/// `union_affinity_candidates`).
///
/// Shared by `union_affinity_candidates` (persisted tag/creator affinity, from
/// `UserPreferences`) and `get_recommendations`'s genre_slugs retrieval pass
/// (GENRE-02, request-scoped) — both need the same "pull in extra candidates
/// from outside the recency window, dedup by id" shape.
fn merge_candidates_dedup(
    base: Vec<(CandidateNft, Option<NftFeatures>)>,
    extra: Vec<(CandidateNft, Option<NftFeatures>)>,
) -> Vec<(CandidateNft, Option<NftFeatures>)> {
    let mut seen_ids: HashSet<String> = base
        .iter()
        .filter_map(|(nft, _)| nft.id.clone())
        .collect();
    let mut merged = base;
    for item in extra {
        let keep = match &item.0.id {
            Some(id) => seen_ids.insert(id.clone()),
            None => true,
        };
        if keep {
            merged.push(item);
        }
    }
    merged
}

// Suppress unused import warning for _RecCache type alias pulled in for clarity.
const _: () = {
    let _ = std::marker::PhantomData::<_RecCache>;
};
