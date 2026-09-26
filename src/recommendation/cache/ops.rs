//! Generic Redis helpers and `RecCache` struct for the recommendation cache layer.
//!
//! Domain-specific methods live in sibling sub-modules:
//!   - `nft`    — NFT feature + seen-NFTs cache
//!   - `user`   — user preferences + following + recommendation results
//!   - `social` — FoF buckets + vertex bloom filter + "Who to Follow" suggestions
//!   - `signal` — session signals + topic affinity + Nebula query + boards
//!
//! All methods degrade gracefully: errors are logged and callers receive `None`
//! or an empty collection rather than propagating Redis failures.

use anyhow::Result;
use redis::aio::ConnectionManager;
use redis::AsyncCommands;
use serde::{de::DeserializeOwned, Serialize};
use std::sync::Arc;
use tracing::{debug, warn};

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
                Some(Self { conn: Arc::new(conn) })
            }
            Err(e) => {
                warn!("Redis ConnectionManager failed: {}", e);
                None
            }
        }
    }

    /// OB-01: Liveness probe — returns Ok(()) if Redis responds to PING.
    pub async fn ping(&self) -> Result<()> {
        let mut conn = self.conn_clone();
        let _: String = redis::cmd("PING")
            .query_async(&mut conn)
            .await
            .map_err(|e| anyhow::anyhow!("Redis ping failed: {}", e))?;
        Ok(())
    }

    /// Clone the underlying `ConnectionManager`.
    ///
    /// Child impl blocks (nft/user/social/signal) call this instead of
    /// accessing the private `conn` field directly so the field can remain
    /// `pub(super)` rather than fully `pub`.
    pub(super) fn conn_clone(&self) -> ConnectionManager {
        (*self.conn).clone()
    }

    // ── Single-key DEL ───────────────────────────────────────────────────────

    /// DEL one key, logging on error. Used by all single-key invalidation methods.
    pub(super) async fn del_key(&self, key: &str) {
        let mut conn = self.conn_clone();
        if let Err(e) = conn.del::<_, ()>(key).await {
            warn!("RecCache DEL {}: {}", key, e);
        }
    }

    // ── Generic helpers ──────────────────────────────────────────────────────

    /// Encode a value to its wire format (MessagePack, named-field mode).
    ///
    /// Single-point choke for the serialization format — this and `decode` below
    /// are the only two functions that know the on-wire encoding.
    ///
    /// Named-field mode (`to_vec_named`, a map keyed by field name) rather than
    /// compact/positional mode (`to_vec`, a bare array of values in field order):
    /// positional mode is unsafe for any type using `#[serde(skip_serializing_if)]`
    /// (as `ScoredNft.tags` does) because omitting one field desyncs every field
    /// after it — the decoder has no field names to re-synchronise against, unlike
    /// JSON's map representation. Named mode preserves JSON's exact missing-field
    /// semantics (a field is recovered via `#[serde(default)]`, same requirement
    /// either way) while still cutting payload size: binary ints/floats instead of
    /// ASCII digit strings, no quote/comma/colon punctuation.
    pub(super) fn encode<T: Serialize + ?Sized>(value: &T) -> Result<Vec<u8>, rmp_serde::encode::Error> {
        rmp_serde::to_vec_named(value)
    }

    /// Decode a value from its wire format (MessagePack).
    pub(super) fn decode<T: DeserializeOwned>(raw: &[u8]) -> Result<T, rmp_serde::decode::Error> {
        rmp_serde::from_slice(raw)
    }

    /// GET + deserialize. Returns None on miss or error.
    pub(super) async fn get_json<T: DeserializeOwned>(&self, key: &str) -> Option<T> {
        let mut conn = self.conn_clone();
        match conn.get::<_, Option<Vec<u8>>>(key).await {
            Ok(Some(raw)) => match Self::decode::<T>(&raw) {
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
    // RS-12: ?Sized allows passing &[T] (unsized slice reference) as well as &Vec<T>.
    pub(super) async fn set_json<T: Serialize + ?Sized>(&self, key: &str, value: &T, ttl_secs: u64) {
        let raw = match Self::encode(value) {
            Ok(b) => b,
            Err(e) => {
                warn!("RecCache serialize error for {}: {}", key, e);
                return;
            }
        };
        let mut conn = self.conn_clone();
        if let Err(e) = conn.set_ex::<_, _, ()>(key, &raw, ttl_secs).await {
            warn!("RecCache SETEX error for {}: {}", key, e);
        }
    }

    /// GET raw string. Returns None on miss or error.
    pub(super) async fn get_raw(&self, key: &str) -> Option<String> {
        let mut conn = self.conn_clone();
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
    pub(super) async fn set_raw(&self, key: &str, value: &str, ttl_secs: u64) {
        let mut conn = self.conn_clone();
        if let Err(e) = conn.set_ex::<_, _, ()>(key, value, ttl_secs).await {
            warn!("RecCache SETEX error (raw) for {}: {}", key, e);
        }
    }

    /// FNV-1a hash of query text → 16-char lowercase hex string.
    /// Fast, deterministic, no crypto overhead.
    pub(super) fn hash_query(query: &str) -> String {
        let mut hash: u64 = 0xcbf29ce484222325;
        for byte in query.bytes() {
            hash ^= byte as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
        format!("{:016x}", hash)
    }
}
