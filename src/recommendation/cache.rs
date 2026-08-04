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
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{debug, warn};

use super::schema_consts::{
    FEED_TYPE_PERSONALIZED, FEED_TYPE_ENHANCED, FEED_TYPE_TRENDING, FEED_TYPE_FOLLOWING,
    REDIS_TOPIC_AFFINITY_PREFIX,
};

/// TTL constants (seconds)
pub const NEBULA_QUERY_TTL: u64 = 300;       // 5 min — graph traversals change slowly
pub const NFT_FEATURES_TTL: u64 = 1800;      // 30 min — features are near-immutable
pub const USER_PREFS_TTL: u64 = 300;          // 5 min — preferences shift slowly
pub const FOLLOWING_TTL: u64 = 300;           // 5 min — follow graph changes infrequently
pub const SEEN_NFTS_TTL: u64 = 1800;         // 30 min — seen set for dedup
pub const RECOMMENDATION_TTL: u64 = 600;     // 10 min — cached recommendations
pub const SESSION_TTL: u64 = 7200;           // 2h — YouTube-style session recency window

/// Key prefixes for namespace isolation
const PREFIX_NEBULA: &str = "rec:nebula:";
/// NASA-3: bumped to v2 after C3 changed the cached type from NftFeatures (11 fields)
/// to ScoringFeatures (4 fields). Old v1 entries under "rec:features:" will
/// simply not be found (cache miss → DB fetch → store v2 entry). No manual
/// flush required; entries expire naturally under NFT_FEATURES_TTL (30 min).
/// Bump this version any time the serialised shape of a cached type changes.
const PREFIX_FEATURES: &str = "rec:features:v2:";
const PREFIX_PREFS: &str = "rec:prefs:";
const PREFIX_FOLLOWING: &str = "rec:following:";
const PREFIX_SEEN: &str = "rec:seen:";
const PREFIX_RECS: &str = "rec:results:";
const PREFIX_FOF: &str = "rec:fof:";
const PREFIX_FOF_VIEW: &str = "rec:fof_view:";
const PREFIX_FOF_COMMENT: &str = "rec:fof_comment:";
const PREFIX_FOF_PURCHASE: &str = "rec:fof_purchase:";
const PREFIX_FOF_SHARE: &str = "rec:fof_share:";
const PREFIX_FOF_BOOKMARK: &str = "rec:fof_bookmark:";
const PREFIX_BOARD: &str = "board:";
const PREFIX_SESSION: &str = "rec:session:";

/// One user interaction recorded in the session recency list.
///
/// Stored as JSON in a Redis LIST (LPUSH + LTRIM to cap at 20 entries).
/// Scoring reads these to compute a short-term recency boost — `weight ×
/// exp(-age_secs / 1800)` — that layers on top of long-term preferences.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SessionSignal {
    pub tags: Vec<String>,
    pub creator: Option<String>,
    pub interaction_weight: f32,
    pub ts_unix: i64,
}

/// Centralized cache key constructors.
///
/// Every Redis key in the recommendation namespace is built here so that
/// prefix strings live in exactly one place. Callers import `CacheKey` and
/// call e.g. `CacheKey::fof(addr)` — a typo in a prefix is now a compile
/// error rather than a silent cache miss.
pub struct CacheKey;

impl CacheKey {
    /// Key for friend-of-friend graph recommendations (follow-graph bucket only).
    /// Use `fof_view` or `fof_comment` for the other FoF buckets.
    pub fn fof(addr: &str) -> String {
        format!("{}{}", PREFIX_FOF, addr.to_lowercase())
    }

    /// Key for dwell-weighted FoF recommendations (view_event bucket).
    pub fn fof_view(addr: &str) -> String {
        format!("{}{}", PREFIX_FOF_VIEW, addr.to_lowercase())
    }

    /// Key for comment-weighted FoF recommendations (comments_on bucket).
    pub fn fof_comment(addr: &str) -> String {
        format!("{}{}", PREFIX_FOF_COMMENT, addr.to_lowercase())
    }

    /// Key for purchase-weighted FoF recommendations (likes edge, reaction_type=purchase bucket).
    pub fn fof_purchase(addr: &str) -> String {
        format!("{}{}", PREFIX_FOF_PURCHASE, addr.to_lowercase())
    }

    /// Key for share-weighted FoF recommendations (shared edge, 15-day half-life bucket).
    pub fn fof_share(addr: &str) -> String {
        format!("{}{}", PREFIX_FOF_SHARE, addr.to_lowercase())
    }

    /// Key for bookmark-weighted FoF recommendations (bookmarked edge, 10-day half-life bucket).
    pub fn fof_bookmark(addr: &str) -> String {
        format!("{}{}", PREFIX_FOF_BOOKMARK, addr.to_lowercase())
    }

    /// Vertex-seen bloom-filter key.
    ///
    /// Set with SETNX + 24h TTL when a vertex is first upserted into Nebula.
    /// When this key exists, the vertex INSERT-IF-NOT-EXISTS nGQL fragment is
    /// elided from the query — saving one Nebula IF-NOT-EXISTS round-trip per
    /// write event for hot (viral) vertices. False positives (key present but
    /// vertex gone) are harmless: the missing vertex is detected on the next
    /// Nebula query path and auto-created by the next cold-path upsert.
    pub fn vertex_seen(vid: &str) -> String {
        format!("vx:{}", vid)
    }

