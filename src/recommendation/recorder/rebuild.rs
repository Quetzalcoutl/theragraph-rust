//! Rebuild a user's `user_preferences` row from their full `user_interactions`
//! history — the recovery path for the durable-signal foundation.
//!
//! No code path existed before this to reconstruct `tag_preferences` /
//! `creator_preferences` / affinities if `user_preferences` were ever lost or
//! corrupted. The raw ingredients survive regardless: nothing in this
//! codebase ever deletes or truncates `user_interactions` (every interaction,
//! including "unlike" and "not interested", is its own new immutable row).
//! This module closes that gap by replaying that history through the same
//! pure [`apply_interaction_to_prefs`] the live recorder uses — mirroring
//! `event_processor::reconciliation`'s "replay Postgres history through the
//! same write path" pattern, but for preferences instead of Nebula edges.
//!
//! Intended as an on-demand recovery/backfill operation, not a scheduled job:
//! call [`rebuild_user_preferences`] for one user, or [`rebuild_all_user_preferences`]
//! once after a backfill populates historically-empty `nft_tags` values.
//!
//! Chronological order matters: `MAX_TAG_PREFS`/`MAX_CREATOR_PREFS` eviction
//! (lowest-weight-wins) is order-dependent, so replaying out of order can
//! evict different tags/creators than production history actually did.
//!
//! `pct_played` (added in migration 015) lets `Listen`/`FlixWatch` weight be
//! reproduced exactly for interactions recorded after that migration; rows
//! recorded before it have `pct_played = NULL` and fall back to full weight
//! (`unwrap_or(1.0)`) like any other caller of `interaction_weight`.

use anyhow::Result;
use futures::stream::{self, StreamExt};
use sqlx::PgPool;
use tracing::{info, warn};
use uuid::Uuid;

use crate::recommendation::cache::RecCache;
use crate::recommendation::model::{
    apply_interaction_to_prefs, EvictionPolicy, InteractionEvent, InteractionType,
    TagEnrichmentStatus, UserPreferences,
};

/// Max concurrent rebuilds in `rebuild_all_user_preferences`. Matches
/// `event_processor::reconciliation::RECONCILE_CONCURRENCY` — same shape of
/// operation (replay Postgres history through a write path), same bound.
const REBUILD_CONCURRENCY: usize = 16;

#[derive(sqlx::FromRow)]
struct InteractionRow {
    interaction_type: String,
    view_duration_ms: Option<i64>,
    pct_played: Option<f32>,
    source: Option<String>,
    nft_contract_type: Option<String>,
    nft_creator_address: Option<String>,
    nft_tags: Vec<String>,
    nft_id: Uuid,
}

fn row_to_event(user_address: &str, row: InteractionRow) -> Option<InteractionEvent> {
    let interaction_type: InteractionType = row.interaction_type.parse().ok()?;
    Some(InteractionEvent {
        user_address: user_address.to_string(),
        nft_id: row.nft_id.to_string(),
        interaction_type,
        view_duration_ms: row.view_duration_ms,
        pct_played: row.pct_played,
        source: row.source,
        nft_contract_type: row.nft_contract_type,
        nft_creator_address: row.nft_creator_address,
        nft_tags: row.nft_tags,
        tag_enrichment: TagEnrichmentStatus::Complete,
        event_id: None,
    })
}

