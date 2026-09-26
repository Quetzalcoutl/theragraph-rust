//! Social graph cache operations on `RecCache`.
//!
//! Covers: FoF recommendation buckets, Nebula vertex-seen bloom filter,
//! and "Who to Follow" user suggestions.

use redis::AsyncCommands;
use tracing::warn;

use super::keys::{CacheKey, NEBULA_QUERY_TTL};
use super::ops::RecCache;
use super::super::graph_client::FofBucket;

impl RecCache {
    // ── FoF recommendations ───────────────────────────────────────────────────

    /// Get cached FoF recommendations for any bucket.
    ///
    /// Returns `f32` scores — scoring engine uses f32 throughout; storing f64
    /// wastes ~11 bytes/entry with zero precision benefit.
    pub(crate) async fn get_fof_recs(
        &self,
        bucket: FofBucket,
        user_address: &str,
    ) -> Option<Box<[(Box<str>, f32)]>> {
        self.get_json(&CacheKey::fof_bucket(bucket, user_address)).await
    }

    /// Cache FoF recommendations for any bucket.
    ///
    /// Downcasts f64 → f32 before serialization. Guards NaN/Inf: unlike JSON
    /// (which has no NaN literal and would emit `null`), MessagePack encodes
    /// NaN as a real IEEE754 bit pattern and round-trips it faithfully — an
    /// unguarded NaN from Nebula would silently re-enter scoring arithmetic
    /// downstream instead of erroring at deserialize time.
    pub(crate) async fn set_fof_recs(
        &self,
        bucket: FofBucket,
        user_address: &str,
        recs: &[(String, f64)],
    ) {
        let key = CacheKey::fof_bucket(bucket, user_address);
        let recs_f32: Vec<(String, f32)> = recs
            .iter()
            .map(|(k, v)| {
                let safe = if v.is_finite() { *v as f32 } else {
                    warn!(fof_key = %key, score = v, "non-finite FoF score from Nebula — storing 0.0");
                    0.0f32
                };
                (k.clone(), safe)
            })
            .collect();
        self.set_json(&key, &recs_f32, NEBULA_QUERY_TTL).await;
    }

    /// Invalidate all FoF cache slots for a user in one round-trip.
    ///
    /// Iterates `FofBucket::all()` so a new bucket is automatically invalidated.
    /// EFF-008: single DEL for all bucket keys.
    pub async fn delete_fof_all(&self, addr: &str) {
        let keys: Vec<String> = FofBucket::all()
            .iter()
            .map(|b| CacheKey::fof_bucket(*b, addr))
            .collect();
        let mut conn = self.conn_clone();
        if let Err(e) = conn.del::<_, ()>(keys.as_slice()).await {
            warn!("RecCache DEL fof_all error for {}: {}", addr, e);
        }
    }

    // ── Nebula vertex-seen bloom filter ───────────────────────────────────────

    /// Check whether a Nebula graph vertex (user or post) has been upserted before.
    ///
    /// Returns `true` when the vertex-seen bloom filter key exists in Redis.
    /// A `true` result means the caller can skip the `INSERT ... IF NOT EXISTS`
    /// nGQL fragment — the vertex already exists in Nebula with very high probability.
    ///
    /// Returns `false` on any Redis error so the caller falls back to always-upsert.
    pub async fn is_vertex_seen(&self, vid: &str) -> bool {
        let mut conn = self.conn_clone();
        let key = CacheKey::vertex_seen(vid);
        match conn.exists::<_, bool>(&key).await {
            Ok(v) => v,
            Err(_) => false,
        }
    }

    /// Mark a Nebula vertex as confirmed-upserted with a 24-hour TTL.
    ///
    /// Uses SET NX (set-if-not-exists) so concurrent callers are idempotent.
    /// The 24h TTL ensures the bloom filter self-heals if a vertex is removed.
    pub async fn mark_vertex_seen(&self, vid: &str) {
        let mut conn = self.conn_clone();
        let key = CacheKey::vertex_seen(vid);
        let _: Result<(), _> = conn.set_options(
            &key,
            1u8,
            redis::SetOptions::default()
                .conditional_set(redis::ExistenceCheck::NX)
                .with_expiration(redis::SetExpiry::EX(86_400)),
        ).await;
    }

    // ── "Who to Follow" user suggestions ─────────────────────────────────────

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
    /// Downcasts f64 → f32 before serialization — resolver only needs ranking precision.
    /// Guards NaN/Inf before the cast — MessagePack round-trips NaN faithfully
    /// (unlike JSON's `null`), so an unguarded value would resurface downstream.
    pub async fn set_user_suggestions(
        &self,
        viewer: &str,
        creator: &str,
        recs: &[(String, f64)],
    ) {
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

    /// Invalidate the "Who to Follow" user-suggestions cache slot for (viewer, creator).
    pub async fn delete_user_suggestions(&self, viewer: &str, creator: &str) {
        self.del_key(&CacheKey::user_suggestions(viewer, creator)).await;
    }
}
