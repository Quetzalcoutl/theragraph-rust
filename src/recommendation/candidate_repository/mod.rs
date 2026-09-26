//! Candidate Repository
//!
//! All SQL for fetching and filtering NFT candidates lives here.
//! The engine imports these free functions and delegates — no SQL in engine.rs.
//!
//! Seam: callers pass `pool` + `cache` references; schema changes only require
//! edits to this file.

use anyhow::Result;
use sqlx::PgPool;
use std::collections::{HashMap, HashSet};
use tracing::warn;
use uuid::Uuid;

use super::cache::RecCache;
use super::features::{NftFeatures, ScoringFeatures};
use super::scoring::ScoredNft;
use super::types::ContentType;

use serde_json;

/// Raw NFT row returned from candidate queries.
// GENRE-03: Serialize/Deserialize so a Vec<CandidateNft> can round-trip
// through the shared genre-pool Redis cache (`RecCache::get_genre_pool`/
// `set_genre_pool`) — a plain data struct, adding derives changes nothing
// about its DB-row semantics.
//
// `token_id` is `Option<i64>` because `nfts.token_id` itself IS nullable in
// Postgres — lazy-mint economy: a freshly-posted, not-yet-purchased draft has
// no on-chain token yet. Fixed 2026-09-14: this used to be `token_id: i64`
// with every query below filtering `token_id IS NOT NULL`, on the reasoning
// that "a draft has nothing to recommend/collect" — true under the OLD
// immediate-mint model, but backwards under lazy-mint: a *listed* draft
// (has a voucher/price) is exactly the thing a first buyer needs to discover
// in order to trigger its first mint. That filter meant NO freshly-created
// content, of any kind, could ever appear in feed/recommendations — a
// complete, silent breakage of "give it its first buy." Downstream consumers
// (`ScoredNft.token_id: i64`, `NftFeatures.token_id: i64`) convert this back
// to a plain `i64` via `.unwrap_or(0)` — `0` is the same "draft, not yet
// minted" sentinel the frontend's `isDraftNft(tokenId)` already uses
// everywhere (TheraFriendz.sol's `_nextTokenId` pre-increments from 0, so a
// real on-chain token id is never 0) — no API/wire contract change needed.
#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize)]
pub struct CandidateNft {
    pub id: Option<String>,
    pub token_id: Option<i64>,
    pub contract_address: String,
    pub contract_type: Option<String>,
    pub creator_address: String,
    pub created_at: Option<String>,
}

// ── Candidate sets ────────────────────────────────────────────────────────────

/// Fetch raw NFT rows — no feature loading.
///
/// Callers that paginate or that want to reuse a feature map across multiple
/// pages should call this + [`load_features`] separately rather than
/// using the combined [`get_candidates`] wrapper.
pub async fn list_candidates(
    pool: &PgPool,
    contract_type_filter: Option<&str>,
    limit: usize,
    offset: usize,
) -> Result<Vec<CandidateNft>> {
    // Validate and normalise the filter before building the query so the callers
    // never need to lowercase themselves.
    let ct_lower = contract_type_filter.map(|ct| {
        if ContentType::from_str(ct).is_none() {
            return Err(anyhow::anyhow!("Invalid contract_type: {}", ct));
        }
        Ok(ct.to_lowercase())
    }).transpose()?;

    // Both branches share the same SELECT projection, freshness window, and ORDER BY.
    // The only difference is the optional `AND contract_type = $1` predicate.
    // Using a single query with a nullable bind avoids duplicating 7 SQL lines.
    Ok(sqlx::query_as::<_, CandidateNft>(
        r#"
        SELECT id::text, token_id, contract_address, contract_type::text,
               LOWER(creator_address) AS creator_address,
               COALESCE(creation_time, inserted_at)::text as created_at
        FROM nfts
        WHERE is_deleted = false AND is_original = true AND is_blocked = false
          AND ($1::text IS NULL OR contract_type::text = $1)
          AND COALESCE(creation_time, inserted_at) > NOW() - INTERVAL '30 days'
        ORDER BY COALESCE(creation_time, inserted_at) DESC, CAST(token_id AS BIGINT) % 997
        LIMIT $2 OFFSET $3
        "#,
    )
    .bind(ct_lower.as_deref())
    .bind(limit as i64)
    .bind(offset as i64)
    .fetch_all(pool)
    .await?)
}

