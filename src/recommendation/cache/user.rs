//! User-domain cache operations on `RecCache`.
//!
//! Covers: preferences, following list, recommendation results, and batch invalidation.

use redis::AsyncCommands;
use serde::{de::DeserializeOwned, Serialize};
use tracing::warn;

use super::keys::{CacheKey, FOLLOWING_TTL, RECOMMENDATION_TTL, USER_PREFS_TTL};
use super::ops::RecCache;
use super::super::schema_consts::{
    FEED_TYPE_ENHANCED, FEED_TYPE_FOLLOWING, FEED_TYPE_PERSONALIZED, FEED_TYPE_TRENDING,
};

impl RecCache {
    // ── User preferences ──────────────────────────────────────────────────────

    /// Get cached user preferences.
    pub async fn get_user_prefs<T: DeserializeOwned>(&self, user_address: &str) -> Option<T> {
        self.get_json(&CacheKey::prefs(user_address)).await
    }

    /// Cache user preferences.
    pub async fn set_user_prefs<T: Serialize>(&self, user_address: &str, prefs: &T) {
        self.set_json(&CacheKey::prefs(user_address), prefs, USER_PREFS_TTL).await;
    }

    /// Invalidate user preferences — call after any interaction that mutates prefs.
    pub async fn delete_user_prefs(&self, user_address: &str) {
        self.del_key(&CacheKey::prefs(user_address)).await;
    }

    // ── Following list ────────────────────────────────────────────────────────

    /// Get cached following list.
    pub async fn get_following(&self, user_address: &str) -> Option<Vec<String>> {
        self.get_json(&CacheKey::following(user_address)).await
    }

    /// Cache following list.
    pub async fn set_following(&self, user_address: &str, following: &[String]) {
        self.set_json(&CacheKey::following(user_address), &following, FOLLOWING_TTL).await;
    }

    /// Invalidate cached following list — call after follow/unfollow events.
    pub async fn delete_following(&self, user_address: &str) {
        self.del_key(&CacheKey::following(user_address)).await;
    }

    // ── Recommendation results ────────────────────────────────────────────────

    /// Get cached recommendation results.
    pub async fn get_recommendations<T: DeserializeOwned>(
        &self,
        user_address: &str,
        feed_type: &str,
    ) -> Option<T> {
        self.get_json(&CacheKey::recs(user_address, feed_type)).await
    }

    /// Cache recommendation results.
    // RS-12: T: ?Sized so callers can pass &[ScoredNft] directly (slice, not Vec).
    pub async fn set_recommendations<T: Serialize + ?Sized>(
        &self,
        user_address: &str,
        feed_type: &str,
        recs: &T,
    ) {
        self.set_json(&CacheKey::recs(user_address, feed_type), recs, RECOMMENDATION_TTL).await;
    }

    /// Invalidate all recommendation result caches for a user.
    /// EFF-005: single DEL with 4 keys in one round-trip instead of 4 sequential DELs (~1.5ms saved).
    pub async fn delete_recommendations(&self, user_address: &str) {
        let keys = [
            CacheKey::recs(user_address, FEED_TYPE_PERSONALIZED),
            CacheKey::recs(user_address, FEED_TYPE_ENHANCED),
            CacheKey::recs(user_address, FEED_TYPE_TRENDING),
            CacheKey::recs(user_address, FEED_TYPE_FOLLOWING),
        ];
        let mut conn = self.conn_clone();
        if let Err(e) = conn.del::<_, ()>(keys.as_slice()).await {
            warn!("RecCache DEL recs error for {}: {}", user_address, e);
        }
    }

    /// Invalidate one genre-filtered recommendation cache entry (GENRE-02
    /// follow-up) — narrower than [`delete_recommendations`](Self::delete_recommendations),
    /// which only targets the 4 fixed feed types. Genre feed types are
    /// dynamically hashed (see `super::keys::genre_feed_type`) so there is no
    /// fixed set of keys to enumerate; callers pass the exact `feed_type`
    /// already computed for the slug set being invalidated.
    pub async fn delete_recommendations_for_feed_type(&self, user_address: &str, feed_type: &str) {
        self.del_key(&CacheKey::recs(user_address, feed_type)).await;
    }

    /// Batch-invalidate prefs + recommendation caches for multiple users in two
    /// Redis DEL commands (one for all pref keys, one for all rec keys).
    ///
    /// Replaces N × 2 sequential DEL round-trips with exactly 2 round-trips
    /// regardless of the number of users. Used by apply_preference_decay which
    /// may affect thousands of users per daily sweep.
    pub async fn delete_user_caches_batch(&self, user_addresses: &[String]) {
        if user_addresses.is_empty() {
            return;
        }

        let pref_keys: Vec<String> = user_addresses
            .iter()
            .map(|addr| CacheKey::prefs(addr))
            .collect();

        // Each user has 4 recommendation feed types (personalized, enhanced, trending, following).
        let rec_keys: Vec<String> = user_addresses
            .iter()
            .flat_map(|addr| {
                [
                    CacheKey::recs(addr, FEED_TYPE_PERSONALIZED),
                    CacheKey::recs(addr, FEED_TYPE_ENHANCED),
                    CacheKey::recs(addr, FEED_TYPE_TRENDING),
                    CacheKey::recs(addr, FEED_TYPE_FOLLOWING),
                ]
            })
            .collect();

        let mut conn = self.conn_clone();
        if let Err(e) = conn.del::<_, ()>(pref_keys.as_slice()).await {
            warn!("RecCache batch DEL prefs failed ({} users): {}", user_addresses.len(), e);
        }
        if let Err(e) = conn.del::<_, ()>(rec_keys.as_slice()).await {
            warn!("RecCache batch DEL recs failed ({} users): {}", user_addresses.len(), e);
        }
    }
}
