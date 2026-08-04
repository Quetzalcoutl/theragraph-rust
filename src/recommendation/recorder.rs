//! Async database and cache I/O for user preferences.
//!
//! This module wraps the pure domain logic in [`super::model`] with async
//! database access via sqlx and Redis cache operations via [`super::cache`].
//!
//! Pure types and functions with no I/O live in [`super::model`].

use anyhow::Result;
use sqlx::PgPool;
use tracing::{info, warn};
use uuid::Uuid;

use super::cache::RecCache;
use super::model::{
    apply_interaction_to_prefs, apply_preset_seeds, interaction_weight,
    EvictionPolicy, InteractionEvent, UserPreferences,
};
use super::weights::DECAY_FACTOR;

// ── Database row mapping ──────────────────────────────────────────────────────

/// Database row for preferences
#[derive(Debug, sqlx::FromRow)]
struct PreferencesRow {
    user_address: String,
    snap_affinity: f32,
    art_affinity: f32,
    music_affinity: f32,
    flix_affinity: f32,
    tag_preferences: serde_json::Value,
    creator_preferences: serde_json::Value,
    total_likes: i32,
    total_purchases: i32,
    total_views: i32,
}

/// Convert a database row into `UserPreferences`, returning an error on corrupt JSON.
///
/// Centralises the row→struct mapping so both `get_or_create_preferences` and
/// `load_or_insert_prefs_for_update` use identical deserialisation logic.
fn row_to_prefs(row: PreferencesRow) -> Result<UserPreferences> {
    let tag_preferences = serde_json::from_value(row.tag_preferences)
        .map_err(|e| anyhow::anyhow!("Corrupt tag_preferences for {}: {}", row.user_address, e))?;
    let creator_preferences = serde_json::from_value(row.creator_preferences)
        .map_err(|e| anyhow::anyhow!("Corrupt creator_preferences for {}: {}", row.user_address, e))?;
    Ok(UserPreferences {
        user_address: row.user_address,
        snap_affinity: row.snap_affinity,
        art_affinity: row.art_affinity,
        music_affinity: row.music_affinity,
        flix_affinity: row.flix_affinity,
        tag_preferences,
        creator_preferences,
        total_likes: row.total_likes,
        total_purchases: row.total_purchases,
        total_views: row.total_views,
    })
}

// ── Transaction-level helpers ─────────────────────────────────────────────────

/// Load the preferences row for `addr` within `tx`, inserting defaults if no row exists yet.
///
/// FIX new-user-prefs-update-race: The original code called `INSERT … ON CONFLICT DO NOTHING`
/// and then immediately used in-memory defaults — meaning two concurrent first-time requests
/// would each apply their event to an independent default struct and the second `UPDATE` would
/// overwrite the first interaction permanently.
///
/// This helper always re-issues `SELECT … FOR UPDATE` after the INSERT so that:
/// - If this transaction won the insert race, the lock is acquired on the freshly inserted row.
/// - If a concurrent transaction won, the re-SELECT blocks until that transaction commits, then
///   reads the committed row and acquires the FOR UPDATE lock on it.
///
/// Either way the caller receives the actual committed row — never a stale in-memory default —
/// and the lock prevents any further concurrent writes until this transaction commits.
async fn load_or_insert_prefs_for_update<'t>(
    tx: &mut sqlx::Transaction<'t, sqlx::Postgres>,
    addr: &str,
) -> Result<UserPreferences> {
    // First attempt: lock an existing row.
    let result = sqlx::query_as::<_, PreferencesRow>(
        r#"
        SELECT
            user_address,
            snap_affinity::real, art_affinity::real, music_affinity::real, flix_affinity::real,
            tag_preferences, creator_preferences, total_likes, total_purchases, total_views
        FROM user_preferences
        WHERE user_address = $1
        FOR UPDATE
        "#,
    )
    .bind(addr)
    .fetch_optional(&mut **tx)
    .await?;

    if let Some(row) = result {
        return row_to_prefs(row);
    }

    // Row does not exist yet — insert default values, tolerating a concurrent winner.
    let new_prefs = UserPreferences {
        user_address: addr.to_string(),
        ..Default::default()
    };
    let tag_json = serde_json::to_value(&new_prefs.tag_preferences)?;
    let creator_json = serde_json::to_value(&new_prefs.creator_preferences)?;
    sqlx::query(
        r#"
        INSERT INTO user_preferences
            (id, user_address, snap_affinity, art_affinity, music_affinity, flix_affinity,
             tag_preferences, creator_preferences, total_likes, total_purchases, total_views,
             last_activity_at, inserted_at, updated_at)
        VALUES
            (gen_random_uuid(), $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, NOW(), NOW(), NOW())
        ON CONFLICT (user_address) DO NOTHING
        "#,
    )
    .bind(addr)
    .bind(new_prefs.snap_affinity)
    .bind(new_prefs.art_affinity)
    .bind(new_prefs.music_affinity)
    .bind(new_prefs.flix_affinity)
    .bind(&tag_json)
    .bind(&creator_json)
    .bind(new_prefs.total_likes)
    .bind(new_prefs.total_purchases)
    .bind(new_prefs.total_views)
    .execute(&mut **tx)
    .await?;

    // Re-SELECT after INSERT: reads the committed row regardless of which concurrent
    // transaction won the insert race. The FOR UPDATE lock serialises further writes.
    let row = sqlx::query_as::<_, PreferencesRow>(
        r#"
        SELECT
            user_address,
            snap_affinity::real, art_affinity::real, music_affinity::real, flix_affinity::real,
            tag_preferences, creator_preferences, total_likes, total_purchases, total_views
        FROM user_preferences
        WHERE user_address = $1
        FOR UPDATE
        "#,
    )
    .bind(addr)
    .fetch_one(&mut **tx)
    .await?;

    row_to_prefs(row)
}