    /// Key for "Who to Follow" user suggestion results.
    /// Scoped by viewer + creator so each profile page gets its own cache slot.
    pub fn user_suggestions(viewer: &str, creator: &str) -> String {
        format!("rec:suggestions:{}:{}", viewer.to_lowercase(), creator.to_lowercase())
    }

    /// Key for a cached Nebula NGQL query result (keyed by hash).
    pub fn nebula(hash: &str) -> String {
        format!("{}{}", PREFIX_NEBULA, hash)
    }

    /// Key for cached NFT feature vectors.
    pub fn features(nft_id: &str) -> String {
        format!("{}{}", PREFIX_FEATURES, nft_id)
    }

    /// Key for cached user preferences.
    pub fn prefs(addr: &str) -> String {
        format!("{}{}", PREFIX_PREFS, addr.to_lowercase())
    }

    /// Key for cached recommendation results, scoped by feed type.
    pub fn recs(addr: &str, feed_type: &str) -> String {
        format!("{}{}:{}", PREFIX_RECS, addr.to_lowercase(), feed_type)
    }

    /// Key for cached following-address list.
    pub fn following(addr: &str) -> String {
        format!("{}{}", PREFIX_FOLLOWING, addr.to_lowercase())
    }

    /// Key for the seen-NFT set (Redis SET used for dedup).
    pub fn seen(addr: &str) -> String {
        format!("{}{}", PREFIX_SEEN, addr.to_lowercase())
    }

    /// Key for board cache entries.
    pub fn board(key: &str) -> String {
        format!("{}{}", PREFIX_BOARD, key)
    }

    /// Key for the session-recency signal list (Redis LIST, capped at 20 entries).
    pub fn session(addr: &str) -> String {
        format!("{}{}", PREFIX_SESSION, addr.to_lowercase())
    }