/// Core replay logic, no cache invalidation. Shared by the single-user and
/// batch entry points so the batch path can invalidate once for every
/// successfully-rebuilt user instead of once per user (same reasoning as
/// `apply_preference_decay`'s CC-001 batching).
async fn rebuild_one(pool: &PgPool, user_address: &str) -> Result<UserPreferences> {
    let normalized = user_address.to_lowercase();

    // RACE-001: lock the user_preferences row (or insert-if-missing) FIRST,
    // then read user_interactions history inside the SAME transaction —
    // before this, the history read happened outside any lock, so a live
    // interaction could land between the read and the final overwrite below
    // and be silently clobbered until its next re-application. A concurrent
    // record_interaction call takes this same FOR UPDATE lock (see
    // update_preferences_from_interaction), so it now either completes
    // first (and this rebuild's history read sees it) or blocks until this
    // transaction commits (and applies cleanly afterward) — no window where
    // a fresh interaction is silently overwritten.
    let mut tx = pool.begin().await?;
    super::load_or_insert_prefs_for_update(&mut tx, &normalized).await?;

    let rows: Vec<InteractionRow> = sqlx::query_as(
        "SELECT interaction_type, view_duration_ms, pct_played, source, \
         nft_contract_type, nft_creator_address, nft_tags, nft_id \
         FROM user_interactions \
         WHERE user_address = $1 \
         ORDER BY created_at ASC",
    )
    .bind(&normalized)
    .fetch_all(&mut *tx)
    .await?;

    let mut prefs = UserPreferences {
        user_address: normalized.clone(),
        ..Default::default()
    };

    let mut skipped = 0u64;
    for row in rows {
        match row_to_event(&normalized, row) {
            Some(event) => apply_interaction_to_prefs(&mut prefs, &event, EvictionPolicy::default()),
            None => skipped += 1,
        }
    }
    if skipped > 0 {
        warn!(
            "rebuild_one({normalized}): skipped {skipped} row(s) with an unparseable interaction_type"
        );
    }

    super::save_preferences(&mut *tx, &prefs).await?;
    tx.commit().await?;

    info!(
        user_address = %normalized,
        tags = prefs.tag_preferences.len(),
        creators = prefs.creator_preferences.len(),
        "rebuild_one: rebuilt from interaction history"
    );

    Ok(prefs)
}

/// CC-001 (same reasoning as `apply_preference_decay`): invalidate both the
/// Redis and Postgres recommendation caches for every address in one batched
/// call each, rather than one round-trip per user, so rebuilt preferences
/// are reflected on the next feed request instead of being masked by a
/// stale cache entry for up to its full TTL.
async fn invalidate_caches_batch(pool: &PgPool, cache: Option<&RecCache>, addresses: &[String]) {
    if addresses.is_empty() {
        return;
    }
    if let Some(cache) = cache {
        cache.delete_user_caches_batch(addresses).await;
    }
    if let Err(e) = sqlx::query("DELETE FROM recommendation_cache WHERE user_address = ANY($1)")
        .bind(addresses)
        .execute(pool)
        .await
    {
        warn!(
            "invalidate_caches_batch: failed to invalidate recommendation_cache for {} user(s): {e}",
            addresses.len()
        );
    }
}

/// Rebuild one user's `user_preferences` row from scratch by replaying their
/// entire `user_interactions` history, in chronological order, through the
/// same pure `apply_interaction_to_prefs` function the live recorder uses.
/// Replaces whatever is currently stored for this user — a full recovery,
/// not an incremental merge.
pub async fn rebuild_user_preferences(
    pool: &PgPool,
    cache: Option<&RecCache>,
    user_address: &str,
) -> Result<UserPreferences> {
    let prefs = rebuild_one(pool, user_address).await?;
    invalidate_caches_batch(pool, cache, std::slice::from_ref(&prefs.user_address)).await;
    Ok(prefs)
}

/// Rebuild every user that has at least one row in `user_interactions`, up
/// to `REBUILD_CONCURRENCY` at once — mirrors
/// `event_processor::reconciliation`'s bounded fan-out for the same shape of
/// "replay Postgres history" operation, rather than one user at a time.
/// Intended for a one-time recovery pass (e.g. immediately after a
/// historical `nft_tags` backfill) — not a scheduled job. Continues past
/// individual failures so one bad user doesn't abort the whole pass, and
/// invalidates caches once, in a single batch, for every user that
/// succeeded — not once per user.
pub async fn rebuild_all_user_preferences(pool: &PgPool, cache: Option<&RecCache>) -> Result<(u64, u64)> {
    let addresses: Vec<(String,)> =
        sqlx::query_as("SELECT DISTINCT user_address FROM user_interactions")
            .fetch_all(pool)
            .await?;

    let results: Vec<(String, Result<()>)> = stream::iter(addresses)
        .map(|(address,)| async move {
            let result = rebuild_one(pool, &address).await.map(|_| ());
            (address, result)
        })
        .buffer_unordered(REBUILD_CONCURRENCY)
        .collect()
        .await;

    let mut succeeded = Vec::with_capacity(results.len());
    let mut failed = 0u64;
    for (address, result) in results {
        match result {
            Ok(()) => succeeded.push(address),
            Err(e) => {
                failed += 1;
                warn!("rebuild_all_user_preferences: failed for {address}: {e}");
            }
        }
    }

    invalidate_caches_batch(pool, cache, &succeeded).await;

    let rebuilt = succeeded.len() as u64;
    info!(rebuilt, failed, "rebuild_all_user_preferences: pass complete");
    Ok((rebuilt, failed))
}

