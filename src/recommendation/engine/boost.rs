//! FoF and topic-affinity boost helpers for the recommendation engine.
//!
//! Free functions here are shared by all three feed paths
//! (`get_enhanced_feed`, `get_recommendations`, `get_following_feed`)
//! and live in one place so any tuning change touches all three.

use std::collections::HashMap;
use std::sync::Arc;

use sqlx::PgPool;
use crate::recommendation::cache::{rec_dirty_debounce_secs, RecCache};
use crate::recommendation::graph_client::{FofBucket, GraphTraversal};
use crate::recommendation::candidate_repository::{self as repo, CandidateNft};
use crate::recommendation::features::NftFeatures;
use crate::recommendation::scoring::{nan_safe_sort_desc, ScoredNft};

// ── FoF boost weights ─────────────────────────────────────────────────────────

/// Follow-based FoF boost weight (follows → liked content).
const FOF_FOLLOW_WEIGHT:   f32 = 0.10;
/// View-event FoF boost weight (follows → viewed content).
const FOF_VIEW_WEIGHT:     f32 = 0.05;
/// Comment FoF boost weight (follows → commented content).
const FOF_COMMENT_WEIGHT:  f32 = 0.05;
/// Purchase FoF boost weight (follows → purchased content, 30-day half-life).
/// Highest of the FoF weights: purchase is a 1000 THERA economic commitment.
const FOF_PURCHASE_WEIGHT: f32 = 0.15;
/// Share FoF boost weight (follows → shared content, 15-day half-life).
const FOF_SHARE_WEIGHT:    f32 = 0.12;
/// Bookmark FoF boost weight (follows → bookmarked content, 10-day half-life).
const FOF_BOOKMARK_WEIGHT: f32 = 0.08;
/// Vid-watch FoF boost weight (follows → vid watched, 14-day half-life).
/// Stronger than view_event (passive scroll) and bookmark; equal to purchase
/// in social signal quality — deliberate completion signals peer taste.
const FOF_VID_WEIGHT:     f32 = 0.08;

/// Companion (AiFriendz chat) style-preference boost weight.
/// A style mentioned in passing conversation is a real but weaker signal than
/// an explicit like/purchase (which is what drives tag_preferences directly) —
/// weighted at the same tier as a single FoF signal, not as strong as an
/// established, reinforced tag preference.
const STYLE_PREF_WEIGHT:  f32 = 0.10;

/// Declared genre-preference boost weight (`genre_preference` edges, up to 30
/// slugs — see `write_genre_preference_edges`). Same tier as
/// `STYLE_PREF_WEIGHT`: both are explicit, low-frequency-write preference
/// signals rather than derived-from-behavior FoF signals.
const GENRE_PREF_WEIGHT: f32 = 0.10;

// ── Boost application ─────────────────────────────────────────────────────────

/// Accumulate FoF scores from one cache bucket into the boost map.
///
/// Caps each raw score at 1.0 before applying the weight — Nebula returns
/// power-law distributed composite scores (e.g. 200.0 for viral posts), so
/// uncapped values would clamp every top item to 1.0 and destroy rank ordering.
#[inline]
fn accumulate_fof(map: &mut HashMap<Box<str>, f32>, scores: Option<Box<[(Box<str>, f32)]>>, weight: f32) {
    if let Some(scores) = scores {
        for (id, s) in scores {
            *map.entry(id).or_insert(0.0) += weight * s.min(1.0);
        }
    }
}