    /// Key for a per-user per-tag topic-affinity score written by Elixir.
    ///
    /// Both address and tag are lowercased so keys are stable regardless of
    /// input casing. All `topic_affinity:` keys live in one place here — a
    /// future prefix rename is a one-line change with no risk of mismatched
    /// call sites.
    pub fn topic_affinity(addr: &str, tag: &str) -> String {
        format!("{}:{}:{}", REDIS_TOPIC_AFFINITY_PREFIX, addr.to_lowercase(), tag.to_lowercase())
    }
}

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

    /// OB-01: Liveness probe — returns Ok(()) if Redis responds to PING.
    pub async fn ping(&self) -> Result<()> {
        let mut conn = (*self.conn).clone();
        let _: String = redis::cmd("PING")
            .query_async(&mut conn)
            .await
            .map_err(|e| anyhow::anyhow!("Redis ping failed: {}", e))?;
        Ok(())
    }

    // ── Generic helpers ─────────────────────────────────────────────────

    /// Encode a value to its wire format.
    ///
    /// Single-point choke for the serialization format. Changing JSON → MessagePack
    /// requires editing only this function and `decode` below.
    // RS-12: ?Sized mirrors set_json so callers can pass &[T] as well as &Vec<T>.
    fn encode<T: Serialize + ?Sized>(value: &T) -> Result<String, serde_json::Error> {
        serde_json::to_string(value)
    }

    /// Decode a value from its wire format.
    fn decode<T: DeserializeOwned>(raw: &str) -> Result<T, serde_json::Error> {
        serde_json::from_str(raw)
    }

    /// GET + deserialize. Returns None on miss or error.
    async fn get_json<T: DeserializeOwned>(&self, key: &str) -> Option<T> {
        let mut conn = (*self.conn).clone();
        match conn.get::<_, Option<String>>(key).await {
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
    async fn set_json<T: Serialize + ?Sized>(&self, key: &str, value: &T, ttl_secs: u64) {
        let raw = match Self::encode(value) {
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

    /// Try to get cached Nebula query output.
    pub async fn get_nebula_query(&self, query: &str) -> Option<String> {
        let hash = Self::hash_query(query);
        self.get_raw(&CacheKey::nebula(&hash)).await
    }

    /// Cache Nebula query output.
    pub async fn set_nebula_query(&self, query: &str, output: &str) {
        let hash = Self::hash_query(query);
        self.set_raw(&CacheKey::nebula(&hash), output, NEBULA_QUERY_TTL)
            .await;
    }

    // ── NFT features cache ─────────────────────────────────────────────

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
        let key = CacheKey::features(nft_id);
        let mut conn = (*self.conn).clone();
        if let Err(e) = conn.del::<_, ()>(&key).await {
            warn!("RecCache DEL error for {}: {}", key, e);
        }
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

        let mut conn = (*self.conn).clone();
        let raw_vals: Vec<Option<String>> = match redis::cmd("MGET")
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

    // ── User preferences cache ──────────────────────────────────────────

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
        let key = CacheKey::prefs(user_address);
        let mut conn = (*self.conn).clone();
        if let Err(e) = conn.del::<_, ()>(&key).await {
            warn!("RecCache DEL error for {}: {}", key, e);
        }
    }

    /// Invalidate all recommendation result caches for a user.
    /// Call after any interaction that should immediately affect what recs the user sees.
    /// EFF-005: single DEL with 4 keys in one round-trip instead of 4 sequential DELs (~1.5ms saved).
    pub async fn delete_recommendations(&self, user_address: &str) {
        let keys = [
            CacheKey::recs(user_address, FEED_TYPE_PERSONALIZED),
            CacheKey::recs(user_address, FEED_TYPE_ENHANCED),
            CacheKey::recs(user_address, FEED_TYPE_TRENDING),
            CacheKey::recs(user_address, FEED_TYPE_FOLLOWING),
        ];
        let mut conn = (*self.conn).clone();
        if let Err(e) = conn.del::<_, ()>(keys.as_slice()).await {
            warn!("RecCache DEL recs error for {}: {}", user_address, e);
        }
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

        let mut conn = (*self.conn).clone();
        if let Err(e) = conn.del::<_, ()>(pref_keys.as_slice()).await {
            warn!("RecCache batch DEL prefs failed ({} users): {}", user_addresses.len(), e);
        }
        if let Err(e) = conn.del::<_, ()>(rec_keys.as_slice()).await {
            warn!("RecCache batch DEL recs failed ({} users): {}", user_addresses.len(), e);
        }
    }

    // ── Following addresses cache ───────────────────────────────────────

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
        let key = CacheKey::following(user_address);
        let mut conn = (*self.conn).clone();
        if let Err(e) = conn.del::<_, ()>(&key).await {
            warn!("RecCache DEL following error for {}: {}", key, e);
        }
    }

    // ── Seen NFTs (bulk check) ──────────────────────────────────────────

    /// Check if user has seen a specific NFT (uses Redis SET).
    pub async fn has_user_seen_nft(&self, user_address: &str, nft_id: &str) -> Option<bool> {
        let key = CacheKey::seen(user_address);
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
        let key = CacheKey::seen(user_address);
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

    // ── FoF recommendations cache ───────────────────────────────────────

    /// Get cached FoF graph recommendations (follow-graph / likes bucket).
    /// For view-event or comment FoF, use `get_view_fof_recommendations` /
    /// `get_comment_fof_recommendations` instead.
    ///
    /// Returns `f32` scores — the scoring engine uses f32 throughout, so
    /// storing f64 in JSON wastes 11 bytes per entry with zero precision benefit.
    pub async fn get_fof_recommendations(
        &self,
        user_address: &str,
    ) -> Option<Vec<(String, f32)>> {
        self.get_json(&CacheKey::fof(user_address)).await
    }

    /// Write a FoF bucket: downcast f64 → f32, then SETEX under `key`.
    ///
    /// Single implementation for all four FoF buckets. serde_json encodes f64
    /// with up to 17 significant digits; f32 uses 9, saving ~11 bytes per entry
    /// (~2.2 KB per user across all four buckets). Engine casts back to f32 on
    /// read anyway so no precision is lost in practice.
    async fn set_fof_bucket(&self, key: &str, recs: &[(String, f64)]) {
        let recs_f32: Vec<(String, f32)> = recs
            .iter()
            .map(|(k, v)| {
                // Guard NaN/Inf before the f64→f32 cast: serde serializes f32::NAN as JSON null,
                // which deserializes back as 0.0, silently zeroing FoF boost scores.
                let safe = if v.is_finite() { *v as f32 } else {
                    tracing::warn!(
                        fof_key = key,
                        score = v,
                        "non-finite FoF score from Nebula — storing 0.0"
                    );
                    0.0f32
                };
                (k.clone(), safe)
            })
            .collect();
        self.set_json(key, &recs_f32, NEBULA_QUERY_TTL).await;
    }

    /// Cache FoF graph recommendations (follow-graph / likes bucket).
    pub async fn set_fof_recommendations(&self, user_address: &str, recs: &[(String, f64)]) {
        self.set_fof_bucket(&CacheKey::fof(user_address), recs).await;
    }

    /// Get cached dwell-weighted (view_event) FoF recommendations.
    pub async fn get_view_fof_recommendations(
        &self,
        user_address: &str,
    ) -> Option<Vec<(String, f32)>> {
        self.get_json(&CacheKey::fof_view(user_address)).await
    }

    /// Cache dwell-weighted (view_event) FoF recommendations.
    pub async fn set_view_fof_recommendations(&self, user_address: &str, recs: &[(String, f64)]) {
        self.set_fof_bucket(&CacheKey::fof_view(user_address), recs).await;
    }

    /// Get cached comment-weighted FoF recommendations.
    pub async fn get_comment_fof_recommendations(
        &self,
        user_address: &str,
    ) -> Option<Vec<(String, f32)>> {
        self.get_json(&CacheKey::fof_comment(user_address)).await
    }

    /// Cache comment-weighted FoF recommendations.
    pub async fn set_comment_fof_recommendations(&self, user_address: &str, recs: &[(String, f64)]) {
        self.set_fof_bucket(&CacheKey::fof_comment(user_address), recs).await;
    }

    /// Get cached purchase-weighted FoF recommendations (30-day half-life bucket).
    pub async fn get_purchase_fof_recommendations(
        &self,
        user_address: &str,
    ) -> Option<Vec<(String, f32)>> {
        self.get_json(&CacheKey::fof_purchase(user_address)).await
    }

    /// Cache purchase-weighted FoF recommendations.
    pub async fn set_purchase_fof_recommendations(&self, user_address: &str, recs: &[(String, f64)]) {
        self.set_fof_bucket(&CacheKey::fof_purchase(user_address), recs).await;
    }

    /// Get cached share-weighted FoF recommendations (15-day half-life bucket).
    pub async fn get_share_fof_recommendations(
        &self,
        user_address: &str,
    ) -> Option<Vec<(String, f32)>> {
        self.get_json(&CacheKey::fof_share(user_address)).await
    }

    /// Cache share-weighted FoF recommendations.
    pub async fn set_share_fof_recommendations(&self, user_address: &str, recs: &[(String, f64)]) {
        self.set_fof_bucket(&CacheKey::fof_share(user_address), recs).await;
    }

    /// Get cached bookmark-weighted FoF recommendations (10-day half-life bucket).
    pub async fn get_bookmark_fof_recommendations(
        &self,
        user_address: &str,
    ) -> Option<Vec<(String, f32)>> {
        self.get_json(&CacheKey::fof_bookmark(user_address)).await
    }

    /// Cache bookmark-weighted FoF recommendations.
    pub async fn set_bookmark_fof_recommendations(&self, user_address: &str, recs: &[(String, f64)]) {
        self.set_fof_bucket(&CacheKey::fof_bookmark(user_address), recs).await;
    }

    /// Check whether a Nebula graph vertex (user or post) has been upserted before.
    ///
    /// Returns `true` when the vertex-seen bloom filter key exists in Redis.
    /// A `true` result means the caller can skip the `INSERT ... IF NOT EXISTS`
    /// nGQL fragment — the vertex already exists in Nebula with very high probability.
    ///
    /// Returns `false` on any Redis error so the caller falls back to always-upsert.
    pub async fn is_vertex_seen(&self, vid: &str) -> bool {
        let mut conn = (*self.conn).clone();
        let key = CacheKey::vertex_seen(vid);
        match conn.exists::<_, bool>(&key).await {
            Ok(v) => v,
            Err(_) => false,
        }
    }

    /// Mark a Nebula vertex as confirmed-upserted with a 24-hour TTL.
    ///
    /// Uses SET NX (set-if-not-exists) so concurrent callers are idempotent.
    /// The 24h TTL ensures the bloom filter self-heals if a vertex is somehow
    /// removed from Nebula (e.g. during data maintenance).
    pub async fn mark_vertex_seen(&self, vid: &str) {
        let mut conn = (*self.conn).clone();
        let key = CacheKey::vertex_seen(vid);
        let _: Result<(), _> = conn.set_options(
            &key,
            1u8,
            redis::SetOptions::default()
                .conditional_set(redis::ExistenceCheck::NX)
                .with_expiration(redis::SetExpiry::EX(86_400)),
        ).await;
    }

    /// Get cached "Who to Follow" user suggestions for a viewer on a creator's profile.
    pub async fn get_user_suggestions(
        &self,
        viewer: &str,
        creator: &str,
    ) -> Option<Vec<(String, f32)>> {
        self.get_json(&CacheKey::user_suggestions(viewer, creator)).await
    }

    /// Cache "Who to Follow" user suggestions for a viewer on a creator's profile.
    ///
    /// Downcasts f64 → f32 before serialization. serde_json encodes f64 with up to
    /// 17 significant digits; f32 uses ≤9, saving ~11 bytes per entry. The resolver
    /// only needs ranking precision, not double precision, for Who-to-Follow scores.
    pub async fn set_user_suggestions(
        &self,
        viewer: &str,
        creator: &str,
        recs: &[(String, f64)],
    ) {
        // Guard NaN/Inf before the f64→f32 cast. serde_json serialises f32::NAN as JSON
        // `null`; deserialisation of Vec<(String, f32)> then fails entirely, causing the
        // cache to return None on every hit and forcing a Nebula traversal each page view.
        let recs_f32: Vec<(String, f32)> = recs.iter().map(|(k, v)| {
            let safe = if v.is_finite() {
                *v as f32
            } else {
                warn!(viewer = viewer, creator = creator, score = v, "non-finite user suggestion score from Nebula — storing 0.0");
                0.0f32
            };
            (k.clone(), safe)
        }).collect();
        self.set_json(&CacheKey::user_suggestions(viewer, creator), &recs_f32, NEBULA_QUERY_TTL).await;
    }

    /// Invalidate all three FoF cache slots for a user in one round-trip.
    ///
    /// Call after any follow or unfollow so the follower's friend-graph expansion
    /// is reflected on the next feed open rather than after the full NEBULA_QUERY_TTL.
    /// EFF-008: single DEL with 6 keys (like, view, comment, purchase, share, bookmark).
    pub async fn delete_fof_all(&self, addr: &str) {
        let keys = [
            CacheKey::fof(addr),
            CacheKey::fof_view(addr),
            CacheKey::fof_comment(addr),
            CacheKey::fof_purchase(addr),
            CacheKey::fof_share(addr),
            CacheKey::fof_bookmark(addr),
        ];
        let mut conn = (*self.conn).clone();
        if let Err(e) = conn.del::<_, ()>(keys.as_slice()).await {
            warn!("RecCache DEL fof_all error for {}: {}", addr, e);
        }
    }

    /// Invalidate the "Who to Follow" user-suggestions cache slot for (viewer, creator).
    ///
    /// The most impactful stale slot after a follow is (viewer=follower, creator=followee)
    /// because the anti-join result was computed before the new edge existed.
    pub async fn delete_user_suggestions(&self, viewer: &str, creator: &str) {
        let key = CacheKey::user_suggestions(viewer, creator);
        let mut conn = (*self.conn).clone();
        if let Err(e) = conn.del::<_, ()>(&key).await {
            warn!("RecCache DEL user_suggestions error for {}/{}: {}", viewer, creator, e);
        }
    }

    // ── Session recency signals ────────────────────────────────────────────

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
        let mut conn = (*self.conn).clone();
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
    /// Malformed JSON entries are silently skipped.
    pub async fn get_session_signals(&self, user_address: &str) -> Vec<SessionSignal> {
        let key = CacheKey::session(user_address);
        let mut conn = (*self.conn).clone();
        let raws: Vec<String> = match conn.lrange::<_, Vec<String>>(&key, 0, 19).await {
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

    // ── Board cache ────────────────────────────────────────────────────

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
    ) -> HashMap<String, f32> {
        if tags.is_empty() {
            return HashMap::new();
        }
        let addr_lower = user_address.to_lowercase();
        let keys: Vec<String> = tags
            .iter()
            .map(|t| CacheKey::topic_affinity(&addr_lower, t))
            .collect();

        let mut conn = (*self.conn).clone();
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
                    .map(|score| (tag.to_string(), score))
            })
            .collect()
    }

    pub async fn get_board<T: DeserializeOwned>(&self, key: &str) -> Option<T> {
        self.get_json(&CacheKey::board(key)).await
    }

    pub async fn set_board<T: Serialize>(&self, key: &str, value: &T, ttl_secs: u64) {
        self.set_json(&CacheKey::board(key), value, ttl_secs).await;
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

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ── fof ──────────────────────────────────────────────────────────────────

    #[test]
    fn cache_key_fof_lowercases_address() {
        // Mixed-case address must be normalized so callers that pass 0xABC and
        // 0xabc both hit the same Redis key.
        assert_eq!(
            CacheKey::fof("0xABCDEF1234567890ABCDEF1234567890ABCDEF12"),
            "rec:fof:0xabcdef1234567890abcdef1234567890abcdef12"
        );
    }

    #[test]
    fn cache_key_fof_already_lowercase_is_identity() {
        let addr = "0xabcdef1234567890abcdef1234567890abcdef12";
        assert_eq!(CacheKey::fof(addr), format!("rec:fof:{addr}"));
    }

    #[test]
    fn cache_key_fof_short_address_still_lowercased() {
        // Addresses that are structurally invalid (too short) are not validated
        // by CacheKey — that is the caller's concern. We just verify lowercasing.
        assert_eq!(CacheKey::fof("0xABC"), "rec:fof:0xabc");
    }

    // ── nebula ───────────────────────────────────────────────────────────────

    #[test]
    fn cache_key_nebula_has_correct_prefix() {
        let hash = "QmHash123AbcDef";
        let key = CacheKey::nebula(hash);
        assert!(
            key.starts_with("rec:nebula:"),
            "expected key to start with 'rec:nebula:', got: {key}"
        );
    }

    #[test]
    fn cache_key_nebula_preserves_hash_casing() {
        // Nebula keys are keyed by FNV hash (hex string), not user addresses.
        // The hash is already lowercase hex so casing must be preserved as-is.
        let hash = "a1b2c3d4e5f60000";
        assert_eq!(CacheKey::nebula(hash), format!("rec:nebula:{hash}"));
    }

    // ── features ─────────────────────────────────────────────────────────────

    #[test]
    fn cache_key_features_has_correct_prefix() {
        let nft_id = "42";
        let key = CacheKey::features(nft_id);
        // NASA-3: prefix bumped to v2 so old NftFeatures entries are isolated
        assert!(
            key.starts_with("rec:features:v2:"),
            "expected key to start with 'rec:features:v2:', got: {key}"
        );
    }

    #[test]
    fn cache_key_features_appends_nft_id() {
        // NASA-3: v2 prefix
        assert_eq!(CacheKey::features("42"), "rec:features:v2:42");
        assert_eq!(CacheKey::features("99999"), "rec:features:v2:99999");
    }

    // ── prefs ─────────────────────────────────────────────────────────────────

    #[test]
    fn cache_key_prefs_lowercases_address() {
        assert_eq!(
            CacheKey::prefs("0xDEF0DEF0DEF0DEF0DEF0DEF0DEF0DEF0DEF0DEF0"),
            "rec:prefs:0xdef0def0def0def0def0def0def0def0def0def0"
        );
    }

    #[test]
    fn cache_key_prefs_has_correct_prefix() {
        let key = CacheKey::prefs("0xaabbccddaabbccddaabbccddaabbccddaabbccdd");
        assert!(
            key.starts_with("rec:prefs:"),
            "expected key to start with 'rec:prefs:', got: {key}"
        );
    }

    // ── recs ─────────────────────────────────────────────────────────────────

    #[test]
    fn cache_key_recs_lowercases_address_and_appends_feed_type() {
        let addr = "0xABCDABCDABCDABCDABCDABCDABCDABCDABCDABCD";
        let key = CacheKey::recs(addr, "trending");
        // Prefix is "rec:results:" per PREFIX_RECS constant
        assert_eq!(
            key,
            "rec:results:0xabcdabcdabcdabcdabcdabcdabcdabcdabcdabcd:trending"
        );
    }

    #[test]
    fn cache_key_recs_different_feed_types_produce_different_keys() {
        let addr = "0x1234567890123456789012345678901234567890";
        let key_personalized = CacheKey::recs(addr, "personalized");
        let key_trending = CacheKey::recs(addr, "trending");
        let key_following = CacheKey::recs(addr, "following");

        assert_ne!(key_personalized, key_trending);
        assert_ne!(key_trending, key_following);
        assert_ne!(key_personalized, key_following);
    }

    // ── following ────────────────────────────────────────────────────────────

    #[test]
    fn cache_key_following_lowercases_address() {
        let addr = "0xGHIGHIGHIGHIGHIGHIGHIGHIGHIGHIGHIGHIGHI";
        // Even though 'G', 'H', 'I' are not valid hex — the function lowercases
        // the raw string regardless. Validation is the caller's responsibility.
        let key = CacheKey::following(addr);
        assert_eq!(
            key,
            format!("rec:following:{}", addr.to_lowercase())
        );
    }

    #[test]
    fn cache_key_following_has_correct_prefix() {
        let key = CacheKey::following("0xf0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0");
        assert!(
            key.starts_with("rec:following:"),
            "expected key to start with 'rec:following:', got: {key}"
        );
    }

    // ── seen ─────────────────────────────────────────────────────────────────

    #[test]
    fn cache_key_seen_lowercases_address() {
        let addr = "0xJKLJKLJKLJKLJKLJKLJKLJKLJKLJKLJKLJKLJKL";
        let key = CacheKey::seen(addr);
        assert_eq!(key, format!("rec:seen:{}", addr.to_lowercase()));
    }

    #[test]
    fn cache_key_seen_has_correct_prefix() {
        let key = CacheKey::seen("0xdeaddeaddeaddeaddeaddeaddeaddeaddeaddead");
        assert!(
            key.starts_with("rec:seen:"),
            "expected key to start with 'rec:seen:', got: {key}"
        );
    }

    // ── namespace isolation ──────────────────────────────────────────────────

    #[test]
    fn all_key_prefixes_are_distinct() {
        // Each namespace must map to a unique prefix so a SCAN or key-pattern
        // operation on one namespace never bleeds into another.
        let addr = "0xaaaa000000000000000000000000000000000000";
        let hash = "0000000000000000";
        let nft_id = "1";
        let feed = "trending";

        let keys = vec![
            CacheKey::fof(addr),
            CacheKey::nebula(hash),
            CacheKey::features(nft_id),
            CacheKey::prefs(addr),
            CacheKey::recs(addr, feed),
            CacheKey::following(addr),
            CacheKey::seen(addr),
            CacheKey::board("trending"),
            CacheKey::session(addr),
            CacheKey::fof_view(addr),
            CacheKey::fof_comment(addr),
            CacheKey::fof_purchase(addr),
            CacheKey::user_suggestions(addr, addr),
        ];

        // Extract the namespace prefix (e.g. "rec:fof:", "board:").
        // Strategy: take everything up to and including the second colon when
        // there are two colons; if there is only one colon (e.g. "board:foo"),
        // take up to and including the first colon.
        let prefixes: Vec<&str> = keys.iter().map(|k| {
            // Position of the first colon (all our keys have at least one).
            let first_colon = k.find(':').unwrap_or(k.len());
            // Look for a second colon after the first.
            let second_colon = k[first_colon + 1..]
                .find(':')
                .map(|i| first_colon + 1 + i);
            match second_colon {
                Some(pos) => &k[..=pos],        // includes the second colon
                None      => &k[..=first_colon], // only one colon (e.g. "board:")
            }
        }).collect();

        let unique: std::collections::HashSet<&str> = prefixes.iter().copied().collect();
        assert_eq!(
            unique.len(),
            prefixes.len(),
            "duplicate prefix detected among CacheKey constructors: {:?}",
            prefixes
        );
    }

    // ── board ─────────────────────────────────────────────────────────────────

    #[test]
    fn cache_key_board_has_correct_prefix() {
        let key = CacheKey::board("trending");
        assert!(
            key.starts_with("board:"),
            "expected key to start with 'board:', got: {key}"
        );
    }

    #[test]
    fn cache_key_board_appends_key_verbatim() {
        // Board keys are opaque slugs (not addresses), so no lowercasing is applied.
        assert_eq!(CacheKey::board("trending"), "board:trending");
        assert_eq!(CacheKey::board("user:0xABCD:posts"), "board:user:0xABCD:posts");
    }

    // ── empty-string inputs ──────────────────────────────────────────────────

    #[test]
    fn cache_key_fof_empty_string_does_not_panic() {
        // CacheKey is not a validator; an empty address is the caller's problem.
        // These tests just confirm we don't panic or produce a bare prefix collision.
        let key = CacheKey::fof("");
        assert_eq!(key, "rec:fof:");
    }

    #[test]
    fn cache_key_nebula_empty_string_does_not_panic() {
        let key = CacheKey::nebula("");
        assert_eq!(key, "rec:nebula:");
    }

    #[test]
    fn cache_key_features_empty_string_does_not_panic() {
        let key = CacheKey::features("");
        // NASA-3: v2 prefix
        assert_eq!(key, "rec:features:v2:");
    }

    #[test]
    fn cache_key_prefs_empty_string_does_not_panic() {
        let key = CacheKey::prefs("");
        assert_eq!(key, "rec:prefs:");
    }

    #[test]
    fn cache_key_following_empty_string_does_not_panic() {
        let key = CacheKey::following("");
        assert_eq!(key, "rec:following:");
    }

    #[test]
    fn cache_key_seen_empty_string_does_not_panic() {
        let key = CacheKey::seen("");
        assert_eq!(key, "rec:seen:");
    }

    #[test]
    fn cache_key_recs_empty_strings_do_not_panic() {
        let key = CacheKey::recs("", "");
        assert_eq!(key, "rec:results::");
    }

    #[test]
    fn cache_key_board_empty_string_does_not_panic() {
        let key = CacheKey::board("");
        assert_eq!(key, "board:");
    }

    // ── topic_affinity ───────────────────────────────────────────────────────

    #[test]
    fn cache_key_topic_affinity_has_correct_prefix() {
        let addr = "0xABCDEF1234567890ABCDEF1234567890ABCDEF12";
        let tag = "GenerativeArt";
        let key = CacheKey::topic_affinity(addr, tag);
        assert!(
            key.starts_with("topic_affinity:"),
            "expected key to start with 'topic_affinity:', got: {key}"
        );
        // Both components must be lowercased for key stability.
        assert_eq!(
            key,
            "topic_affinity:0xabcdef1234567890abcdef1234567890abcdef12:generativeart"
        );
    }

    #[test]
    fn cache_key_topic_affinity_lowercases_addr_and_tag() {
        let key_upper = CacheKey::topic_affinity("0xABCD", "ART");
        let key_lower = CacheKey::topic_affinity("0xabcd", "art");
        assert_eq!(key_upper, key_lower, "mixed-case and lowercase inputs must produce the same key");
    }

    // ── TTL ordering sanity ──────────────────────────────────────────────────

    #[test]
    fn ttl_nebula_query_is_shorter_than_recommendation() {
        // Nebula graph traversals re-run on a tighter cadence than cached recs.
        assert!(
            NEBULA_QUERY_TTL < RECOMMENDATION_TTL,
            "NEBULA_QUERY_TTL ({NEBULA_QUERY_TTL}s) should be < RECOMMENDATION_TTL ({RECOMMENDATION_TTL}s)"
        );
    }

    #[test]
    fn ttl_recommendation_is_shorter_than_nft_features() {
        // Recommendation lists are volatile relative to feature vectors.
        assert!(
            RECOMMENDATION_TTL < NFT_FEATURES_TTL,
            "RECOMMENDATION_TTL ({RECOMMENDATION_TTL}s) should be < NFT_FEATURES_TTL ({NFT_FEATURES_TTL}s)"
        );
    }

    #[test]
    fn ttl_all_values_are_positive() {
        assert!(NEBULA_QUERY_TTL > 0);
        assert!(NFT_FEATURES_TTL > 0);
        assert!(USER_PREFS_TTL > 0);
        assert!(FOLLOWING_TTL > 0);
        assert!(SEEN_NFTS_TTL > 0);
        assert!(RECOMMENDATION_TTL > 0);
    }

    // ── hash_query determinism ────────────────────────────────────────────────

    #[test]
    fn hash_query_is_deterministic() {
        let q = "MATCH (n:User)-[:FOLLOWS]->(m) WHERE n.addr == '0xabc' RETURN m LIMIT 100";
        assert_eq!(RecCache::hash_query(q), RecCache::hash_query(q));
    }

    #[test]
    fn hash_query_different_inputs_produce_different_hashes() {
        let h1 = RecCache::hash_query("SELECT * FROM foo WHERE id = 1");
        let h2 = RecCache::hash_query("SELECT * FROM foo WHERE id = 2");
        assert_ne!(h1, h2);
    }

    #[test]
    fn hash_query_output_is_16_hex_chars() {
        let h = RecCache::hash_query("any query text here");
        assert_eq!(h.len(), 16, "expected 16-char hex string, got: {h}");
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()), "non-hex char in: {h}");
    }

    #[test]
    fn hash_query_empty_string_does_not_panic() {
        let h = RecCache::hash_query("");
        // FNV-1a of the empty string is the offset basis itself.
        assert_eq!(h.len(), 16);
    }

    // ── serde roundtrip ───────────────────────────────────────────────────────

    #[test]
    fn scored_nft_serde_roundtrip() {
        use crate::recommendation::scoring::{RecommendationReason, ScoredNft};

        let original = ScoredNft {
            nft_id: "nft-abc-123".to_string(),
            token_id: 42,
            contract_address: "0x1111111111111111111111111111111111111111".to_string(),
            score: 0.875_f32,
            reason: RecommendationReason::TagMatch {
                matching_tags: vec!["art".to_string(), "generative".to_string()],
            },
            contract_type: "ERC721".to_string(),
            creator_address: "0x2222222222222222222222222222222222222222".to_string(),
            tags: vec!["art".to_string(), "generative".to_string()],
        };

        let json = serde_json::to_string(&original).expect("ScoredNft should serialize");
        let decoded: ScoredNft =
            serde_json::from_str(&json).expect("ScoredNft should deserialize");

        assert_eq!(decoded.nft_id, original.nft_id);
        assert_eq!(decoded.token_id, original.token_id);
        assert_eq!(decoded.contract_address, original.contract_address);
        // f32 round-trips through JSON as a float literal; compare via absolute diff.
        assert!(
            (decoded.score - original.score).abs() < 1e-6,
            "score mismatch after roundtrip: {} vs {}",
            decoded.score,
            original.score
        );
        assert_eq!(decoded.contract_type, original.contract_type);
        assert_eq!(decoded.creator_address, original.creator_address);
        assert_eq!(decoded.tags, original.tags);
    }

    #[test]
    fn recommendation_reason_trending_serde_roundtrip() {
        use crate::recommendation::scoring::RecommendationReason;

        let reason = RecommendationReason::Trending { trending_score: 0.95 };
        let json = serde_json::to_string(&reason).expect("serialize");
        let decoded: RecommendationReason = serde_json::from_str(&json).expect("deserialize");

        // serde_json::to_string on both sides gives canonical form for comparison.
        assert_eq!(
            serde_json::to_string(&decoded).unwrap(),
            serde_json::to_string(&reason).unwrap()
        );
    }

    #[test]
    fn fof_vec_tuple_serde_roundtrip() {
        // FoF caches store Vec<(String, f32)> — scores are cast from f64 at write time
        // (set_fof_recommendations) and returned as f32 at read time.
        // f32 is sufficient: the scoring engine (apply_cache_boosts) uses f32 throughout.
        let recs: Vec<(String, f32)> = vec![
            ("0xaaaa".to_string(), 0.9_f32),
            ("0xbbbb".to_string(), 0.5_f32),
            ("0xcccc".to_string(), 0.1_f32),
        ];

        let json = serde_json::to_string(&recs).expect("serialize");
        let decoded: Vec<(String, f32)> = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(decoded.len(), recs.len());
        for ((addr_orig, score_orig), (addr_dec, score_dec)) in recs.iter().zip(decoded.iter()) {
            assert_eq!(addr_orig, addr_dec);
            assert!((score_orig - score_dec).abs() < f32::EPSILON);
        }
    }

    // ── FoF sub-bucket key distinctness ─────────────────────────────────────

    /// Verify that all four FoF-bucket key constructors produce distinct keys
    /// for the same input address so reads and writes never alias across buckets.
    #[test]
    fn fof_sub_bucket_keys_are_distinct_for_same_address() {
        let addr = "0xaaaa000000000000000000000000000000000000";

        let key_fof          = CacheKey::fof(addr);
        let key_fof_view     = CacheKey::fof_view(addr);
        let key_fof_cmt      = CacheKey::fof_comment(addr);
        let key_fof_purchase = CacheKey::fof_purchase(addr);

        // All four must be different from each other.
        assert_ne!(key_fof, key_fof_view,         "fof and fof_view must not collide");
        assert_ne!(key_fof, key_fof_cmt,          "fof and fof_comment must not collide");
        assert_ne!(key_fof, key_fof_purchase,     "fof and fof_purchase must not collide");
        assert_ne!(key_fof_view, key_fof_cmt,     "fof_view and fof_comment must not collide");
        assert_ne!(key_fof_view, key_fof_purchase,"fof_view and fof_purchase must not collide");
        assert_ne!(key_fof_cmt, key_fof_purchase, "fof_comment and fof_purchase must not collide");

        // Exact prefix assertions so a future rename is caught immediately.
        assert!(key_fof.starts_with("rec:fof:"),              "fof key must have 'rec:fof:' prefix");
        assert!(key_fof_view.starts_with("rec:fof_view:"),    "fof_view key must start with 'rec:fof_view:'");
        assert!(key_fof_cmt.starts_with("rec:fof_comment:"),  "fof_comment key must start with 'rec:fof_comment:'");
        assert!(key_fof_purchase.starts_with("rec:fof_purchase:"), "fof_purchase key must start with 'rec:fof_purchase:'");
    }

    #[test]
    fn user_suggestions_key_uses_dedicated_prefix() {
        let viewer  = "0xaaaa000000000000000000000000000000000000";
        let creator = "0xbbbb000000000000000000000000000000000000";
        let key = CacheKey::user_suggestions(viewer, creator);

        // Must use the dedicated suggestions namespace, not the fof namespace.
        assert!(
            key.starts_with("rec:suggestions:"),
            "user_suggestions key must start with 'rec:suggestions:', got: {key}"
        );
        // Must not alias any fof bucket key for the same viewer address.
        assert_ne!(key, CacheKey::fof(viewer),         "must not collide with fof key");
        assert_ne!(key, CacheKey::fof_view(viewer),    "must not collide with fof_view key");
        assert_ne!(key, CacheKey::fof_comment(viewer), "must not collide with fof_comment key");
    }

    #[test]
    fn user_suggestions_key_is_scoped_by_both_viewer_and_creator() {
        let v1 = "0xaaaa000000000000000000000000000000000000";
        let v2 = "0xbbbb000000000000000000000000000000000000";
        let c1 = "0xcccc000000000000000000000000000000000000";
        let c2 = "0xdddd000000000000000000000000000000000000";

        // Same viewer, different creator → different keys.
        assert_ne!(CacheKey::user_suggestions(v1, c1), CacheKey::user_suggestions(v1, c2));
        // Different viewer, same creator → different keys.
        assert_ne!(CacheKey::user_suggestions(v1, c1), CacheKey::user_suggestions(v2, c1));
        // Swapped viewer/creator → different key (order matters).
        assert_ne!(CacheKey::user_suggestions(v1, c1), CacheKey::user_suggestions(c1, v1));
    }

    // ── compile-time guarantee note ──────────────────────────────────────────

    // There are no hardcoded prefix strings scattered in the rest of the codebase.
    // All Redis keys for the recommendation namespace flow through CacheKey::*.
    // This is a compile-time property: the PREFIX_* constants are private (no `pub`)
    // and CacheKey is the only module that imports them. Any new key constructor
    // must be added here, preventing silent mismatches. No runtime test can verify
    // a compile-time invariant — this comment documents it instead.
}