// NOTE: the historical nft_tags backfill (for rows the BUG-TAGS-01 fix left
// permanently empty) lives in event_processor::elixir_db::backfill, not
// here — it needs event_processor::elixir_db::nft_metadata, and
// `recommendation` is compiled twice (once into the lib target, once into
// the bin target alongside event_processor); a reference from this shared
// file to a bin-only module breaks the lib build. rebuild_user_preferences
// above has no such dependency, so it stays here.

#[cfg(test)]
mod tests {
    use super::*;

    fn row(interaction_type: &str, tags: Vec<&str>, pct_played: Option<f32>) -> InteractionRow {
        InteractionRow {
            interaction_type: interaction_type.to_string(),
            view_duration_ms: None,
            pct_played,
            source: Some("feed".to_string()),
            nft_contract_type: Some("music".to_string()),
            nft_creator_address: Some("0xcreator".to_string()),
            nft_tags: tags.into_iter().map(str::to_string).collect(),
            nft_id: Uuid::nil(),
        }
    }

    #[test]
    fn row_to_event_parses_known_interaction_type() {
        let event = row_to_event("0xuser", row("like", vec!["psy-trance"], None))
            .expect("\"like\" should parse");
        assert_eq!(event.interaction_type, InteractionType::Like);
        assert_eq!(event.user_address, "0xuser");
        assert_eq!(event.nft_tags, vec!["psy-trance".to_string()]);
        assert_eq!(event.tag_enrichment, TagEnrichmentStatus::Complete);
        assert_eq!(event.event_id, None);
    }

    #[test]
    fn row_to_event_carries_pct_played_through_for_listen_flix_watch() {
        let event = row_to_event("0xuser", row("flix_watch", vec![], Some(0.87)))
            .expect("\"flix_watch\" should parse");
        assert_eq!(event.pct_played, Some(0.87));
    }

    #[test]
    fn row_to_event_returns_none_for_an_unparseable_interaction_type() {
        // Guards the `skipped` counter in rebuild_user_preferences: a row
        // with a corrupt/unknown interaction_type must be skippable rather
        // than panicking or aborting the whole rebuild.
        assert!(row_to_event("0xuser", row("not_a_real_type", vec![], None)).is_none());
    }
}

#[cfg(test)]
mod live {
    //! Hand-run tests against a real database — `cargo test` never runs
    //! these (ignored by default). Everything below inserts under a
    //! clearly-fake `0xrebuildlivetest...` address, never a real user, and
    //! cleans up its own rows on the way out. Run with:
    //!   cargo test --lib recommendation::recorder::rebuild::live -- --ignored --nocapture
    use super::*;

    async fn connect() -> PgPool {
        let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
        PgPool::connect(&database_url).await.expect("connect pool")
    }

    /// Fake contract address every throwaway test NFT is minted under —
    /// distinctive enough to find and delete without touching real content.
    const LIVE_TEST_CONTRACT: &str = "0xlivetestcontract000000000000000000001";

    /// `user_interactions.nft_id` has a real FK to `nfts(id)` — a random
    /// UUID with no backing row fails the insert. Mint a minimal throwaway
    /// NFT first and return its id.
    async fn seed_nft(pool: &PgPool) -> Uuid {
        let token_id = (Uuid::new_v4().as_u128() & 0x7FFF_FFFF_FFFF_FFFF) as i64;
        let (id,): (Uuid,) = sqlx::query_as(
            "INSERT INTO nfts \
             (id, token_id, contract_address, contract_type, creator_address, owner_address, \
              created_at_block, inserted_at, updated_at) \
             VALUES (gen_random_uuid(), $1, $2, 'music', '0xcreatorlivetest', '0xcreatorlivetest', \
                     0, NOW(), NOW()) \
             RETURNING id",
        )
        .bind(token_id)
        .bind(LIVE_TEST_CONTRACT)
        .fetch_one(pool)
        .await
        .expect("seed_nft insert failed");
        id
    }