/// Batch-load NftFeatures for a slice of candidates — single Redis MGET + one DB query for misses.
///
/// Previous implementation fired N concurrent individual GET/SELECT calls (one per NFT).
/// This version collapses the Redis tier to one MGET round-trip and the DB tier to a
/// single `WHERE nft_id = ANY($1)` query, reducing latency by ~N× on large candidate sets.
///
/// Returns pairs preserving the input order. Candidates with no `id` are skipped.
pub async fn load_features(
    pool: &PgPool,
    cache: Option<&RecCache>,
    nfts: Vec<CandidateNft>,
) -> Result<Vec<(CandidateNft, Option<NftFeatures>)>> {
    // Collect all NFT IDs that have valid UUIDs.
    let nft_id_strs: Vec<&str> = nfts
        .iter()
        .filter_map(|nft| nft.id.as_deref())
        .collect();

    // ── Redis tier: single MGET round-trip (slim ScoringFeatures, ~100B) ─
    // C3: cache the 4-field ScoringFeatures projection, not the 11-field NftFeatures.
    // The scoring hot path uses only tags/engagement/trending/quality; storing the
    // full NftFeatures (primary_color, style, mood, genre, etc.) wastes ~2.75× Redis
    // memory and forces a ScoringFeatures::from() allocation on every feed request.
    let mut scoring_map: HashMap<String, ScoringFeatures> = if let Some(c) = cache {
        c.mget_nft_features::<ScoringFeatures>(&nft_id_strs).await
    } else {
        HashMap::with_capacity(nft_id_strs.len())
    };

    // We still need NftFeatures from the DB for cache misses (to derive and store
    // ScoringFeatures).  Full NftFeatures are never stored in Redis — only used here
    // to produce the ScoringFeatures projection.
    let miss_uuids: Vec<Uuid> = nft_id_strs
        .iter()
        .filter(|id| !scoring_map.contains_key(**id))
        .filter_map(|id| Uuid::parse_str(id).ok())
        .collect();

    if !miss_uuids.is_empty() {
        match super::features::get_features_batch(pool, &miss_uuids).await {
            Ok(rows) => {
                for feat in rows {
                    // Narrow to ScoringFeatures before caching — keeps Redis entries small.
                    let slim = ScoringFeatures::from(&feat);
                    if let Some(c) = cache {
                        // Reuse the existing cache key (features:{nft_id}); format changed
                        // from NftFeatures JSON to ScoringFeatures JSON.  Old cached entries
                        // in a different format will deserialise as None (mget_nft_features
                        // silently drops deserialisation failures) — they expire naturally.
                        c.set_nft_features(&feat.nft_id, &slim).await;
                    }
                    scoring_map.insert(feat.nft_id.clone(), slim);
                }
            }
            Err(e) => warn!("Batch features DB query failed: {e:?}"),
        }
    }

    // Reconstruct the (CandidateNft, Option<NftFeatures>) output type.  Callers that
    // need full NftFeatures (e.g. enrichment for metadata display) already have the
    // CandidateNft and can fetch from DB independently.  The scoring engine only ever
    // converts Option<NftFeatures> → Option<ScoringFeatures> immediately after — that
    // conversion now happens here, at the cache boundary, rather than at lines 397/602/905.
    let mut results = Vec::with_capacity(nfts.len());
    for nft in nfts {
        // We return Option<NftFeatures> to preserve the existing public type signature.
        // Internally we build a minimal NftFeatures from ScoringFeatures so the engine's
        // ScoringFeatures::from() call is a zero-copy identity (all scoring fields present).
        let features: Option<NftFeatures> = nft.id.as_deref().and_then(|id| {
            scoring_map.remove(id).map(|sf| NftFeatures {
                nft_id:           id.to_string(),
                contract_address: nft.contract_address.clone(),
                // In practice this branch only runs when a `nft_features` row
                // already exists for this NFT, which only happens once it's
                // been through feature extraction (post-mint) — token_id is
                // expected to always be Some(_) here. unwrap_or(0) is a type-
                // level safety net using the same draft sentinel as ScoredNft.
                token_id:         nft.token_id.unwrap_or(0),
                tags:             sf.tags.iter().map(|s| s.to_string()).collect(),
                primary_color:    None,
                style:            None,
                mood:             None,
                genre:            None,
                engagement_score: sf.engagement_score,
                trending_score:   sf.trending_score,
                quality_score:    sf.quality_score,
            })
        });
        results.push((nft, features));
    }
    Ok(results)
}

