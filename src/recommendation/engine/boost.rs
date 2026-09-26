//! FoF and topic-affinity boost helpers for the recommendation engine.
//!
//! Free functions here are shared by all three feed paths
//! (`get_enhanced_feed`, `get_recommendations`, `get_following_feed`)
//! and live in one place so any tuning change touches all three.

use std::collections::HashMap;
use std::sync::Arc;

use sqlx::PgPool;
use crate::recommendation::cache::RecCache;
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
/// Flix-watch FoF boost weight (follows → flix watched, 14-day half-life).
/// Stronger than view_event (passive scroll) and bookmark; equal to purchase
/// in social signal quality — deliberate completion signals peer taste.
const FOF_FLIX_WEIGHT:     f32 = 0.08;

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
///   purchase (0.15) → share (0.12) → follow-like (0.10) → flix-watch/bookmark (0.08) → comment/view (0.05)
pub(crate) async fn apply_cache_boosts(
    scored: &mut Vec<ScoredNft>,
    cache: &RecCache,
    user_address: &str,
    graph: Option<&Arc<dyn GraphTraversal>>,
) {
    let (fof_opt, view_opt, comment_opt, purchase_opt, share_opt, bookmark_opt, flix_opt) = tokio::join!(
        cache.get_fof_recs(FofBucket::FollowLike, user_address),
        cache.get_fof_recs(FofBucket::ViewEvent,  user_address),
        cache.get_fof_recs(FofBucket::Comment,    user_address),
        cache.get_fof_recs(FofBucket::Purchase,   user_address),
        cache.get_fof_recs(FofBucket::Share,      user_address),
        cache.get_fof_recs(FofBucket::Bookmark,   user_address),
        cache.get_fof_recs(FofBucket::FlixWatch,  user_address),
    );

    let mut boost_map: HashMap<Box<str>, f32> = HashMap::new();
    accumulate_fof(&mut boost_map, fof_opt,      FOF_FOLLOW_WEIGHT);
    accumulate_fof(&mut boost_map, view_opt,     FOF_VIEW_WEIGHT);
    accumulate_fof(&mut boost_map, comment_opt,  FOF_COMMENT_WEIGHT);
    accumulate_fof(&mut boost_map, purchase_opt, FOF_PURCHASE_WEIGHT);
    accumulate_fof(&mut boost_map, share_opt,    FOF_SHARE_WEIGHT);
    accumulate_fof(&mut boost_map, bookmark_opt, FOF_BOOKMARK_WEIGHT);
    accumulate_fof(&mut boost_map, flix_opt,     FOF_FLIX_WEIGHT);

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