// ── Public async API ──────────────────────────────────────────────────────────

/// Load (or create) preferences for `user_address`, with an optional Redis
/// read-ahead and write-through.
///
/// Pass `cache = Some(c)` from every hot path (engine feeds) so the DB is
/// only hit on a cold start or after an invalidation.  Pass `cache = None`
/// from paths that don't hold a `RecCache` handle (e.g. admin endpoints,
/// onboarding seeding).
///
/// Cache contract:
/// - Hit: return immediately without a DB round-trip.
/// - Miss: load from DB, write through to Redis, return.
/// - Error reading cache: silently fall through to DB (cache is best-effort).
pub async fn get_or_create_preferences(
    pool: &PgPool,
    cache: Option<&RecCache>,
    user_address: &str,
) -> Result<UserPreferences> {
    let normalized = user_address.to_lowercase();

    // Fast path: Redis hit.
    if let Some(c) = cache {
        if let Some(prefs) = c.get_user_prefs::<UserPreferences>(&normalized).await {
            return Ok(prefs);
        }
    }

    let result = sqlx::query_as::<_, PreferencesRow>(
        r#"
        SELECT
            user_address,
            snap_affinity::real, art_affinity::real, music_affinity::real, flix_affinity::real,
            tag_preferences, creator_preferences, total_likes, total_purchases, total_views
        FROM user_preferences
        WHERE user_address = $1
        "#,
    )
    .bind(&normalized)
    .fetch_optional(pool)
    .await?;

    // BUG-004: corrupt JSON must surface as an Err so callers can decide
    // whether to repair the row.  Silently returning empty defaults hides
    // data corruption and causes the engine to overwrite good data with
    // zero-weight profiles on the next preferences write.
    let prefs = match result {
        Some(row) => row_to_prefs(row)?,
        None => {
            // Create new preferences with defaults
            let prefs = UserPreferences {
                user_address: normalized.clone(),
                ..Default::default()
            };

            let tag_prefs_json = serde_json::to_value(&prefs.tag_preferences)?;
            let creator_prefs_json = serde_json::to_value(&prefs.creator_preferences)?;

            sqlx::query(
                r#"
                INSERT INTO user_preferences
                    (id, user_address, snap_affinity, art_affinity, music_affinity, flix_affinity,
                     tag_preferences, creator_preferences, total_likes, total_purchases, total_views,
                     last_activity_at, inserted_at, updated_at)
                VALUES
                    (gen_random_uuid(), $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, NOW(), NOW(), NOW())
                ON CONFLICT (user_address) DO NOTHING
                "#
            )
            .bind(&normalized)
            .bind(prefs.snap_affinity)
            .bind(prefs.art_affinity)
            .bind(prefs.music_affinity)
            .bind(prefs.flix_affinity)
            .bind(&tag_prefs_json)
            .bind(&creator_prefs_json)
            .bind(prefs.total_likes)
            .bind(prefs.total_purchases)
            .bind(prefs.total_views)
            .execute(pool)
            .await?;

            // Re-SELECT after INSERT: if ON CONFLICT DO NOTHING was a no-op (concurrent
            // insert race), the in-memory default would be cached to Redis for 300s,
            // poisoning the profile with zeros. Always read what is actually in the DB.
            match sqlx::query_as::<_, PreferencesRow>(
                r#"
                SELECT
                    user_address,
                    snap_affinity::real, art_affinity::real, music_affinity::real, flix_affinity::real,
                    tag_preferences, creator_preferences, total_likes, total_purchases, total_views
                FROM user_preferences
                WHERE user_address = $1
                "#,
            )
            .bind(&normalized)
            .fetch_optional(pool)
            .await?
            {
                Some(row) => row_to_prefs(row)?,
                None => prefs,
            }
        }
    };

    // Write-through: populate Redis so subsequent requests in the same TTL
    // window skip the DB entirely.
    if let Some(c) = cache {
        c.set_user_prefs(&normalized, &prefs).await;
    }

    Ok(prefs)
}

