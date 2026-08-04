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

/// Raw NFT row returned from candidate queries.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct CandidateNft {
    pub id: Option<String>,
    pub token_id: i64,
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
        HashMap::new()
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
                token_id:         nft.token_id,
                tags:             sf.tags,
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
) -> Result<HashSet<String>> {
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

    let seen_strings: Vec<String> = seen.into_iter().map(|u| u.to_string()).collect();

    if let Some(cache) = cache {
        cache.mark_nfts_seen(user_address, &seen_strings).await;
    }

    Ok(seen_strings.into_iter().collect())
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

// ── Tests ─────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use super::*;

    // ── Helpers ───────────────────────────────────────────────────────────────

    /// Mirrors the validation gate inside `list_candidates` without touching
    /// the database.  If this returns `Err`, `list_candidates` would also
    /// return `Err` immediately, before any SQL is executed.
    fn validate_contract_type(ct: Option<&str>) -> Result<()> {
        if let Some(s) = ct {
            if ContentType::from_str(s).is_none() {
                return Err(anyhow::anyhow!("Invalid contract_type: {}", s));
            }
        }
        Ok(())
    }

    // ── Valid inputs ──────────────────────────────────────────────────────────

    #[test]
    fn valid_known_types_pass() {
        for ct in &["snap", "art", "music", "flix"] {
            assert!(
                validate_contract_type(Some(ct)).is_ok(),
                "expected Ok for known type {ct:?}"
            );
        }
    }

    #[test]
    fn valid_known_types_are_case_insensitive() {
        // `list_candidates` passes the raw string to SQL as `$1` but the
        // ContentType guard uses `.to_lowercase()`, so mixed-case strings
        // accepted by the guard will reach the DB with their original casing.
        // This test ensures the guard itself does not reject them.
        for ct in &["SNAP", "Art", "MUSIC", "FLiX"] {
            assert!(
                validate_contract_type(Some(ct)).is_ok(),
                "expected Ok for mixed-case type {ct:?}"
            );
        }
    }

    #[test]
    fn none_filter_is_always_valid() {
        assert!(
            validate_contract_type(None).is_ok(),
            "None filter (fetch all) must not be rejected"
        );
    }

    // ── Invalid inputs ────────────────────────────────────────────────────────

    #[test]
    fn unknown_type_returns_err() {
        let err = validate_contract_type(Some("video"))
            .expect_err("'video' is not a known ContentType and must be rejected");
        let msg = err.to_string();
        assert!(
            msg.contains("Invalid contract_type"),
            "error message should mention 'Invalid contract_type', got: {msg:?}"
        );
        assert!(
            msg.contains("video"),
            "error message should echo the bad value, got: {msg:?}"
        );
    }

    #[test]
    fn empty_string_is_rejected() {
        let err = validate_contract_type(Some(""))
            .expect_err("empty string must be rejected as an invalid ContentType");
        let msg = err.to_string();
        assert!(
            msg.contains("Invalid contract_type"),
            "error message should mention 'Invalid contract_type', got: {msg:?}"
        );
    }

    #[test]
    fn whitespace_string_is_rejected() {
        let err = validate_contract_type(Some("  "))
            .expect_err("whitespace-only string must be rejected as an invalid ContentType");
        let msg = err.to_string();
        assert!(
            msg.contains("Invalid contract_type"),
            "error message should mention 'Invalid contract_type', got: {msg:?}"
        );
    }

    #[test]
    fn close_misspelling_is_rejected() {
        // Guard against fuzzy-matching: "snapp", "artt", etc. must not slip through.
        for bad in &["snapp", "artt", "musics", "flix2", "NFT", "image"] {
            assert!(
                validate_contract_type(Some(bad)).is_err(),
                "close misspelling {bad:?} must be rejected by the validation gate"
            );
        }
    }
}

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
) -> Result<HashSet<String>> {
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

    Ok(ids.into_iter().map(|u| u.to_string()).collect())
}

// ── Recommendation cache (Postgres) ──────────────────────────────────────────

