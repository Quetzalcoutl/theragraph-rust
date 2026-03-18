//! Redis Cache Layer for Recommendation Engine
//!
//! Provides cache-aside pattern for Nebula graph queries, NFT features,
//! user preferences, and recommendation results.
//!
//! Joint optimization by Parity Technologies & Ferrous Systems:
//! - ConnectionManager for automatic reconnection
//! - Binary serialization with serde_json for complex types
//! - TTL-based expiration with configurable durations
//! - Graceful degradation: cache misses fall through to DB/Nebula

use anyhow::Result;
use redis::aio::ConnectionManager;
use redis::AsyncCommands;
use serde::{de::DeserializeOwned, Serialize};
use std::sync::Arc;
use tracing::{debug, warn};

/// TTL constants (seconds)
pub const NEBULA_QUERY_TTL: u64 = 300;       // 5 min — graph traversals change slowly
pub const NFT_FEATURES_TTL: u64 = 1800;      // 30 min — features are near-immutable
pub const USER_PREFS_TTL: u64 = 300;          // 5 min — preferences shift slowly
pub const FOLLOWING_TTL: u64 = 300;           // 5 min — follow graph changes infrequently
pub const SEEN_NFTS_TTL: u64 = 1800;         // 30 min — seen set for dedup
pub const RECOMMENDATION_TTL: u64 = 600;     // 10 min — cached recommendations

/// Key prefixes for namespace isolation
const PREFIX_NEBULA: &str = "rec:nebula:";
const PREFIX_FEATURES: &str = "rec:features:";
const PREFIX_PREFS: &str = "rec:prefs:";
const PREFIX_FOLLOWING: &str = "rec:following:";
const PREFIX_SEEN: &str = "rec:seen:";
const PREFIX_RECS: &str = "rec:results:";

/// Redis cache handle for the recommendation engine.
/// Uses `Arc` internally so cloning is cheap.
#[derive(Clone)]
pub struct RecCache {
    conn: Arc<ConnectionManager>,
}

impl RecCache {
    /// Create a new RecCache from a Redis URL.
    ///
    /// Falls back to `None` so callers can degrade gracefully.
    pub async fn connect(redis_url: &str) -> Option<Self> {
        let client = match redis::Client::open(redis_url) {
            Ok(c) => c,
            Err(e) => {
                warn!("Redis client creation failed: {}", e);
                return None;
            }
        };

        match ConnectionManager::new(client).await {
            Ok(conn) => {
                debug!("✅ RecCache connected to Redis");
                Some(Self {
                    conn: Arc::new(conn),
                })
            }
            Err(e) => {
                warn!("Redis ConnectionManager failed: {}", e);
                None
            }
        }
    }

    // ── Generic helpers ─────────────────────────────────────────────────

    /// GET + deserialize. Returns None on miss or error.
    async fn get_json<T: DeserializeOwned>(&self, key: &str) -> Option<T> {
        let mut conn = (*self.conn).clone();
        match conn.get::<_, Option<String>>(key).await {
            Ok(Some(raw)) => match serde_json::from_str::<T>(&raw) {
                Ok(val) => {
                    debug!("🎯 RecCache HIT: {}", key);
                    Some(val)
                }
                Err(e) => {
                    warn!("RecCache deserialize error for {}: {}", key, e);
                    None
                }
            },
            Ok(None) => None,
            Err(e) => {
                warn!("RecCache GET error for {}: {}", key, e);
                None
            }
        }
    }

    /// Serialize + SETEX. Silently ignores errors (cache is best-effort).
    async fn set_json<T: Serialize>(&self, key: &str, value: &T, ttl_secs: u64) {
        let raw = match serde_json::to_string(value) {
            Ok(s) => s,
            Err(e) => {
                warn!("RecCache serialize error for {}: {}", key, e);
                return;
            }
        };
        let mut conn = (*self.conn).clone();
        if let Err(e) = conn.set_ex::<_, _, ()>(key, &raw, ttl_secs).await {
            warn!("RecCache SETEX error for {}: {}", key, e);
        }
    }

    /// GET raw string. Returns None on miss or error.
    async fn get_raw(&self, key: &str) -> Option<String> {
        let mut conn = (*self.conn).clone();
        match conn.get::<_, Option<String>>(key).await {
            Ok(v) => {
                if v.is_some() {
                    debug!("🎯 RecCache HIT (raw): {}", key);
                }
                v
            }
            Err(e) => {
                warn!("RecCache GET error (raw) for {}: {}", key, e);
                None
            }
        }
    }

    /// SETEX raw string.
    async fn set_raw(&self, key: &str, value: &str, ttl_secs: u64) {
        let mut conn = (*self.conn).clone();
        if let Err(e) = conn.set_ex::<_, _, ()>(key, value, ttl_secs).await {
            warn!("RecCache SETEX error (raw) for {}: {}", key, e);
        }
    }

    // ── Nebula query cache ──────────────────────────────────────────────

    /// Build a cache key for a Nebula NGQL query (hash the query text).
    fn nebula_key(query_hash: &str) -> String {
        format!("{}{}", PREFIX_NEBULA, query_hash)
    }

    /// Try to get cached Nebula query output.
    pub async fn get_nebula_query(&self, query: &str) -> Option<String> {
        let hash = Self::hash_query(query);
        self.get_raw(&Self::nebula_key(&hash)).await
    }