/// Fetch NFT candidates matching a user's strong tag/creator affinities,
/// without the recency window `list_candidates` applies.
///
/// `list_candidates` only returns NFTs from the last 30 days — a user's
/// established affinity for an older or niche tag/creator can never surface
/// through it, because the item is filtered out at retrieval, before scoring
/// ever runs. This is a second retrieval path meant to be unioned with the
/// recency pool at the call site so strong per-user affinity can pull in
/// older matches instead of only re-ranking whatever is already recent.
///
/// Empty `top_tags` and `top_creators` returns an empty vec without querying —
/// callers should skip this entirely for a user with no established affinity
/// yet (e.g. a fresh cold-start user) rather than pay for a query that can
/// only return nothing.
pub async fn list_affinity_candidates(
    pool: &PgPool,
    top_tags: &[String],
    top_creators: &[String],
    limit: usize,
) -> Result<Vec<CandidateNft>> {
    if top_tags.is_empty() && top_creators.is_empty() {
        return Ok(vec![]);
    }

    let creators_lower: Vec<String> = top_creators.iter().map(|c| c.to_lowercase()).collect();

    // Empty-array `&&`/`= ANY` both correctly evaluate to false for every row,
    // so passing either side empty (when the other is non-empty) needs no
    // special-casing beyond the early-return above.
    Ok(sqlx::query_as::<_, CandidateNft>(
        r#"
        SELECT n.id::text, n.token_id, n.contract_address, n.contract_type::text,
               LOWER(n.creator_address) AS creator_address,
               COALESCE(n.creation_time, n.inserted_at)::text as created_at
        FROM nfts n
        JOIN nft_features f ON f.nft_id = n.id
        WHERE n.is_deleted = false AND n.is_original = true AND n.is_blocked = false
          AND (f.tags && $1 OR LOWER(n.creator_address) = ANY($2))
        ORDER BY COALESCE(n.creation_time, n.inserted_at) DESC
        LIMIT $3
        "#,
    )
    .bind(top_tags)
    .bind(&creators_lower)
    .bind(limit as i64)
    .fetch_all(pool)
    .await?)
}

/// Combined fetch: [`list_affinity_candidates`] then load features in one call.
pub async fn get_affinity_candidates(
    pool: &PgPool,
    cache: Option<&RecCache>,
    top_tags: &[String],
    top_creators: &[String],
    limit: usize,
) -> Result<Vec<(CandidateNft, Option<NftFeatures>)>> {
    let nfts = list_affinity_candidates(pool, top_tags, top_creators, limit).await?;
    load_features(pool, cache, nfts).await
}

/// Minimum shared pool size to actually fetch/cache for a genre-slug set —
/// GENRE-03. Rounds a small `needed` up to a size worth sharing across users
/// (the common `warmup_genre_feed`/default-limit request shape), so the
/// cached pool tends to be reusable rather than sized to whatever the first
/// caller happened to ask for. Pure and separately tested — see
/// `genre_pool_fetch_size` in tests.
const GENRE_POOL_SIZE: usize = 300;