/// Upsert a recommendation list into the `recommendation_cache` table.
pub async fn cache_recommendations_pg(
    pool: &PgPool,
    user_address: &str,
    feed_type: &str,
    recommendations: &[ScoredNft],
    ttl_minutes: i64,
) -> Result<()> {
    let expires_at = chrono::Utc::now() + chrono::Duration::minutes(ttl_minutes.max(1));
    let recommendations_json = serde_json::to_value(recommendations)?;

    sqlx::query(
        r#"
        INSERT INTO recommendation_cache
            (id, user_address, feed_type, recommendations, computed_at, expires_at, version)
        VALUES
            (gen_random_uuid(), $1, $2, $3, NOW(), $4, 1)
        ON CONFLICT (user_address, feed_type) DO UPDATE SET
            recommendations = $3,
            computed_at = NOW(),
            expires_at = $4,
            version = recommendation_cache.version + 1
        "#,
    )
    .bind(user_address.to_lowercase())
    .bind(feed_type)
    .bind(&recommendations_json)
    .bind(expires_at)
    .execute(pool)
    .await?;

    Ok(())
}

/// Fetch a non-expired recommendation list from `recommendation_cache`.
pub async fn get_cached_recommendations_pg(
    pool: &PgPool,
    user_address: &str,
    feed_type: &str,
) -> Result<Option<Vec<ScoredNft>>> {
    get_cached_recommendations_pg_filtered(pool, user_address, feed_type, None).await
}

/// Like `get_cached_recommendations_pg` but pushes an optional `contract_type`
/// filter to Postgres, avoiding full-row deserialise on the caller side.
pub async fn get_cached_recommendations_pg_filtered(
    pool: &PgPool,
    user_address: &str,
    feed_type: &str,
    contract_type: Option<&str>,
) -> Result<Option<Vec<ScoredNft>>> {
    let result: Option<serde_json::Value> = match contract_type {
        None => {
            sqlx::query_scalar::<_, serde_json::Value>(
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
            .await?
        }
        Some(ct) => {
            sqlx::query_scalar::<_, serde_json::Value>(
                r#"
                SELECT json_agg(elem ORDER BY (elem->>'score')::float DESC)
                FROM recommendation_cache rc,
                     jsonb_array_elements(rc.recommendations) AS elem
                WHERE rc.user_address = $1
                  AND rc.feed_type    = $2
                  AND rc.expires_at   > NOW()
                  AND elem->>'contract_type' = $3
                "#,
            )
            .bind(user_address.to_lowercase())
            .bind(feed_type)
            .bind(ct)
            .fetch_optional(pool)
            .await?
            .and_then(|v| if v.is_null() { None } else { Some(v) })
        }
    };

    match result {
        Some(value) => Ok(serde_json::from_value(value)?),
        None => Ok(None),
    }
}

/// Recent NFTs from a set of creator addresses.
pub async fn get_nfts_from_creators(
    pool: &PgPool,
    creators: &[String],
    limit: usize,
    offset: usize,
) -> Result<Vec<CandidateNft>> {
    // Normalize to lowercase before binding so the query can use a functional
    // index on LOWER(creator_address) instead of scanning every row.
    let normalized: Vec<String> = creators.iter().map(|s| s.to_lowercase()).collect();
    let nfts = sqlx::query_as::<_, CandidateNft>(
        r#"
        SELECT id::text, token_id, contract_address, contract_type::text,
               LOWER(creator_address) AS creator_address,
               COALESCE(creation_time, inserted_at)::text as created_at
        FROM nfts
        WHERE is_deleted = false
        AND is_original = true
        AND is_blocked = false
        AND LOWER(creator_address) = ANY($1)
        AND COALESCE(creation_time, inserted_at) > NOW() - INTERVAL '30 days'
        ORDER BY COALESCE(creation_time, inserted_at) DESC
        LIMIT $2 OFFSET $3
        "#,
    )
    .bind(&normalized)
    .bind(limit as i64)
    .bind(offset as i64)
    .fetch_all(pool)
    .await?;

    Ok(nfts)
}