/// EFF-001: Apply FoF signal boosts and topic-affinity boosts to a scored list.
///
/// Concurrently reads seven Redis FoF caches via `tokio::join!`, merges them into
/// one additive boost map, applies in a single O(n) pass, then applies
/// topic-affinity boosts from board-dwell signals written by Elixir.
///
/// ByteGraph signal hierarchy (highest to lowest weight):
///   purchase (0.15) → share (0.12) → follow-like (0.10) → vid-watch/bookmark (0.08) → comment/view (0.05)
pub(crate) async fn apply_cache_boosts(
    scored: &mut Vec<ScoredNft>,
    cache: &RecCache,
    user_address: &str,
    graph: Option<&Arc<dyn GraphTraversal>>,
) {
    let (fof_opt, view_opt, comment_opt, purchase_opt, share_opt, bookmark_opt, vid_opt) = tokio::join!(
        cache.get_fof_recs(FofBucket::FollowLike, user_address),
        cache.get_fof_recs(FofBucket::ViewEvent,  user_address),
        cache.get_fof_recs(FofBucket::Comment,    user_address),
        cache.get_fof_recs(FofBucket::Purchase,   user_address),
        cache.get_fof_recs(FofBucket::Share,      user_address),
        cache.get_fof_recs(FofBucket::Bookmark,   user_address),
        cache.get_fof_recs(FofBucket::VidWatch,  user_address),
    );

    let mut boost_map: HashMap<Box<str>, f32> = HashMap::new();
    accumulate_fof(&mut boost_map, fof_opt,      FOF_FOLLOW_WEIGHT);
    accumulate_fof(&mut boost_map, view_opt,     FOF_VIEW_WEIGHT);
    accumulate_fof(&mut boost_map, comment_opt,  FOF_COMMENT_WEIGHT);
    accumulate_fof(&mut boost_map, purchase_opt, FOF_PURCHASE_WEIGHT);
    accumulate_fof(&mut boost_map, share_opt,    FOF_SHARE_WEIGHT);
    accumulate_fof(&mut boost_map, bookmark_opt, FOF_BOOKMARK_WEIGHT);
    accumulate_fof(&mut boost_map, vid_opt,     FOF_VID_WEIGHT);

    if !boost_map.is_empty() {
        let mut fof_changed = false;
        for s in scored.iter_mut() {
            if let Some(&b) = boost_map.get(s.nft_id.as_ref()) {
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
            .flat_map(|s| s.tags.iter().map(|t| t.as_ref()))
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
                    .filter_map(|t| affinity_map.get(t.as_ref()))
                    .cloned()
                    .fold(0.0f32, f32::max);
                // Affinities are on a 0-10 scale; use > 0.0 so the signal fires for any
                // dwell-derived affinity, not just power users.
                if best > 0.0 {
                    s.score = (s.score + (best / 10.0).min(1.0) * 0.15).clamp(0.0, 1.0);
                }
            }
            nan_safe_sort_desc(scored);
        }
    }

    // Companion style-preference boost — chat-derived signal (see
    // graph_client::get_style_preferences), read-only here: this cache is
    // populated by the background update cycle (updater.rs), never traversed
    // live on the request path.
    let style_prefs = cache.get_style_preferences(user_address).await;
    if let Some(style_prefs) = style_prefs {
        if !style_prefs.is_empty() {
            let mut style_changed = false;
            for s in scored.iter_mut() {
                let best = s
                    .tags
                    .iter()
                    .filter_map(|t| style_prefs.get(t.as_ref()))
                    .cloned()
                    .fold(0.0f32, f32::max);
                if best > 0.0 {
                    s.score = (s.score + best.min(1.0) * STYLE_PREF_WEIGHT).clamp(0.0, 1.0);
                    style_changed = true;
                }
            }
            if style_changed {
                nan_safe_sort_desc(scored);
            }
        }
    }

    // Declared genre-preference boost (GENRE-01) — live Nebula read (self-cached
    // internally via Redis, see `GraphClient::get_genre_preferences`), not a
    // background pre-warm job: that is deliberately a separate, later pass.
    // Matches genre_preference slugs against `ScoredNft::tags` exactly the way
    // the style-preference block above matches style tags — genre now lives in
    // the same tag-slug namespace (see `event_processor::elixir_db::
    // process_enrichment` and `write_genre_preference_edges`), so no separate
    // genre-specific scoring path is needed.
    if let Some(graph) = graph {
        let genre_prefs = graph.get_genre_preferences(user_address).await;
        if !genre_prefs.is_empty() {
            let mut genre_changed = false;
            for s in scored.iter_mut() {
                let best = s
                    .tags
                    .iter()
                    .filter_map(|t| genre_prefs.get(t.as_ref()))
                    .cloned()
                    .fold(0.0f32, f32::max);
                if best > 0.0 {
                    s.score = (s.score + best.min(1.0) * GENRE_PREF_WEIGHT).clamp(0.0, 1.0);
                    genre_changed = true;
                }
            }
            if genre_changed {
                nan_safe_sort_desc(scored);
            }
        }
    }
}

/// A-03: Remove candidates the user has permanently rejected.
///
/// Pure synchronous filter — `not_interested_ids` is pre-fetched by the caller.
/// id-less (malformed) rows are dropped unconditionally.
pub(crate) fn filter_not_interested(
    candidates: Vec<(CandidateNft, Option<NftFeatures>)>,
    not_interested_ids: &std::collections::HashSet<Box<str>>,
) -> Vec<(CandidateNft, Option<NftFeatures>)> {
    if not_interested_ids.is_empty() {
        return candidates;
    }
    candidates
        .into_iter()
        .filter(|(nft, _)| {
            nft.id
                .as_deref()
                .map(|id| !not_interested_ids.contains(id))
                .unwrap_or(false)
        })
        .collect()
}

