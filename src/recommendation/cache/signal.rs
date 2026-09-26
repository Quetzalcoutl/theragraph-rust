//! Session signal, topic affinity, Nebula query, and board cache operations on `RecCache`.

use std::collections::HashMap;

use redis::AsyncCommands;
use tracing::warn;

use super::keys::{CacheKey, GENRE_PREFS_TTL, NEBULA_QUERY_TTL, SESSION_TTL, STYLE_PREFS_TTL, SessionSignal};
use super::ops::RecCache;

impl RecCache {
    // ── Nebula query cache ────────────────────────────────────────────────────

    /// Try to get cached Nebula query output.
    pub async fn get_nebula_query(&self, query: &str) -> Option<String> {
        let hash = Self::hash_query(query);
        self.get_raw(&CacheKey::nebula(&hash)).await
    }

    /// Cache Nebula query output.
    pub async fn set_nebula_query(&self, query: &str, output: &str) {
        let hash = Self::hash_query(query);
        self.set_raw(&CacheKey::nebula(&hash), output, NEBULA_QUERY_TTL).await;
    }

    // ── Session recency signals ───────────────────────────────────────────────

    /// Prepend a session signal to the user's session list and trim to 20 entries.
    ///
    /// LPUSH + LTRIM + EXPIRE are issued as a single pipeline (one round-trip).
    /// Silently ignores errors — session signals are best-effort.
    pub async fn append_session_signal(&self, user_address: &str, signal: SessionSignal) {
        let key = CacheKey::session(user_address);
        let raw = match Self::encode(&signal) {
            Ok(s) => s,
            Err(e) => {
                warn!("SessionSignal serialize error for {}: {}", user_address, e);
                return;
            }
        };
        let mut conn = self.conn_clone();
        if let Err(e) = redis::pipe()
            .lpush(&key, &raw)
            .ignore()
            .ltrim(&key, 0, 19)
            .ignore()
            .expire(&key, SESSION_TTL as i64)
            .ignore()
            .query_async::<()>(&mut conn)
            .await
        {
            warn!("RecCache session signal pipeline failed for {}: {}", user_address, e);
        }
    }

    /// Return the most-recent session signals for a user (up to 20).
    ///
    /// Malformed entries are silently skipped.
    pub async fn get_session_signals(&self, user_address: &str) -> Vec<SessionSignal> {
        let key = CacheKey::session(user_address);
        let mut conn = self.conn_clone();
        let raws: Vec<Vec<u8>> = match conn.lrange::<_, Vec<Vec<u8>>>(&key, 0, 19).await {
            Ok(v) => v,
            Err(e) => {
                warn!("RecCache LRANGE session error for {}: {}", key, e);
                return vec![];
            }
        };
        raws.iter()
            .filter_map(|s| Self::decode::<SessionSignal>(s).ok())
            .collect()
    }

    // ── Topic affinity ────────────────────────────────────────────────────────

    /// Fetch topic affinity scores for a set of tags for a given user.
    ///
    /// Uses Redis MGET so all lookups are one round-trip.
    /// Keys are `topic_affinity:{addr}:{tag}` written by Elixir RecommendationSurfaceController.
    /// Returns a map of tag → affinity score (0.0–10.0). Missing keys are absent from the map.
    /// EFF-007: accepts &[&str] so callers can pass a Vec<&str> directly, avoiding
    /// O(unique_tags) String heap allocations on every feed request.
    pub async fn mget_topic_affinities(
        &self,
        user_address: &str,
        tags: &[&str],
    ) -> HashMap<Box<str>, f32> {
        if tags.is_empty() {
            return HashMap::new();
        }
        let addr_lower = user_address.to_lowercase();
        let keys: Vec<String> = tags
            .iter()
            .map(|t| CacheKey::topic_affinity(&addr_lower, t))
            .collect();

        let mut conn = self.conn_clone();
        let raw_vals: Vec<Option<String>> = match redis::cmd("MGET")
            .arg(&keys)
            .query_async(&mut conn)
            .await
        {
            Ok(v) => v,
            Err(e) => {
                warn!("RecCache MGET topic_affinity error: {}", e);
                return HashMap::new();
            }
        };

        tags.iter()
            .zip(raw_vals.iter())
            .filter_map(|(tag, raw)| {
                raw.as_ref()
                    .and_then(|s| s.parse::<f32>().ok())
                    .map(|score| ((*tag).into(), score))
            })
            .collect()
    }

    // ── Companion style preferences ───────────────────────────────────────────
    //
    // Cached read of style_preference Nebula edges written by
    // write_companion_preference_edges (AiFriendz chat → music/flix companion
    // signal). See graph_client::get_style_preferences for the traversal.

    pub async fn get_style_preferences(&self, user_address: &str) -> Option<HashMap<Box<str>, f32>> {
        self.get_json(&CacheKey::style_prefs(user_address)).await
    }

    pub async fn set_style_preferences(&self, user_address: &str, prefs: &HashMap<Box<str>, f32>) {
        self.set_json(&CacheKey::style_prefs(user_address), prefs, STYLE_PREFS_TTL).await;
    }

    // ── Declared genre preferences ────────────────────────────────────────────
    //
    // Cached read of genre_preference Nebula edges written by
    // write_genre_preference_edges (explicit listener-declared taste, up to 30
    // slugs) and any future companion-signal genre writes. See
    // graph_client::get_genre_preferences for the traversal.

    pub async fn get_genre_preferences(&self, user_address: &str) -> Option<HashMap<Box<str>, f32>> {
        self.get_json(&CacheKey::genre_prefs(user_address)).await
    }

    pub async fn set_genre_preferences(&self, user_address: &str, prefs: &HashMap<Box<str>, f32>) {
        self.set_json(&CacheKey::genre_prefs(user_address), prefs, GENRE_PREFS_TTL).await;
    }

    /// Invalidate the cached genre-preference read — call after
    /// `write_genre_preference_edges` so the next feed request within
    /// `GENRE_PREFS_TTL` sees the new declaration immediately instead of a
    /// stale (possibly empty) cached read.
    pub async fn delete_genre_preferences(&self, user_address: &str) {
        self.del_key(&CacheKey::genre_prefs(user_address)).await;
    }

    // ── Board cache ───────────────────────────────────────────────────────────

    pub async fn get_board<T: serde::de::DeserializeOwned>(&self, key: &str) -> Option<T> {
        self.get_json(&CacheKey::board(key)).await
    }

    pub async fn set_board<T: serde::Serialize>(&self, key: &str, value: &T, ttl_secs: u64) {
        self.set_json(&CacheKey::board(key), value, ttl_secs).await;
    }
}