/// How large a Postgres query to actually run when (re)computing the shared
/// genre pool for a request that needs `needed` items — always at least
/// `GENRE_POOL_SIZE` so the cached entry is generously sized for reuse by the
/// next caller, but never less than what the current caller needs.
fn genre_pool_fetch_size(needed: usize) -> usize {
    needed.max(GENRE_POOL_SIZE)
}

/// Fetch NFT candidates matching a genre-slug set, shared (not user-keyed)
/// across every user requesting the same genre combination — GENRE-03
/// follow-up to `get_affinity_candidates`.
///
/// `list_affinity_candidates(genre_slugs, &[], ..)` is a pure function of
/// `(genre_slugs, limit)` — it binds no per-user state (no `top_creators`, no
/// `user_address` anywhere in the SQL). Every user who shares a genre
/// combination (declared favorites, or a single "radio" slug) triggers a
/// byte-identical Postgres JOIN + array-overlap query; the hourly
/// `warmup_genre_feed` pre-warm pass alone reruns it once per active user
/// *per genre combination*, every hour. Caching the raw `Vec<CandidateNft>`
/// once per genre-hash (rather than recomputing it once per user) collapses
/// that fan-out to a single shared Postgres round trip per `GENRE_POOL_TTL`
/// window. Every per-user personalization layer downstream (prefs affinity,
/// session boosts, FoF/topic boosts, seen/not-interested filtering, scoring,
/// diversity shuffle) is untouched by this — it only changes how the shared
/// *candidate pool* is retrieved, not how it's ranked or filtered per user.
///
/// `genre_hash` is the caller's already-computed `genre_feed_type(genre_slugs)`
/// (callers already need this for the per-user results cache key, so it's
/// passed in rather than recomputed here).
///
/// `needed` is the caller's requested pool size. A cache hit shorter than
/// `needed` is treated as a miss — it falls through to a live Postgres query
/// sized at `needed` (identical to pre-cache behavior), rather than silently
/// starving a larger request. This means requests larger than
/// `GENRE_POOL_SIZE` are never slower than before caching existed; only the
/// common case (`needed <= GENRE_POOL_SIZE`) benefits.
pub async fn list_genre_candidates_cached(
    pool: &PgPool,
    cache: Option<&RecCache>,
    genre_hash: &str,
    genre_slugs: &[String],
    needed: usize,
) -> Result<Vec<CandidateNft>> {
    if let Some(c) = cache {
        if let Some(cached) = c.get_genre_pool::<Vec<CandidateNft>>(genre_hash).await {
            if cached.len() >= needed {
                return Ok(cached.into_iter().take(needed).collect());
            }
        }
    }

    let fetch_size = genre_pool_fetch_size(needed);
    let fresh = list_affinity_candidates(pool, genre_slugs, &[], fetch_size).await?;

    if let Some(c) = cache {
        c.set_genre_pool(genre_hash, &fresh).await;
    }

    Ok(fresh.into_iter().take(needed).collect())
}

/// Combined fetch: list candidates then load features in one call.
///
/// Convenience wrapper around [`list_candidates`] + [`load_features`].
/// Use the split form when paginating or reusing a feature map.
pub async fn get_candidates(
    pool: &PgPool,
    cache: Option<&RecCache>,
    contract_type_filter: Option<&str>,
    limit: usize,
    offset: usize,
) -> Result<Vec<(CandidateNft, Option<NftFeatures>)>> {
    let nfts = list_candidates(pool, contract_type_filter, limit, offset).await?;
    load_features(pool, cache, nfts).await
}

/// Extract valid UUIDs from a candidate slice, skipping rows with no id or unparseable ids.
///
/// Shared by `get_seen_nft_ids` and `get_not_interested_nft_ids` to avoid the
/// same iterator chain appearing in both functions.
fn extract_candidate_uuids(candidates: &[(CandidateNft, Option<NftFeatures>)]) -> Vec<Uuid> {
    candidates
        .iter()
        .filter_map(|(nft, _)| nft.id.as_deref().and_then(|id| Uuid::parse_str(id).ok()))
        .collect()
}