/// Persist `prefs` to `user_preferences` via the provided executor.
///
/// FIX update-prefs-sql-duplication: accepts any `sqlx::Executor` so the caller
/// can pass either a `&PgPool` (used by `seed_from_presets`) or a `&mut *tx`
/// inside an open transaction (used by `update_preferences_from_interaction`).
/// The UPDATE query text now lives in exactly one place.
async fn save_preferences<'e, E>(executor: E, prefs: &UserPreferences) -> Result<()>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    let tag_prefs_json = serde_json::to_value(&prefs.tag_preferences)?;
    let creator_prefs_json = serde_json::to_value(&prefs.creator_preferences)?;

    sqlx::query(
        r#"
        UPDATE user_preferences SET
            snap_affinity = $2,
            art_affinity = $3,
            music_affinity = $4,
            flix_affinity = $5,
            tag_preferences = $6,
            creator_preferences = $7,
            total_likes = $8,
            total_purchases = $9,
            total_views = $10,
            last_activity_at = NOW(),
            updated_at = NOW()
        WHERE user_address = $1
        "#,
    )
    .bind(&prefs.user_address)
    .bind(prefs.snap_affinity)
    .bind(prefs.art_affinity)
    .bind(prefs.music_affinity)
    .bind(prefs.flix_affinity)
    .bind(&tag_prefs_json)
    .bind(&creator_prefs_json)
    .bind(prefs.total_likes)
    .bind(prefs.total_purchases)
    .bind(prefs.total_views)
    .execute(executor)
    .await?;

    Ok(())
}

/// Records a user interaction and updates preferences.
/// Pass `cache` to invalidate the stale prefs key immediately after the DB write —
/// the next recommendation query will then load fresh weights on the first request.
pub async fn record_interaction(
    pool: &PgPool,
    event: InteractionEvent,
    cache: Option<&RecCache>,
) -> Result<()> {
    // 1. Insert interaction record
    insert_interaction(pool, &event).await?;

    // 2. Update user preferences based on interaction
    update_preferences_from_interaction(pool, &event).await?;

    // 3. Invalidate Redis prefs key AND recommendation results so the next query uses fresh data,
    //    then append a session signal so the recency boost is immediately available.
    let normalized_addr = event.user_address.to_lowercase();
    if let Some(cache) = cache {
        cache.delete_user_prefs(&normalized_addr).await;
        cache.delete_recommendations(&normalized_addr).await;

        // Only positive interactions drive the session recency boost — negative weight
        // events (Unlike, NotInterested) should not promote tags/creators.
        let w = interaction_weight(&event);
        if w > 0.0 {
            let ts_unix = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64;
            let signal = super::cache::SessionSignal {
                tags: event.nft_tags.clone(),
                creator: event.nft_creator_address.clone(),
                interaction_weight: w,
                ts_unix,
            };
            cache.append_session_signal(&normalized_addr, signal).await;
        }
    }

    // 4. Invalidate SQL recommendation cache — both layers must agree or stale recs leak through
    if let Err(e) = sqlx::query("DELETE FROM recommendation_cache WHERE user_address = $1")
        .bind(event.user_address.to_lowercase())
        .execute(pool)
        .await
    {
        warn!("Failed to invalidate recommendation cache for {}: {}", event.user_address, e);
        metrics::counter!("pref_cache_invalidation_failures_total").increment(1);
    }

    info!(
        "📊 Recorded {} interaction: user={}, nft={}",
        event.interaction_type, event.user_address, event.nft_id
    );

    Ok(())
}

