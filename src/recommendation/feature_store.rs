//! Async Postgres persistence for NFT features.
//!
//! All SQL lives here. `feature_extractor` stays free of `sqlx` and `PgPool`
//! so the pure extraction logic can be unit-tested without a live database.

use anyhow::Result;
use sqlx::PgPool;
use tracing::info;
use uuid::Uuid;

use super::feature_extractor::NftFeatures;

// ── Private DB row ────────────────────────────────────────────────────────────

#[derive(Debug, sqlx::FromRow)]
struct FeaturesRow {
    nft_id: Uuid,
    contract_address: String,
    token_id: i64,
    tags: Option<Vec<String>>,
    primary_color: Option<String>,
    style: Option<String>,
    mood: Option<String>,
    genre: Option<String>,
    engagement_score: f32,
    trending_score: f32,
    quality_score: f32,
}

fn row_to_model(row: FeaturesRow) -> NftFeatures {
    NftFeatures {
        nft_id: row.nft_id.to_string(),
        contract_address: row.contract_address,
        token_id: row.token_id,
        tags: row.tags.unwrap_or_default(),
        primary_color: row.primary_color,
        style: row.style,
        mood: row.mood,
        genre: row.genre,
        engagement_score: row.engagement_score,
        trending_score: row.trending_score,
        quality_score: row.quality_score,
    }
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Upsert extracted features into the `nft_features` table.
pub async fn save_features(pool: &PgPool, features: &NftFeatures) -> Result<()> {
    let nft_uuid = Uuid::parse_str(&features.nft_id)
        .map_err(|_| anyhow::anyhow!("Invalid UUID in nft_id: {}", features.nft_id))?;

    sqlx::query(
        r#"
        INSERT INTO nft_features
            (id, nft_id, contract_address, token_id, tags, primary_color,
             style, mood, genre, engagement_score, trending_score, quality_score,
             inserted_at, updated_at)
        VALUES
            (gen_random_uuid(), $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, NOW(), NOW())
        ON CONFLICT (nft_id) DO UPDATE SET
            tags = $4,
            primary_color = $5,
            style = $6,
            mood = $7,
            genre = $8,
            engagement_score = $9,
            trending_score = $10,
            quality_score = $11,
            updated_at = NOW()
        "#,
    )
    .bind(nft_uuid)
    .bind(&features.contract_address)
    .bind(features.token_id)
    .bind(&features.tags)
    .bind(&features.primary_color)
    .bind(&features.style)
    .bind(&features.mood)
    .bind(&features.genre)
    .bind(features.engagement_score)
    .bind(features.trending_score)
    .bind(features.quality_score)
    .execute(pool)
    .await?;

    Ok(())
}

/// Batch-fetch `NftFeatures` for many NFTs in one SQL query — eliminates N+1.
///
/// Returns only entries that exist in `nft_features`; missing IDs are absent.
pub async fn get_features_batch(pool: &PgPool, nft_ids: &[Uuid]) -> Result<Vec<NftFeatures>> {
    if nft_ids.is_empty() {
        return Ok(vec![]);
    }

    let rows: Vec<FeaturesRow> = sqlx::query_as::<_, FeaturesRow>(
        r#"
        SELECT nft_id, contract_address, token_id, tags, primary_color,
               style, mood, genre,
               engagement_score::real, trending_score::real, quality_score::real
        FROM nft_features
        WHERE nft_id = ANY($1)
        "#,
    )
    .bind(nft_ids)
    .fetch_all(pool)
    .await?;

    Ok(rows.into_iter().map(row_to_model).collect())
}

/// Recompute engagement scores for all NFTs (run periodically).
///
/// Formula: LN(1 + weighted_raw) / 5.0, clamped to [0, 1].
/// Zero-engagement items score 0.0 (not 0.5 from old sigmoid).
/// Saturation ~150 weighted interactions (LN(151) / 5.0 ≈ 1.0).
/// Weights: likes×1, buys×3, comments×0.5.
pub async fn update_engagement_scores(pool: &PgPool) -> Result<u64> {
    let result = sqlx::query(
        r#"
        UPDATE nft_features f SET
            engagement_score = LEAST(1.0::double precision,
                LN(1.0 + (
                    COALESCE(n.likes_count, 0)::double precision +
                    COALESCE(n.buys_count, 0)::double precision * 3.0 +
                    COALESCE(n.comments_count, 0)::double precision * 0.5
                )) / 5.0
            ),
            updated_at = NOW()
        FROM nfts n
        WHERE f.nft_id = n.id
        "#,
    )
    .execute(pool)
    .await?;

    info!("📊 Updated engagement scores for {} NFTs", result.rows_affected());
    Ok(result.rows_affected())
}

/// Recompute trending scores from recent interactions (run hourly).
///
/// CTE + LEFT JOIN replaces the old correlated subquery that held a write
/// lock on the entire `nft_features` table per row. Single GROUP BY over
/// `user_interactions` once; covering index on (nft_id, created_at) avoids
/// the sequential scan (migration 007).
pub async fn update_trending_scores(pool: &PgPool) -> Result<u64> {
    let result = sqlx::query(
        r#"
        WITH recent_scores AS (
            SELECT
                nft_id,
                SUM(
                    CASE interaction_type
                        WHEN 'like'     THEN 1.0
                        WHEN 'purchase' THEN 3.0
                        WHEN 'view'     THEN 0.1
                        ELSE 0.5
                    END * EXP(-EXTRACT(EPOCH FROM (NOW() - created_at)) / 86400.0)
                ) AS raw_score
            FROM user_interactions
            WHERE created_at > NOW() - INTERVAL '7 days'
            GROUP BY nft_id
        )
        UPDATE nft_features f
        SET trending_score = LEAST(1.0::double precision,
                LN(1.0 + COALESCE(s.raw_score, 0.0)) / LN(301.0)
            ),
            updated_at = NOW()
        FROM nft_features AS base
        LEFT JOIN recent_scores s ON s.nft_id = base.nft_id
        WHERE f.nft_id = base.nft_id
        "#,
    )
    .execute(pool)
    .await?;

    info!("📈 Updated trending scores for {} NFTs", result.rows_affected());
    Ok(result.rows_affected())
}