// ── Cache read/write free functions ───────────────────────────────────────────
//
// Extracted from engine methods so they can be captured as closures by
// StampedeCoalescer::run() — closures cannot borrow `self` directly.

/// Read Redis then PG. Returns `Ok(None)` on miss OR transient DB error (BUG-002).
pub(crate) async fn try_get_cached_free(
    cache: Option<&RecCache>,
    pool: &PgPool,
    user_address: &str,
    feed_type: &str,
) -> anyhow::Result<Option<Vec<ScoredNft>>> {
    if let Some(c) = cache {
        // Dirty-plus-debounce (cache/dirty.rs): an interaction only MARKS this person's feed dirty. The first read
        // after the mark is `rec_dirty_debounce_secs()` old purges both cache tiers and reports a miss, so ONE
        // recompute covers the whole burst; until then the cached feed is served. Debounce 0 = the legacy mode,
        // where interactions purge at once and there is never a mark to look for.
        let debounce = rec_dirty_debounce_secs();
        if debounce > 0 && c.take_recs_dirty_if_due(user_address, debounce).await {
            crate::recommendation::recorder::purge_user_recommendation_caches(pool, Some(c), user_address).await;
            metrics::counter!("rec_dirty_recompute_total").increment(1);
            return Ok(None);
        }
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
pub(crate) async fn write_to_caches_free(
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

#[cfg(test)]
mod dirty_read_tests {
    use super::*;
    use crate::recommendation::cache::CacheKey;
    use crate::recommendation::schema_consts::FEED_TYPE_PERSONALIZED;

    // Live (runs only with DATABASE_URL and REDIS_URL, like the other database and cache tests): an interaction only
    // marks the feed dirty; a read serves the cached feed until the mark is old enough, then purges and misses ONCE.
    #[tokio::test]
    async fn a_due_dirty_mark_purges_and_misses_once_while_a_fresh_one_still_serves_the_cache() {
        let (Ok(database_url), Ok(redis_url)) = (std::env::var("DATABASE_URL"), std::env::var("REDIS_URL")) else { return };
        if rec_dirty_debounce_secs() == 0 {
            return; // legacy mode: interactions purge at once and there is no mark to read
        }
        let pool = PgPool::connect(&database_url).await.expect("connect");
        let Some(cache) = RecCache::connect(&redis_url).await else { return };
        let addr = format!("0xdirtyread{}", &uuid::Uuid::new_v4().simple().to_string()[..16]);
        let marker = CacheKey::rec_dirty(&addr);

        // A cached (empty) feed stands for a computed one.
        cache.set_recommendations(&addr, FEED_TYPE_PERSONALIZED, &Vec::<ScoredNft>::new()).await;

        // No mark: served from the cache.
        let hit = try_get_cached_free(Some(&cache), &pool, &addr, FEED_TYPE_PERSONALIZED).await.unwrap();
        assert!(hit.is_some(), "no dirty mark: the cached feed is served");

        // A burst of interactions marks it dirty (the first mark wins); inside the debounce it is still served.
        for _ in 0..5 {
            cache.mark_recs_dirty(&addr).await;
        }
        let hit = try_get_cached_free(Some(&cache), &pool, &addr, FEED_TYPE_PERSONALIZED).await.unwrap();
        assert!(hit.is_some(), "a fresh mark is inside the debounce: still served, no recompute per event");

        // Age the mark past the debounce (write an old time directly), then read: ONE purge and miss.
        let client = redis::Client::open(redis_url.as_str()).unwrap();
        let mut conn = client.get_multiplexed_async_connection().await.unwrap();
        let old = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64 - 600;
        let _: () = redis::cmd("SET").arg(&marker).arg(old).arg("EX").arg(60).query_async(&mut conn).await.unwrap();

        let miss = try_get_cached_free(Some(&cache), &pool, &addr, FEED_TYPE_PERSONALIZED).await.unwrap();
        assert!(miss.is_none(), "a due mark purges both tiers and reports a miss: the feed is recomputed");
        let gone: Option<String> = redis::cmd("GET").arg(&marker).query_async(&mut conn).await.unwrap();
        assert!(gone.is_none(), "the mark is taken: one recompute per burst");
        let again = try_get_cached_free(Some(&cache), &pool, &addr, FEED_TYPE_PERSONALIZED).await.unwrap();
        assert!(again.is_none(), "the cache really was purged (and nothing re-marks it)");
    }
}

