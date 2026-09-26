//! apply_preference_decay — daily maintenance pass for user preference scores.

use anyhow::Result;
use sqlx::PgPool;
use tracing::{info, warn};

use crate::recommendation::cache::RecCache;
use crate::recommendation::weights::DECAY_FACTOR;

/// Apply time decay to all preferences.
///
/// Gated on `last_decayed_at`, NOT `last_activity_at` — `last_activity_at` is
/// refreshed by any interaction (recorder::save_preferences), so gating decay
/// on it would mean an active user's preferences never decay (the predicate
/// never matches while they keep interacting) while a dormant user decays on
/// every scheduler tick instead of once per day (engagement_update_interval
/// defaults to hourly — 0.95 applied 24×/day ≈ 0.29, not the documented
/// gradual week-scale fade). `last_decayed_at` tracks decay application
/// itself, independent of activity or cron cadence, so this runs at most
/// once per row per day regardless of how often the caller invokes it.
///
/// CC-001: pass `cache` so Redis and PG recommendation caches are invalidated
/// for every user whose preferences just changed; without this, stale cached
/// recs are served until their TTL expires (up to several hours after decay).
pub async fn apply_preference_decay(pool: &PgPool, cache: Option<&RecCache>) -> Result<u64> {
    #[derive(sqlx::FromRow)]
    struct AffectedUser {
        user_address: String,
    }

    // RETURNING lets us know exactly which users were touched so we can do
    // targeted cache invalidations rather than a full-cache flush.
    let rows = sqlx::query_as::<_, AffectedUser>(
        r#"
        UPDATE user_preferences SET
            snap_affinity  = 0.5 + (snap_affinity  - 0.5) * $1,
            art_affinity   = 0.5 + (art_affinity   - 0.5) * $1,
            music_affinity = 0.5 + (music_affinity - 0.5) * $1,
            flix_affinity  = 0.5 + (flix_affinity  - 0.5) * $1,
            tag_preferences = COALESCE(
                (SELECT jsonb_object_agg(
                            key,
                            LEAST(1.0, GREATEST(0.0,
                                0.5 + (value::double precision - 0.5) * $1))::text::jsonb)
                 FROM jsonb_each_text(tag_preferences)
                 WHERE jsonb_typeof(tag_preferences) = 'object'),
                tag_preferences
            ),
            creator_preferences = COALESCE(
                (SELECT jsonb_object_agg(
                            key,
                            LEAST(1.0, GREATEST(0.0,
                                0.5 + (value::double precision - 0.5) * $1))::text::jsonb)
                 FROM jsonb_each_text(creator_preferences)
                 WHERE jsonb_typeof(creator_preferences) = 'object'),
                creator_preferences
            ),
            updated_at = NOW(),
            last_decayed_at = NOW()
        WHERE last_decayed_at < NOW() - INTERVAL '1 day'
        RETURNING user_address
        "#,
    )
    .bind(DECAY_FACTOR as f64)
    .fetch_all(pool)
    .await?;

    let count = rows.len() as u64;

    // CC-001: evict stale Redis entries for every affected user.
    // Use batch delete to avoid N × 2 sequential DEL round-trips.
    // delete_user_caches_batch sends exactly 2 DEL commands regardless of user count.
    if let Some(cache) = cache {
        let addresses: Vec<String> = rows.iter().map(|r| r.user_address.clone()).collect();
        cache.delete_user_caches_batch(&addresses).await;
    }

    // CC-001: also purge the PG recommendation_cache so the SQL read path
    // doesn't serve stale recs after Redis is already clean.
    if !rows.is_empty() {
        let addresses: Vec<String> = rows.iter().map(|r| r.user_address.clone()).collect();
        if let Err(e) = sqlx::query(
            "DELETE FROM recommendation_cache WHERE user_address = ANY($1)",
        )
        .bind(&addresses)
        .execute(pool)
        .await
        {
            warn!(
                "CC-001: failed to invalidate PG recommendation_cache after decay ({} users): {e}",
                addresses.len()
            );
        }
    }

    info!("🔄 Applied preference decay to {} users", count);
    Ok(count)
}