    /// Cache Nebula query output.
    pub async fn set_nebula_query(&self, query: &str, output: &str) {
        let hash = Self::hash_query(query);
        self.set_raw(&Self::nebula_key(&hash), output, NEBULA_QUERY_TTL)
            .await;
    }

    // ── NFT features cache ─────────────────────────────────────────────

    /// Get cached NFT features.
    pub async fn get_nft_features<T: DeserializeOwned>(&self, nft_id: &str) -> Option<T> {
        let key = format!("{}{}", PREFIX_FEATURES, nft_id);
        self.get_json(&key).await
    }

    /// Cache NFT features.
    pub async fn set_nft_features<T: Serialize>(&self, nft_id: &str, features: &T) {
        let key = format!("{}{}", PREFIX_FEATURES, nft_id);
        self.set_json(&key, features, NFT_FEATURES_TTL).await;
    }

    // ── User preferences cache ──────────────────────────────────────────

    /// Get cached user preferences.
    pub async fn get_user_prefs<T: DeserializeOwned>(&self, user_address: &str) -> Option<T> {
        let key = format!("{}{}", PREFIX_PREFS, user_address.to_lowercase());
        self.get_json(&key).await
    }

    /// Cache user preferences.
    pub async fn set_user_prefs<T: Serialize>(&self, user_address: &str, prefs: &T) {
        let key = format!("{}{}", PREFIX_PREFS, user_address.to_lowercase());
        self.set_json(&key, prefs, USER_PREFS_TTL).await;
    }

    // ── Following addresses cache ───────────────────────────────────────

    /// Get cached following list.
    pub async fn get_following(&self, user_address: &str) -> Option<Vec<String>> {
        let key = format!("{}{}", PREFIX_FOLLOWING, user_address.to_lowercase());
        self.get_json(&key).await
    }

    /// Cache following list.
    pub async fn set_following(&self, user_address: &str, following: &[String]) {
        let key = format!("{}{}", PREFIX_FOLLOWING, user_address.to_lowercase());
        self.set_json(&key, &following, FOLLOWING_TTL).await;
    }

    // ── Seen NFTs (bulk check) ──────────────────────────────────────────

    /// Check if user has seen a specific NFT (uses Redis SET).
    pub async fn has_user_seen_nft(&self, user_address: &str, nft_id: &str) -> Option<bool> {
        let key = format!("{}{}", PREFIX_SEEN, user_address.to_lowercase());
        let mut conn = (*self.conn).clone();
        match conn.sismember::<_, _, bool>(&key, nft_id).await {
            Ok(v) => Some(v),
            Err(_) => None, // Cache miss — caller should query DB
        }
    }

    /// Mark a batch of NFT IDs as seen (Redis SET with TTL).
    pub async fn mark_nfts_seen(&self, user_address: &str, nft_ids: &[String]) {
        if nft_ids.is_empty() {
            return;
        }
        let key = format!("{}{}", PREFIX_SEEN, user_address.to_lowercase());
        let mut conn = (*self.conn).clone();

        // SADD all IDs in one roundtrip
        let _: Result<(), _> = redis::pipe()
            .atomic()
            .sadd(&key, nft_ids)
            .expire(&key, SEEN_NFTS_TTL as i64)
            .query_async(&mut conn)
            .await;
    }

    // ── Recommendation results cache ────────────────────────────────────

    /// Get cached recommendation results.
    pub async fn get_recommendations<T: DeserializeOwned>(
        &self,
        user_address: &str,
        feed_type: &str,
    ) -> Option<T> {
        let key = format!(
            "{}{}:{}",
            PREFIX_RECS,
            user_address.to_lowercase(),
            feed_type
        );
        self.get_json(&key).await
    }

    /// Cache recommendation results.
    pub async fn set_recommendations<T: Serialize>(
        &self,
        user_address: &str,
        feed_type: &str,
        recs: &T,
    ) {
        let key = format!(
            "{}{}:{}",
            PREFIX_RECS,
            user_address.to_lowercase(),
            feed_type
        );
        self.set_json(&key, recs, RECOMMENDATION_TTL).await;
    }

    // ── FoF recommendations cache ───────────────────────────────────────

    /// Get cached FoF graph recommendations.
    pub async fn get_fof_recommendations(
        &self,
        user_address: &str,
    ) -> Option<Vec<(String, f64)>> {
        let key = format!("rec:fof:{}", user_address.to_lowercase());
        self.get_json(&key).await
    }

    /// Cache FoF graph recommendations.
    pub async fn set_fof_recommendations(
        &self,
        user_address: &str,
        recs: &[(String, f64)],
    ) {
        let key = format!("rec:fof:{}", user_address.to_lowercase());
        self.set_json(&key, &recs, NEBULA_QUERY_TTL).await;
    }

    // ── Utility ─────────────────────────────────────────────────────────

    /// Simple FNV-1a hash of query text → hex string.
    /// Fast, deterministic, no crypto overhead.
    fn hash_query(query: &str) -> String {
        let mut hash: u64 = 0xcbf29ce484222325;
        for byte in query.bytes() {
            hash ^= byte as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
        format!("{:016x}", hash)
    }
}