async fn insert_interaction(pool: &PgPool, event: &InteractionEvent) -> Result<()> {
    let nft_uuid = Uuid::parse_str(&event.nft_id)
        .map_err(|_| anyhow::anyhow!("Invalid UUID in nft_id: {}", event.nft_id))?;

    // BUG-001: use the caller-supplied event_id as an idempotency key so that
    // replayed Kafka messages or retried HTTP calls cannot create duplicate rows.
    // If no event_id is provided, generate a fresh UUID (non-idempotent path,
    // acceptable for fire-and-forget callers that never retry).
    let event_id = event
        .event_id
        .clone()
        .unwrap_or_else(|| Uuid::new_v4().to_string());

    sqlx::query(
        r#"
        INSERT INTO user_interactions
            (id, event_id, user_address, nft_id, interaction_type, view_duration_ms, source,
             nft_contract_type, nft_creator_address, nft_tags, created_at)
        VALUES
            (gen_random_uuid(), $1, $2, $3, $4, $5, $6, $7, $8, $9, NOW())
        ON CONFLICT (event_id) DO NOTHING
        "#,
    )
    .bind(&event_id)
    .bind(&event.user_address.to_lowercase())
    .bind(nft_uuid)
    .bind(event.interaction_type.to_string())
    .bind(event.view_duration_ms)
    .bind(&event.source)
    .bind(&event.nft_contract_type)
    .bind(&event.nft_creator_address)
    .bind(&event.nft_tags)
    .execute(pool)
    .await?;

    Ok(())
}

async fn update_preferences_from_interaction(
    pool: &PgPool,
    event: &InteractionEvent,
) -> Result<()> {
    let normalized = event.user_address.to_lowercase();
    let mut tx = pool.begin().await?;

    // FIX new-user-prefs-update-race: use the helper that re-SELECTs after
    // INSERT so the FOR UPDATE lock is always held on the committed row,
    // regardless of which concurrent transaction won the insert race.
    let mut prefs = load_or_insert_prefs_for_update(&mut tx, &normalized).await?;

    apply_interaction_to_prefs(&mut prefs, event, EvictionPolicy::default());

    // FIX update-prefs-sql-duplication: delegate to the shared helper so the
    // UPDATE query text lives in exactly one place.
    save_preferences(&mut *tx, &prefs).await?;

    tx.commit().await?;
    Ok(())
}

/// Seed initial preferences from onboarding preset selections.
///
/// Only writes values where the existing score is still at the neutral default (≤ 0.5),
/// so real interaction data accumulated before onboarding completes is never overwritten.
///
/// Idempotent: calling twice with the same presets is a no-op for any key that was
/// elevated by the first call (because 0.85 > 0.5 → guard blocks the second write).
///
/// FIX seed-prefs-race: The original code called `get_or_create_preferences` (plain
/// SELECT, no lock) and then wrote back via `save_preferences`.  A concurrent first
/// interaction racing the onboarding seed could read the same unfilled row, apply its
/// own field updates, and the last writer would silently discard the other's changes.
///
/// This function now opens a transaction and uses `load_or_insert_prefs_for_update`
/// (SELECT … FOR UPDATE) — the same pattern used by `update_preferences_from_interaction`
/// — so the seed and any concurrent interaction are serialized at the database level.
///
/// FIX seed-prefs-cache-stale: After a successful seed the Redis prefs key is
/// explicitly deleted so the next feed request reads the freshly seeded weights
/// rather than the unseeded snapshot that was cached before onboarding ran.
pub async fn seed_from_presets(
    pool: &PgPool,
    cache: Option<&RecCache>,
    user_address: &str,
    presets: &[String],
) -> Result<()> {
    let normalized = user_address.to_lowercase();
    let mut tx = pool.begin().await?;

    // FIX seed-prefs-race: acquire a FOR UPDATE lock so concurrent interactions
    // are serialized; never reads a stale in-memory default.
    let mut prefs = load_or_insert_prefs_for_update(&mut tx, &normalized).await?;

    for preset in presets {
        apply_preset_seeds(&mut prefs, preset.as_str());
    }

    save_preferences(&mut *tx, &prefs).await?;
    tx.commit().await?;

    // FIX seed-prefs-cache-stale: evict the unseeded snapshot so the next feed
    // request loads the freshly written weights from DB.
    if let Some(c) = cache {
        c.delete_user_prefs(&normalized).await;
        c.delete_recommendations(&normalized).await;
    }

    info!("🌱 Seeded onboarding preferences for {} ({} presets)", user_address, presets.len());
    Ok(())
}

/// Apply time decay to all preferences (run daily via cron).
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
            updated_at = NOW()
        WHERE last_activity_at < NOW() - INTERVAL '1 day'
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
