// Social-graph handlers — follow/unfollow write to both Postgres and Nebula.

use crate::error::{Error, Result};
use crate::kafka::BlockchainEvent;
use tracing::{info, warn};

use super::EventProcessor;

impl EventProcessor {
    pub(super) async fn handle_follow(&self, event: &BlockchainEvent) -> Result<()> {
        if let Some(data) = &event.data {
            // POOL-004: normalize immediately so every downstream path (DB bind,
            // cache key, graph_sync) receives a consistent lowercase address.
            // Previously the DB got .to_lowercase() at .bind() time but graph_sync
            // received the original mixed-case string which failed is_safe_address
            // (EIP-55 checksummed addresses start with uppercase hex digits).
            let follower_raw = data.get("follower").and_then(|v| v.as_str()).unwrap_or("");
            let target_raw = data.get("target").and_then(|v| v.as_str()).unwrap_or("");
            let follower = follower_raw.to_lowercase();
            let target = target_raw.to_lowercase();
            let follower = follower.as_str();
            let target = target.as_str();

            if follower.is_empty() || target.is_empty() {
                warn!("handle_follow: missing follower or target in event data — dropping: {:?}", data);
                return Ok(());
            }

            // Boost creator affinity in user_preferences (strong social signal: 0.8).
            // jsonb merge preserves all other creator entries.
            sqlx::query(
                "INSERT INTO user_preferences \
                 (id, user_address, creator_preferences, inserted_at, updated_at) \
                 VALUES (gen_random_uuid(), $1, jsonb_build_object($2, 0.8::float8), NOW(), NOW()) \
                 ON CONFLICT (user_address) DO UPDATE SET \
                     creator_preferences = user_preferences.creator_preferences || \
                         jsonb_build_object($2, LEAST( \
                             COALESCE((user_preferences.creator_preferences->$2)::float8, 0.3) + 0.15, \
                             0.95 \
                         )), \
                     updated_at = NOW()",
            )
            .bind(follower)
            .bind(target)
            .execute(&self.pool)
            .await
            .map_err(|e| Error::Database {
                message: "Failed to update creator affinity on follow".into(),
                source: Some(e),
            })?;

            // CC-003: invalidate Redis caches immediately after the DB write so
            // the next feed open reads fresh following/prefs data rather than the
            // stale cached version, which could be served for up to the full TTL.
            if let Some(ref cache) = self.cache {
                // Fire all five DEL operations concurrently — no dependency between them.
                tokio::join!(
                    cache.delete_following(follower),
                    cache.delete_user_prefs(follower),
                    cache.delete_recommendations(follower),
                    cache.delete_fof_all(follower),
                    cache.delete_user_suggestions(follower, target),
                );
            }

            // TAG-S26-03: best-effort Nebula sync. The Postgres upsert above already
            // committed the creator-affinity signal; propagating a Nebula error here
            // does NOT cause a retry — subsequent successful messages in the same
            // partition advance the committed Kafka offset past this one, permanently
            // losing the follow. Use warn-and-continue; the reconciler will replay
            // follows on the next pass if Nebula recovers.
            if let Err(e) = self.graph_sync
                .sync_follow(follower, target, &event.transaction_hash)
                .await
            {
                warn!("handle_follow: Nebula sync_follow failed (best-effort): {e}");
            }

            info!("Follow: {} -> {} (Nebula + prefs updated)", follower, target);
        }
        Ok(())
    }

    pub(super) async fn handle_unfollow(&self, event: &BlockchainEvent) -> Result<()> {
        if let Some(data) = &event.data {
            // POOL-004: same normalization as handle_follow.
            let follower_raw = data.get("follower").and_then(|v| v.as_str()).unwrap_or("");
            let target_raw = data.get("target").and_then(|v| v.as_str()).unwrap_or("");
            let follower = follower_raw.to_lowercase();
            let target = target_raw.to_lowercase();
            let follower = follower.as_str();
            let target = target.as_str();

            if follower.is_empty() || target.is_empty() {
                warn!("handle_unfollow: missing follower or target in event data — dropping: {:?}", data);
                return Ok(());
            }

            // Reduce creator affinity without zeroing it (user might re-follow); floor at 0.2.
            sqlx::query(
                "UPDATE user_preferences SET \
                     creator_preferences = creator_preferences || \
                         jsonb_build_object($2, GREATEST( \
                             COALESCE((creator_preferences->$2)::float8, 0.3) - 0.2, \
                             0.2 \
                         )), \
                     updated_at = NOW() \
                 WHERE user_address = $1",
            )
            .bind(follower)
            .bind(target)
            .execute(&self.pool)
            .await
            .map_err(|e| Error::Database {
                message: "Failed to update creator affinity on unfollow".into(),
                source: Some(e),
            })?;

            // CC-003: invalidate caches after unfollow too — fire concurrently.
            if let Some(ref cache) = self.cache {
                tokio::join!(
                    cache.delete_following(follower),
                    cache.delete_user_prefs(follower),
                    cache.delete_recommendations(follower),
                    cache.delete_fof_all(follower),
                    cache.delete_user_suggestions(follower, target),
                );
            }

            // TAG-S26-03: best-effort — same reasoning as sync_follow above.
            if let Err(e) = self.graph_sync.sync_unfollow(follower, target).await {
                warn!("handle_unfollow: Nebula sync_unfollow failed (best-effort): {e}");
            }

            info!("Unfollow: {} -/-> {} (Nebula + prefs updated)", follower, target);
        }
        Ok(())
    }
}