/// Bulk-load seen NFT IDs for a user — single SQL query, no N+1.
///
/// Returns the subset of candidate IDs that the user interacted with in the
/// last 30 days. Also populates the Redis seen-set for future fast lookups.
pub async fn get_seen_nft_ids(
    pool: &PgPool,
    cache: Option<&RecCache>,
    user_address: &str,
    candidates: &[(CandidateNft, Option<NftFeatures>)],
) -> Result<HashSet<Box<str>>> {
    let candidate_uuids = extract_candidate_uuids(candidates);

    if candidate_uuids.is_empty() {
        return Ok(HashSet::new());
    }

    let seen: Vec<Uuid> = sqlx::query_scalar(
        r#"
        SELECT DISTINCT nft_id
        FROM user_interactions
        WHERE user_address = $1
        AND nft_id = ANY($2)
        AND interaction_type IN ('view', 'like', 'purchase', 'save')
        AND created_at > NOW() - INTERVAL '30 days'
        "#,
    )
    .bind(user_address.to_lowercase())
    .bind(&candidate_uuids)
    .fetch_all(pool)
    .await?;

    // UUID Display is exactly 36 chars → into_boxed_str() is O(1) (cap == len).
    let seen_strings: Vec<String> = seen.iter().map(|u| u.to_string()).collect();

    if let Some(cache) = cache {
        cache.mark_nfts_seen(user_address, &seen_strings).await;
    }

    Ok(seen_strings.into_iter().map(|s| s.into_boxed_str()).collect())
}

// ── Social graph queries ──────────────────────────────────────────────────────

/// Addresses this user follows (active follows only).
pub async fn get_following_addresses(pool: &PgPool, user_address: &str) -> Result<Vec<String>> {
    let rows = sqlx::query_scalar::<_, String>(
        r#"
        SELECT u.address
        FROM follows f
        JOIN social_users u ON u.id = f.followee_id
        JOIN social_users follower ON follower.id = f.follower_id
        WHERE follower.address = $1 AND f.is_active = true AND u.is_blocked = false
        ORDER BY f.inserted_at DESC
        LIMIT 200
        "#,
    )
    .bind(user_address.to_lowercase())
    .fetch_all(pool)
    .await?;

    Ok(rows)
}

// ── Not-interested filter ─────────────────────────────────────────────────────

/// Bulk-load NFT IDs the user has explicitly marked "not interested".
///
/// Unlike `get_seen_nft_ids` (which is gated on `exclude_seen`), this set is
/// applied unconditionally — a user who taps "Not Interested" never wants to
/// see that NFT again regardless of pageSeen state.
///
/// Only looks inside the current candidate set to avoid a full-table scan.
pub async fn get_not_interested_nft_ids(
    pool: &PgPool,
    user_address: &str,
    candidates: &[(CandidateNft, Option<NftFeatures>)],
) -> Result<HashSet<Box<str>>> {
    let candidate_uuids = extract_candidate_uuids(candidates);

    if candidate_uuids.is_empty() {
        return Ok(HashSet::new());
    }

    let ids: Vec<Uuid> = sqlx::query_scalar(
        r#"
        SELECT DISTINCT nft_id
        FROM user_interactions
        WHERE user_address = $1
          AND nft_id = ANY($2)
          AND interaction_type = 'not_interested'
        "#,
    )
    .bind(user_address.to_lowercase())
    .bind(&candidate_uuids)
    .fetch_all(pool)
    .await?;

    Ok(ids.into_iter().map(|u| u.to_string().into_boxed_str()).collect())
}

// ── PG recommendation cache ───────────────────────────────────────────────────

