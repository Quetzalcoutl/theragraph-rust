//! NFT feature cache and seen-NFT operations on `RecCache`.

use std::collections::HashMap;

use redis::AsyncCommands;
use serde::{de::DeserializeOwned, Serialize};
use tracing::{debug, warn};

use super::keys::{CacheKey, GENRE_POOL_TTL, NFT_FEATURES_TTL, SEEN_NFTS_TTL};
use super::ops::RecCache;

impl RecCache {
    /// Get cached NFT features.
    pub async fn get_nft_features<T: DeserializeOwned>(&self, nft_id: &str) -> Option<T> {
        self.get_json(&CacheKey::features(nft_id)).await
    }

    /// Cache NFT features.
    pub async fn set_nft_features<T: Serialize>(&self, nft_id: &str, features: &T) {
        self.set_json(&CacheKey::features(nft_id), features, NFT_FEATURES_TTL).await;
    }

    /// Invalidate cached NFT features — call after saving updated features to DB.
    pub async fn delete_nft_features(&self, nft_id: &str) {
        self.del_key(&CacheKey::features(nft_id)).await;
    }

    // ── Shared genre-tagged candidate pool (GENRE-03) ─────────────────────────

    /// Get the shared (non-user-keyed) genre-tagged candidate pool for
    /// `genre_hash` (see `CacheKey::genre_pool` for why this is a distinct
    /// key from the per-user recommendation-result cache).
    pub async fn get_genre_pool<T: DeserializeOwned>(&self, genre_hash: &str) -> Option<T> {
        self.get_json(&CacheKey::genre_pool(genre_hash)).await
    }

    /// Cache the shared genre-tagged candidate pool for `genre_hash`.
    pub async fn set_genre_pool<T: Serialize + ?Sized>(&self, genre_hash: &str, pool: &T) {
        self.set_json(&CacheKey::genre_pool(genre_hash), pool, GENRE_POOL_TTL).await;
    }

    /// Batch-GET NFT features via a Redis MGET pipeline — single round-trip for N IDs.
    ///
    /// Returns a map of nft_id → deserialized value for keys that hit the cache.
    /// Missing or malformed entries are silently skipped; callers treat absence as a DB miss.
    pub async fn mget_nft_features<T: DeserializeOwned>(
        &self,
        nft_ids: &[&str],
    ) -> HashMap<String, T> {
        if nft_ids.is_empty() {
            return HashMap::new();
        }

        let keys: Vec<String> = nft_ids
            .iter()
            .map(|id| CacheKey::features(id))
            .collect();

        let mut conn = self.conn_clone();
        let raw_vals: Vec<Option<Vec<u8>>> = match redis::cmd("MGET")
            .arg(keys.as_slice())
            .query_async(&mut conn)
            .await
        {
            Ok(vals) => vals,
            Err(e) => {
                warn!("RecCache MGET error: {}", e);
                return HashMap::new();
            }
        };

        let mut result: HashMap<String, T> = HashMap::with_capacity(nft_ids.len());
        for (nft_id, raw) in nft_ids.iter().zip(raw_vals) {
            if let Some(s) = raw {
                match Self::decode::<T>(&s) {
                    Ok(val) => {
                        debug!("🎯 RecCache MGET HIT: {}", nft_id);
                        result.insert(nft_id.to_string(), val);
                    }
                    Err(e) => warn!("RecCache deserialize error for {}: {}", nft_id, e),
                }
            }
        }
        result
    }

    // ── Seen NFTs ─────────────────────────────────────────────────────────────

    /// Check if user has seen a specific NFT (uses Redis SET).
    pub async fn has_user_seen_nft(&self, user_address: &str, nft_id: &str) -> Option<bool> {
        let key = CacheKey::seen(user_address);
        let mut conn = self.conn_clone();
        match conn.sismember::<_, _, bool>(&key, nft_id).await {
            Ok(v) => Some(v),
            Err(_) => None,
        }
    }

    /// Mark a batch of NFT IDs as seen (Redis SET with TTL).
    pub async fn mark_nfts_seen(&self, user_address: &str, nft_ids: &[String]) {
        if nft_ids.is_empty() {
            return;
        }
        let key = CacheKey::seen(user_address);
        let mut conn = self.conn_clone();

        let _: anyhow::Result<(), _> = redis::pipe()
            .atomic()
            .sadd(&key, nft_ids)
            .expire(&key, SEEN_NFTS_TTL as i64)
            .query_async(&mut conn)
            .await;
    }
}