    async fn seed_interaction(
        pool: &PgPool,
        user_address: &str,
        interaction_type: &str,
        nft_tags: &[&str],
    ) {
        let nft_id = seed_nft(pool).await;
        sqlx::query(
            "INSERT INTO user_interactions \
             (id, event_id, user_address, nft_id, interaction_type, nft_contract_type, \
              nft_creator_address, nft_tags, created_at) \
             VALUES (gen_random_uuid(), $1, $2, $3, $4, 'music', '0xcreatorlivetest', $5, NOW())",
        )
        .bind(Uuid::new_v4().to_string())
        .bind(user_address)
        .bind(nft_id)
        .bind(interaction_type)
        .bind(nft_tags)
        .execute(pool)
        .await
        .expect("seed_interaction insert failed");
    }

    async fn cleanup(pool: &PgPool, user_address: &str) {
        sqlx::query("DELETE FROM user_interactions WHERE user_address = $1")
            .bind(user_address)
            .execute(pool)
            .await
            .expect("cleanup user_interactions failed");
        sqlx::query("DELETE FROM user_preferences WHERE user_address = $1")
            .bind(user_address)
            .execute(pool)
            .await
            .expect("cleanup user_preferences failed");
        // Also sweep any throwaway test NFTs this run minted — cheap and
        // keeps the table clean even if a run panics between seed calls.
        sqlx::query("DELETE FROM nfts WHERE contract_address = $1")
            .bind(LIVE_TEST_CONTRACT)
            .execute(pool)
            .await
            .expect("cleanup test nfts failed");
    }

    #[tokio::test]
    #[ignore = "hits a real database — run manually, not part of CI"]
    async fn rebuild_user_preferences_reconstructs_tags_from_real_rows() {
        let pool = connect().await;
        let user = "0xrebuildlivetest0000000000000000000001";
        cleanup(&pool, user).await; // in case a previous run panicked before its own cleanup

        seed_interaction(&pool, user, "like", &["psy-trance", "ambient"]).await;
        seed_interaction(&pool, user, "purchase", &["psy-trance"]).await;

        let prefs = rebuild_user_preferences(&pool, None, user)
            .await
            .expect("rebuild_user_preferences failed");

        assert!(
            prefs.tag_preferences.contains_key("psy-trance"),
            "expected psy-trance in tag_preferences, got {:?}",
            prefs.tag_preferences
        );
        assert!(prefs.tag_preferences.contains_key("ambient"));
        assert_eq!(prefs.total_likes, 1);
        assert_eq!(prefs.total_purchases, 1);

        // Confirm it actually persisted, not just returned in-memory.
        let persisted: (serde_json::Value,) =
            sqlx::query_as("SELECT tag_preferences FROM user_preferences WHERE user_address = $1")
                .bind(user)
                .fetch_one(&pool)
                .await
                .expect("expected a persisted user_preferences row");
        assert!(persisted.0.get("psy-trance").is_some());

        cleanup(&pool, user).await;
    }

    #[tokio::test]
    #[ignore = "hits a real database — run manually, not part of CI"]
    async fn rebuild_all_user_preferences_covers_every_distinct_address() {
        let pool = connect().await;
        let user_a = "0xrebuildlivetest00000000000000000000aa";
        let user_b = "0xrebuildlivetest00000000000000000000bb";
        cleanup(&pool, user_a).await;
        cleanup(&pool, user_b).await;

        seed_interaction(&pool, user_a, "like", &["ufo"]).await;
        seed_interaction(&pool, user_b, "like", &["conspiracy"]).await;

        let (rebuilt, failed) = rebuild_all_user_preferences(&pool, None)
            .await
            .expect("rebuild_all_user_preferences failed");

        assert!(rebuilt >= 2, "expected at least the 2 seeded users, got {rebuilt}");
        assert_eq!(failed, 0);

        let a_prefs = rebuild_one(&pool, user_a).await.expect("re-check user_a");
        let b_prefs = rebuild_one(&pool, user_b).await.expect("re-check user_b");
        assert!(a_prefs.tag_preferences.contains_key("ufo"));
        assert!(b_prefs.tag_preferences.contains_key("conspiracy"));

        cleanup(&pool, user_a).await;
        cleanup(&pool, user_b).await;
    }
}