/// Upsert pre-computed recommendations into the `recommendation_cache` table.
///
/// Writes PG first (durable). The engine's `write_to_caches_free` helper then
/// writes Redis only if this succeeds, so a PG write failure causes Redis to be
/// purged rather than holding stale data.
pub async fn cache_recommendations_pg(
    pool: &PgPool,
    user_address: &str,
    feed_type: &str,
    items: &[ScoredNft],
    ttl_minutes: i64,
) -> Result<()> {
    let recs = serde_json::to_value(items).map_err(anyhow::Error::from)?;
    sqlx::query(
        r#"
        INSERT INTO recommendation_cache
            (user_address, feed_type, recommendations, computed_at, expires_at, version)
        VALUES ($1, $2, $3, NOW(), NOW() + ($4 * INTERVAL '1 minute'), 1)
        ON CONFLICT (user_address, feed_type) DO UPDATE SET
            recommendations = EXCLUDED.recommendations,
            computed_at     = EXCLUDED.computed_at,
            expires_at      = EXCLUDED.expires_at,
            version         = recommendation_cache.version + 1
        "#,
    )
    .bind(user_address.to_lowercase())
    .bind(feed_type)
    .bind(recs)
    .bind(ttl_minutes)
    .execute(pool)
    .await?;
    Ok(())
}

/// Invalidate one feed_type's PG-durable cache entry (GENRE-02 follow-up).
///
/// Narrower than the bulk `DELETE FROM recommendation_cache WHERE user_address = $1`
/// used by `record_interaction`/`apply_preference_decay` (which wipe every feed
/// type for the user) — this targets exactly one `feed_type` so unrelated
/// caches (enhanced, following, trending, or a different genre-hash) are left
/// intact. Used by `RecommendationEngine::invalidate_genre_feed` to clear a
/// user's old genre-filtered feed right after they declare new favorites.
pub async fn delete_cached_recommendations_pg(
    pool: &PgPool,
    user_address: &str,
    feed_type: &str,
) -> Result<()> {
    sqlx::query("DELETE FROM recommendation_cache WHERE user_address = $1 AND feed_type = $2")
        .bind(user_address.to_lowercase())
        .bind(feed_type)
        .execute(pool)
        .await?;
    Ok(())
}

/// Read pre-computed recommendations from the `recommendation_cache` table.
///
/// Returns `None` on a cache miss or if the cached entry has expired.
/// On a miss the engine falls back to full scoring.
pub async fn get_cached_recommendations_pg(
    pool: &PgPool,
    user_address: &str,
    feed_type: &str,
) -> Result<Option<Vec<ScoredNft>>> {
    let row: Option<serde_json::Value> = sqlx::query_scalar(
        r#"
        SELECT recommendations
        FROM recommendation_cache
        WHERE user_address = $1
          AND feed_type = $2
          AND expires_at > NOW()
        "#,
    )
    .bind(user_address.to_lowercase())
    .bind(feed_type)
    .fetch_optional(pool)
    .await?;

    match row {
        None => Ok(None),
        Some(val) => {
            let items = serde_json::from_value::<Vec<ScoredNft>>(val)?;
            Ok(Some(items))
        }
    }
}


// ── Creator-scoped candidates ─────────────────────────────────────────────────

/// Fetch NFT candidates from a specific set of creator addresses.
///
/// Used by the "following" feed to surface content from creators the user follows.
/// Distinct from `list_candidates` (which is unfiltered / recency-sorted): this
/// query scopes entirely to the given creator set and is ordered by recency.
pub async fn get_nfts_from_creators(
    pool: &PgPool,
    creators: &[String],
    limit: usize,
    offset: usize,
) -> Result<Vec<CandidateNft>> {
    if creators.is_empty() {
        return Ok(vec![]);
    }

    let creators_lower: Vec<String> = creators.iter().map(|c| c.to_lowercase()).collect();

    Ok(sqlx::query_as::<_, CandidateNft>(
        r#"
        SELECT id::text, token_id, contract_address, contract_type::text,
               LOWER(creator_address) AS creator_address,
               COALESCE(creation_time, inserted_at)::text as created_at
        FROM nfts
        WHERE is_deleted = false AND is_original = true AND is_blocked = false
          AND LOWER(creator_address) = ANY($1)
        ORDER BY COALESCE(creation_time, inserted_at) DESC
        LIMIT $2 OFFSET $3
        "#,
    )
    .bind(&creators_lower)
    .bind(limit as i64)
    .bind(offset as i64)
    .fetch_all(pool)
    .await?)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests;
