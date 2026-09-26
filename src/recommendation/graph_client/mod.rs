use anyhow::Result;
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{debug, error, instrument, warn};

// A-01: transport layer extracted to graph_transport.rs.
// pub use keeps the graph_client:: import path working for all callsites.
pub use super::graph_transport::{GraphTransport, NebulaConsoleTransport};
use super::graph_transport::NebulaPoolTransport;

use super::cache::RecCache;
use super::schema_consts::SPACE_THERAGRAPH;
use super::graph_dlq;

mod circuit_breaker;
use circuit_breaker::{CircuitBreaker, run_circuit_breaker};

mod edges;
mod traversal;

// ── GraphTraversal trait ──────────────────────────────────────────────────────

/// Minimal graph-traversal interface consumed by the scoring engine and updater.
///
/// Swapping Nebula for a different graph backend requires implementing only this
/// trait, not the full `GraphClient` API.
///
/// Uses `async_trait` (already a workspace dependency) for object safety.
#[async_trait::async_trait]
pub trait GraphTraversal: Send + Sync {
    /// Return ranked friend-of-friend candidate NFTs for `user_address` via likes.
    async fn get_fof_recommendations(&self, user_address: &str) -> Vec<(String, f64)>;
    /// Return ranked FoF candidates via dwell-time (view_event edges).
    async fn get_view_event_fof_recommendations(&self, user_address: &str) -> Vec<(String, f64)>;
    /// Return ranked FoF candidates via comments_on edges.
    /// Comments signal the highest intent — a friend who typed about content
    /// is a stronger recommendation signal than a passive like.
    async fn get_comment_fof_recommendations(&self, user_address: &str) -> Vec<(String, f64)>;
    /// Return ranked FoF candidates via the dedicated `purchases` edge (migration 15).
    /// Purchases are the strongest economic intent signal: 30-day half-life, 3× score weight.
    async fn get_purchase_fof_recommendations(&self, user_address: &str) -> Vec<(String, f64)>;
    /// Return ranked FoF candidates via the `shared` edge (migration 009).
    /// Shares are the strongest social broadcast signal: 15-day half-life, 1.5× score weight.
    /// A user who shared content is explicitly recommending it to their network.
    async fn get_shared_fof_recommendations(&self, user_address: &str) -> Vec<(String, f64)>;
    /// Return ranked FoF candidates via the `bookmarked` edge.
    /// Bookmarks are a strong private save-for-later signal: 10-day half-life, 1.2× score weight.
    /// Unlike shares, bookmarks are private — they reflect genuine personal interest without social incentive.
    async fn get_bookmark_fof_recommendations(&self, user_address: &str) -> Vec<(String, f64)>;
    /// Return ranked FoF candidates via the `flix_watch` edge.
    /// Video watch is a strong completion-quality signal: 14-day half-life, 2.0× score weight.
    /// Weighted above view_event (passive scroll) and below purchase (economic commitment).
    async fn get_flix_watch_fof_recommendations(&self, user_address: &str) -> Vec<(String, f64)>;
    /// Return this user's style_preference edges (companion chat signal) as
    /// tag → confidence. Pre-warmed into Redis by the same background update
    /// cycle as the FoF buckets; `apply_cache_boosts` reads the Redis cache
    /// directly rather than calling this on the request path.
    async fn get_style_preferences(&self, user_address: &str) -> HashMap<Box<str>, f32>;
    /// Return this user's genre_preference edges (declared listener taste, up
    /// to 30 slugs) as genre slug → confidence. Called live on the request
    /// path by `apply_cache_boosts` (self-caches via Redis internally, no
    /// background pre-warm — see `GraphClient::get_genre_preferences`).
    async fn get_genre_preferences(&self, user_address: &str) -> HashMap<Box<str>, f32>;
    /// Write a `purchases` edge for an API-submitted purchase (best-effort, never propagates errors).
    // Called via dyn GraphTraversal in the API router — compiler can't trace through dynamic dispatch.
    #[allow(dead_code)]
    async fn write_purchases_edge(&self, buyer: &str, post_id: &str, event_id: &str);
    /// Return user suggestions for the "Who to Follow" strip on a creator's profile.
    // Called via dyn GraphTraversal in the API router — compiler can't trace through dynamic dispatch.
    #[allow(dead_code)]
    async fn get_viewer_based_user_suggestions(
        &self,
        viewer_address: &str,
        viewing_creator: &str,
        limit: usize,
    ) -> Vec<(String, f64)>;
    /// Batch-write `recommended_to` edges after serving a feed.
    ///
    /// Each `(nft_id, score)` pair records that the engine served this NFT to
    /// `user_address` at the given score. `served` defaults to `false` — it is
    /// flipped to `true` when `mark_recommendation_served` is called on click.
    /// Best-effort: implementations must never propagate errors to caller.
    async fn write_recommended_to_batch(&self, user_address: &str, served: &[(Box<str>, f32)]) -> Result<()>;

    /// Write `recommended_to` edges for multiple users in a single Nebula round-trip.
    ///
    /// Default implementation: sequential loop — safe for test doubles and any
    /// `GraphTraversal` implementor that does not override it. Errors are swallowed
    /// per-user so a single bad address does not abort the batch.
    ///
    /// `GraphClient` overrides with the batched single-query path that cuts Nebula
    /// connection count from N users to 1 per flush interval.
    async fn write_recommended_to_batch_multi(
        &self,
        batch: &[(String, Box<[(Box<str>, f32)]>)],
    ) -> Result<()> {
        for (addr, pairs) in batch {
            let _ = self.write_recommended_to_batch(addr, pairs).await;
        }
        Ok(())
    }
    /// Mark a previously served recommendation as clicked/purchased.
    ///
    /// Flips the `served` property on the `recommended_to` edge so the engine
    /// can learn which of its recommendations actually drove engagement.
    // Called via dyn GraphTraversal in the API router — compiler can't trace through dynamic dispatch.
    #[allow(dead_code)]
    async fn mark_recommendation_served(&self, user_address: &str, nft_id: &str);
    /// Batch variant of mark_recommendation_served — one nGQL round-trip for N NFTs.
    ///
    /// Replaces per-NFT loop in recommendation serving path (S24 finding 2).
    // Called via dyn GraphTraversal in the API router.
    #[allow(dead_code)]
    async fn mark_recommendations_served_batch(&self, user_address: &str, nft_ids: &[String]);

    /// Execute a raw nGQL write statement and return the response string.
    ///
    /// Exposed on the trait so maintenance operations like
    /// `prune_stale_recommended_to` can work through `dyn GraphTraversal`
    /// without requiring a concrete `GraphClient<T>` reference.
    async fn raw_write(&self, query: &str) -> Result<String>;

    /// Returns `true` when the Nebula circuit breaker is open (Nebula unreachable).
    /// Default implementation returns `false` (used by test doubles / mocks).
    fn is_circuit_open(&self) -> bool { false }

    // ── Write-path operations — called from api.rs interaction handler ────────

    /// Write or accumulate a `view_event` edge. Best-effort; never propagates.
    async fn write_view_event(&self, viewer: &str, post_id: &str, event_id: &str, duration_seconds: u32);
    /// Accumulate `creator_affinity` edge via UPSERT. Best-effort; never propagates.
    async fn write_creator_affinity(&self, viewer: &str, creator: &str, view_duration_secs: u32);
    /// Insert a `comments_on` edge. Best-effort; never propagates.
    async fn write_comments_on(&self, commenter: &str, post_id: &str, event_id: &str, comment_preview: &str);
    /// Insert a `likes` edge. Best-effort; never propagates.
    async fn write_likes_edge(&self, liker: &str, post_id: &str, event_id: &str, reaction_type: &str);
    /// UPSERT a `music_listen` edge — accumulates duration and pct_played.
    /// Distinct from write_view_event: 10-day half-life, pct_played quality signal.
    /// Best-effort; never propagates.
    #[allow(dead_code)]
    async fn write_music_listen_edge(
        &self,
        listener: &str,
        post_id: &str,
        event_id: &str,
        duration_seconds: u32,
        pct_played: f32,
    );
    /// Insert or update a `flix_watch` edge. Same UPSERT semantics as
    /// music_listen: one edge per (viewer, post) pair; duration accumulates,
    /// pct_played uses geometric mean, rewatch_count increments.
    async fn write_flix_watch_edge(
        &self,
        viewer: &str,
        post_id: &str,
        event_id: &str,
        duration_seconds: u32,
        pct_played: f32,
    );
    /// Accumulate `creator_affinity` from a music listen — geometric mean prevents
    /// unbounded saturation from loop listeners (Raph Levien).
    /// Best-effort; never propagates.
    #[allow(dead_code)]
    async fn write_music_creator_affinity(
        &self,
        listener: &str,
        creator: &str,
        duration_seconds: u32,
        pct_played: f32,
    );
    /// Insert a `bookmarked` edge. Best-effort; never propagates.
    async fn write_bookmark_edge(&self, user: &str, post_id: &str, event_id: &str);
    /// Delete a `bookmarked` edge. Best-effort; never propagates.
    async fn delete_bookmark_edge(&self, user: &str, post_id: &str);
    /// Write style_preference + genre_preference edges from a companion signal.
    ///
    /// Confidence uses exponential moving average: `new = old * 0.7 + extracted * 0.3`.
    /// Source field = "companion" so the decay job applies the 3-day half-life
    /// (vs listen signals' 10-day). Best-effort; never propagates.
    #[allow(dead_code)]
    async fn write_companion_preference_edges(
        &self,
        user: &str,
        style_tags: &[String],
        genre_ids: &[i32],
        confidence: f32,
        time_context: &str,
    );
    /// Write `genre_preference` edges for a user's declared favorite genres
    /// (up to 30 kebab-case slugs; caller is responsible for the cap).
    /// Best-effort; never propagates.
    async fn write_genre_preference_edges(&self, user: &str, genre_slugs: &[String]);
}

// ── Transport seam: see graph_transport.rs ───────────────────────────────────

// ── GraphClient ───────────────────────────────────────────────────────────────

/// Graph traversal client with per-operation circuit-breakers and Redis cache.
///
/// Generic over `T: GraphTransport` — use `GraphClient::new()` for production
/// (defaults to `NebulaConsoleTransport`) or `GraphClient::with_transport` in
/// tests to inject a mock.
///
/// Read and write paths each carry an independent `CircuitBreaker` so a
/// transient read degradation (slow FoF traversal) does not trip the write
/// path (and vice-versa).
pub struct GraphClient<T: GraphTransport = NebulaConsoleTransport> {
    transport: Arc<T>,
    cache: Option<RecCache>,
    /// NASA-2: optional Postgres pool for the Nebula write DLQ.
    dlq_pool: Option<sqlx::PgPool>,
    /// Circuit breaker for read traversals (FoF queries, user suggestions, etc.).
    read_cb: CircuitBreaker,
    /// Circuit breaker for write mutations (edge inserts, vertex upserts).
    /// Trips independently of `read_cb` — a slow read does not block writes.
    write_cb: CircuitBreaker,
}

impl<T: GraphTransport> Clone for GraphClient<T> {
    fn clone(&self) -> Self {
        Self {
            transport: Arc::clone(&self.transport),
            cache: self.cache.clone(),
            dlq_pool: self.dlq_pool.clone(),
            read_cb: self.read_cb.clone(),
            write_cb: self.write_cb.clone(),
        }
    }
}

/// Normalise an Ethereum address to lowercase so VIDs are always consistent.
///
/// VID-CASE-001: callers must normalise before building any `format!("user:{addr}")`
/// VID string.  `is_safe_address` now only accepts lowercase hex, so any address
/// that passes validation is already lowercase — but explicit normalisation at each
/// write-path call site makes the invariant visible and guards against future refactors.
pub fn normalize_address(addr: &str) -> String {
    addr.to_lowercase()
}

/// Map a reaction type string to a recommendation signal weight.
/// Used by both the API interaction handler and the Kafka event processor
/// so both write paths produce consistent weight values on likes edges.
pub(crate) fn map_reaction_weight(reaction_type: &str) -> f64 {
    match reaction_type {
        "love"     => 1.5_f64,
        "wow"      => 2.0_f64,
        "purchase" => 2.0_f64,
        "haha"     => 0.8_f64,
        "sad"      => 0.6_f64,
        "angry"    => 0.4_f64,
        _          => 1.0_f64,
    }
}

impl Default for GraphClient {
    fn default() -> Self {
        Self::new()
    }
}

impl GraphClient<NebulaConsoleTransport> {
    pub fn new() -> Self {
        Self::with_transport(NebulaConsoleTransport::from_env())
    }
}


impl GraphClient<NebulaPoolTransport> {
    /// Build a `GraphClient` backed by a persistent connection pool.
    ///
    /// Enable with `NEBULA_POOL=true`; falls back to the console transport if
    /// pool construction fails so the service starts in degraded mode rather
    /// than refusing to start.
    ///
    /// Called by `main.rs` when `NEBULA_POOL=true` is set in the environment.
    pub async fn from_env_pooled() -> Result<Self> {
        let transport = NebulaPoolTransport::from_env().await?;
        Ok(Self::with_transport(transport))
    }
}

// ── DynGraphTransport ─────────────────────────────────────────────────────────

/// Adapter that implements `GraphTransport` by delegating to any
/// `Arc<dyn GraphTraversal>` via `raw_write()`.
///
/// This makes a real seam: it lets `GraphSync<DynGraphTransport>` be built
/// from any trait-object-typed graph client (Console OR Pool) without
/// requiring callers to know the concrete transport type.
///
/// Use `GraphClient::from_dyn_traversal(arc)` to get a ready-to-use client.
pub struct DynGraphTransport(pub Arc<dyn GraphTraversal>);

impl GraphTransport for DynGraphTransport {
    fn execute(&self, query: &str) -> impl std::future::Future<Output = Result<String>> + Send {
        let inner = Arc::clone(&self.0);
        let query = query.to_string();
        async move { inner.raw_write(&query).await }
    }
}

impl GraphClient<DynGraphTransport> {
    /// Build a `GraphClient` backed by any `Arc<dyn GraphTraversal>`.
    ///
    /// Useful when `AppState.graph_client` is already erased to a trait object
    /// but a downstream component (e.g. `GraphSync`) requires a concrete
    /// `GraphClient<T>` to call `execute_write` through the retry layer.
    pub fn from_dyn_traversal(gt: Arc<dyn GraphTraversal>) -> Self {
        Self::with_transport(DynGraphTransport(gt))
    }
}

/// Which FoF edge-type bucket a traversal belongs to.
///
/// Passed to `GraphClient::fof_traverse` to select the correct Redis cache slot.
/// `pub(crate)` so `cache.rs` can use a single `get_fof_recs(bucket, addr)` /
/// `set_fof_recs(bucket, addr, recs)` pair instead of 12 named methods.
#[derive(Copy, Clone)]
pub(crate) enum FofBucket { FollowLike, ViewEvent, Comment, Purchase, Share, Bookmark, FlixWatch }

impl FofBucket {
    /// Redis key prefix for this bucket — single source of truth shared by
    /// `CacheKey::fof_bucket` and the old per-bucket prefix constants.
    ///
    /// Bumped to v2 (session U2026-09-04): wire format changed JSON → MessagePack,
    /// see the version-bump note on `PREFIX_FEATURES` in cache/keys.rs.
    pub(crate) fn prefix(self) -> &'static str {
        match self {
            FofBucket::FollowLike => "rec:fof:v2:",
            FofBucket::ViewEvent  => "rec:fof_view:v2:",
            FofBucket::Comment    => "rec:fof_comment:v2:",
            FofBucket::Purchase   => "rec:fof_purchase:v2:",
            FofBucket::Share      => "rec:fof_share:v2:",
            FofBucket::Bookmark   => "rec:fof_bookmark:v2:",
            FofBucket::FlixWatch  => "rec:fof_flix:v2:",
        }
    }

    /// All bucket variants in definition order — used by `delete_fof_all`
    /// so the invalidation list can never silently miss a new bucket.
    pub(crate) fn all() -> &'static [FofBucket] {
        &[
            FofBucket::FollowLike,
            FofBucket::ViewEvent,
            FofBucket::Comment,
            FofBucket::Purchase,
            FofBucket::Share,
            FofBucket::Bookmark,
            FofBucket::FlixWatch,
        ]
    }
}

impl<T: GraphTransport> GraphClient<T> {
    pub fn with_transport(transport: T) -> Self {
        Self {
            transport: Arc::new(transport),
            cache: None,
            dlq_pool: None,
            read_cb: CircuitBreaker::default(),
            write_cb: CircuitBreaker::default(),
        }
    }

    pub fn with_cache(mut self, cache: Option<RecCache>) -> Self {
        self.cache = cache;
        self
    }

    /// NASA-2: attach a Postgres pool for the Nebula write DLQ.
    /// When set, every permanently-failed Nebula write is recorded in
    /// `nebula_write_failures` with per-operation Prometheus counters.
    pub fn with_dlq_pool(mut self, pool: sqlx::PgPool) -> Self {
        self.dlq_pool = Some(pool);
        self
    }

    /// Returns true when either the read or write Nebula circuit breaker is open.
    ///
    /// Used by health-check endpoints to surface Nebula connectivity status.
    pub fn is_circuit_open(&self) -> bool {
        self.read_cb.is_open() || self.write_cb.is_open()
    }

    /// Execute a write nGQL statement (UPSERT/INSERT/DELETE) through the write circuit breaker.
    ///
    /// Never reads from or writes to Redis. Caching mutation results is incorrect:
    /// a cached success response would be returned on retry without re-executing the
    /// write in Nebula, silently skipping the mutation.
    ///
    /// Uses `write_cb` — independent from the read circuit breaker so read-path
    /// degradation does not block edge writes from reaching Nebula.
    #[instrument(skip(self, query), fields(query_len = query.len()))]
    pub async fn execute_write(&self, query: &str) -> Result<String> {
        let transport = Arc::clone(&self.transport);
        let query = query.to_string();
        run_circuit_breaker(
            &self.write_cb,
            async move { transport.execute(&query).await },
            "nebula_writes_total",
            "nebula_write_errors_total",
        ).await
    }

    /// Execute an nGQL query through the read circuit breaker with Redis caching.
    ///
    /// Circuit breaker: opens after 3 consecutive failures; closes on first success.
    /// Results cached in Redis when available (cache-aside pattern).
    /// Use `execute_write` for mutations — they must never be cached.
    ///
    /// Cache contract: a cache hit never interacts with the circuit breaker.
    /// Cache availability is independent of Nebula availability — a Redis hit
    /// must not probe or close the circuit.
    #[allow(dead_code)] // used in circuit-breaker tests (#[cfg(test)])
    #[instrument(skip(self, query), fields(query_len = query.len()))]
    pub async fn execute_query(&self, query: &str) -> Result<String> {
        // Cache-aside: check Redis before touching the transport or circuit breaker.
        if let Some(ref cache) = self.cache {
            if let Some(cached) = cache.get_nebula_query(query).await {
                debug!("Nebula cache HIT");
                metrics::counter!("nebula_cache_hits_total").increment(1);
                return Ok(cached);
            }
        }

        let transport = Arc::clone(&self.transport);
        let query_owned = query.to_string();
        let result = run_circuit_breaker(
            &self.read_cb,
            async move { transport.execute(&query_owned).await },
            "nebula_queries_total",
            "nebula_query_errors_total",
        ).await?;

        if let Some(ref cache) = self.cache {
            cache.set_nebula_query(query, &result).await;
        }

        Ok(result)
    }

    /// Execute an nGQL query through the read circuit breaker, bypassing the query-string cache.
    ///
    /// Use for traversals called after cache invalidation (e.g. fof_traverse
    /// after delete_fof_all) — the per-user FoF slot (tier-1) is cleared by
    /// delete_fof_all, but execute_query's query-string cache (tier-2) would
    /// re-promote stale results for up to the nGQL TTL (~5 min). Bypassing
    /// tier-2 ensures a fresh Nebula query fires and the new result overwrites
    /// the per-user slot correctly.
    pub(crate) async fn execute_query_uncached(&self, query: &str) -> Result<String> {
        let transport = Arc::clone(&self.transport);
        let query_owned = query.to_string();
        run_circuit_breaker(
            &self.read_cb,
            async move { transport.execute(&query_owned).await },
            "nebula_queries_total",
            "nebula_query_errors_total",
        ).await
    }

    /// Execute a write query and, on failure, log the error and record it in the DLQ.
    ///
    /// Centralises the repeated `error! + record_failure` block so each write
    /// method collapses to a single call instead of 5 lines of repeated boilerplate.
    async fn execute_write_or_dlq(
        &self,
        op: &'static str,
        src: &str,
        dst: &str,
        query: &str,
    ) {
        if let Err(e) = self.execute_write(query).await {
            error!("Nebula {op} failed (best-effort): {e}");
            if let Some(ref pool) = self.dlq_pool {
                graph_dlq::record_failure(pool, op, Some(src.to_owned()), Some(dst.to_owned()), query.to_string(), e.to_string());
            }
        }
    }

    /// Check Redis bloom filter for two vertex IDs and return (seen_a, seen_b).
    ///
    /// BLOOM-001: skips the INSERT VERTEX IF NOT EXISTS sub-statement for vertices
    /// that are already confirmed in Nebula. Reduces median nGQL payload size by ~60%
    /// on hot paths (write_view_event fires thousands of times/minute per active user).
    async fn vertex_bloom_check(&self, vid_a: &str, vid_b: &str) -> (bool, bool) {
        if let Some(ref cache) = self.cache {
            tokio::join!(cache.is_vertex_seen(vid_a), cache.is_vertex_seen(vid_b))
        } else {
            (false, false)
        }
    }

    /// Fire-and-forget: mark two vertex IDs as seen in Redis bloom filter.
    fn mark_vertices_seen(&self, vid_a: String, vid_b: String) {
        if let Some(cache) = self.cache.clone() {
            tokio::spawn(async move {
                tokio::join!(cache.mark_vertex_seen(&vid_a), cache.mark_vertex_seen(&vid_b));
            });
        }
    }

    /// Shared scaffold for `INSERT EDGE [IF NOT EXISTS] type(props) VALUES
    /// src -> dst[@rank]:(vals);` — the shape common to write_follows_edge,
    /// write_comments_on, write_likes_edge, write_purchases_edge, and
    /// write_bookmark_edge. Deliberately NOT used by write_view_event or
    /// write_creator_affinity: those use `UPSERT EDGE ON ... SET x = x + delta`
    /// accumulator syntax, a structurally different statement this helper
    /// does not (and should not be made to) express.
    async fn write_edge(
        &self,
        op: &'static str,
        edge_type: &'static str,
        if_not_exists: bool,
        src_vid: &str,
        src_upsert: String,
        dst_vid: &str,
        dst_upsert: String,
        rank: Option<i64>,
        props: &str,
        values: &str,
    ) {
        let kw = if if_not_exists { "INSERT EDGE IF NOT EXISTS" } else { "INSERT EDGE" };
        let rank_sfx = rank.map(|r| format!("@{r}")).unwrap_or_default();
        let query = format!(
            "USE {space};\n{src_upsert}\n{dst_upsert}\n{kw} {edge_type}({props}) VALUES \"{src_vid}\" -> \"{dst_vid}\"{rank_sfx}:({values});",
            space = SPACE_THERAGRAPH,
        );
        self.execute_write_or_dlq(op, src_vid, dst_vid, &query).await;
    }

    /// Shared scaffold for `DELETE EDGE type src -> dst;` — used by
    /// delete_follows_edge and delete_bookmark_edge.
    async fn delete_edge(&self, op: &'static str, edge_type: &'static str, src_vid: &str, dst_vid: &str) {
        let query = format!(
            "USE {space};\nDELETE EDGE {edge_type} \"{src_vid}\" -> \"{dst_vid}\";",
            space = SPACE_THERAGRAPH,
        );
        self.execute_write_or_dlq(op, src_vid, dst_vid, &query).await;
    }
}

// ── GraphTraversal impl for GraphClient ──────────────────────────────────────

/// Implement `GraphTraversal` for `GraphClient` so it can be erased behind
/// `Arc<dyn GraphTraversal>` in the updater and any future callers.
///
/// The implementation delegates to the inherent `GraphClient` methods and
/// swallows errors into an empty `Vec` — the trait contract permits this so
/// callers don't have to handle `Result`.

/// Collapse a `Result<Vec<(String,f64)>>` to `Vec` with a warning on error.
/// Used by every FOF/suggestion delegation method below.
fn unwrap_fof(result: Result<Vec<(String, f64)>>, op: &'static str) -> Vec<(String, f64)> {
    match result {
        Ok(v) => v,
        Err(e) => { warn!("GraphTraversal::{op} failed: {e}"); Vec::new() }
    }
}

#[async_trait::async_trait]
impl<T: GraphTransport + 'static> GraphTraversal for GraphClient<T> {
    async fn get_fof_recommendations(&self, user_address: &str) -> Vec<(String, f64)> {
        unwrap_fof(GraphClient::get_fof_recommendations(self, user_address).await, "get_fof_recommendations")
    }

    async fn get_view_event_fof_recommendations(&self, user_address: &str) -> Vec<(String, f64)> {
        unwrap_fof(GraphClient::get_view_event_fof_recommendations(self, user_address).await, "get_view_event_fof")
    }

    async fn get_comment_fof_recommendations(&self, user_address: &str) -> Vec<(String, f64)> {
        unwrap_fof(GraphClient::get_comment_fof_recommendations(self, user_address).await, "get_comment_fof")
    }

    async fn get_purchase_fof_recommendations(&self, user_address: &str) -> Vec<(String, f64)> {
        unwrap_fof(GraphClient::get_purchase_fof_recommendations(self, user_address).await, "get_purchase_fof")
    }

    async fn get_shared_fof_recommendations(&self, user_address: &str) -> Vec<(String, f64)> {
        unwrap_fof(GraphClient::get_shared_fof_recommendations(self, user_address).await, "get_shared_fof")
    }

    async fn get_bookmark_fof_recommendations(&self, user_address: &str) -> Vec<(String, f64)> {
        unwrap_fof(GraphClient::get_bookmark_fof_recommendations(self, user_address).await, "get_bookmark_fof")
    }
    async fn get_flix_watch_fof_recommendations(&self, user_address: &str) -> Vec<(String, f64)> {
        unwrap_fof(GraphClient::get_flix_watch_fof_recommendations(self, user_address).await, "get_flix_watch_fof")
    }

    async fn get_style_preferences(&self, user_address: &str) -> HashMap<Box<str>, f32> {
        match GraphClient::get_style_preferences(self, user_address).await {
            Ok(v) => v,
            Err(e) => { warn!("GraphTraversal::get_style_preferences failed: {e}"); HashMap::new() }
        }
    }

    async fn get_genre_preferences(&self, user_address: &str) -> HashMap<Box<str>, f32> {
        match GraphClient::get_genre_preferences(self, user_address).await {
            Ok(v) => v,
            Err(e) => { warn!("GraphTraversal::get_genre_preferences failed: {e}"); HashMap::new() }
        }
    }

    async fn get_viewer_based_user_suggestions(
        &self,
        viewer_address: &str,
        viewing_creator: &str,
        limit: usize,
    ) -> Vec<(String, f64)> {
        unwrap_fof(
            GraphClient::get_viewer_based_user_suggestions(self, viewer_address, viewing_creator, limit).await,
            "get_viewer_based_user_suggestions",
        )
    }

    async fn write_recommended_to_batch(&self, user_address: &str, served: &[(Box<str>, f32)]) -> Result<()> {
        GraphClient::write_recommended_to_batch(self, user_address, served).await
    }

    async fn write_recommended_to_batch_multi(
        &self,
        batch: &[(String, Box<[(Box<str>, f32)]>)],
    ) -> Result<()> {
        GraphClient::write_recommended_to_batch_multi(self, batch).await
    }

    async fn mark_recommendation_served(&self, user_address: &str, nft_id: &str) {
        GraphClient::mark_recommendation_served(self, user_address, nft_id).await;
    }

    async fn write_purchases_edge(&self, buyer: &str, post_id: &str, event_id: &str) {
        GraphClient::write_purchases_edge(self, buyer, post_id, event_id).await;
    }

    async fn mark_recommendations_served_batch(&self, user_address: &str, nft_ids: &[String]) {
        GraphClient::mark_recommendations_served_batch(self, user_address, nft_ids).await;
    }

    async fn raw_write(&self, query: &str) -> Result<String> {
        self.execute_write(query).await
    }

    fn is_circuit_open(&self) -> bool {
        GraphClient::is_circuit_open(self)
    }

    async fn write_view_event(&self, viewer: &str, post_id: &str, event_id: &str, duration_seconds: u32) {
        GraphClient::write_view_event(self, viewer, post_id, event_id, duration_seconds).await;
    }

    async fn write_creator_affinity(&self, viewer: &str, creator: &str, view_duration_secs: u32) {
        GraphClient::write_creator_affinity(self, viewer, creator, view_duration_secs).await;
    }

    async fn write_music_listen_edge(&self, listener: &str, post_id: &str, event_id: &str, duration_seconds: u32, pct_played: f32) {
        GraphClient::write_music_listen_edge(self, listener, post_id, event_id, duration_seconds, pct_played).await;
    }

    async fn write_flix_watch_edge(&self, viewer: &str, post_id: &str, event_id: &str, duration_seconds: u32, pct_played: f32) {
        GraphClient::write_flix_watch_edge(self, viewer, post_id, event_id, duration_seconds, pct_played).await;
    }

    async fn write_music_creator_affinity(&self, listener: &str, creator: &str, duration_seconds: u32, pct_played: f32) {
        GraphClient::write_music_creator_affinity(self, listener, creator, duration_seconds, pct_played).await;
    }

    async fn write_comments_on(&self, commenter: &str, post_id: &str, event_id: &str, comment_preview: &str) {
        GraphClient::write_comments_on(self, commenter, post_id, event_id, comment_preview).await;
    }

    async fn write_likes_edge(&self, liker: &str, post_id: &str, event_id: &str, reaction_type: &str) {
        GraphClient::write_likes_edge(self, liker, post_id, event_id, reaction_type).await;
    }

    async fn write_bookmark_edge(&self, user: &str, post_id: &str, event_id: &str) {
        GraphClient::write_bookmark_edge(self, user, post_id, event_id).await;
    }

    async fn delete_bookmark_edge(&self, user: &str, post_id: &str) {
        GraphClient::delete_bookmark_edge(self, user, post_id).await;
    }

    async fn write_companion_preference_edges(
        &self,
        user: &str,
        style_tags: &[String],
        genre_ids: &[i32],
        confidence: f32,
        time_context: &str,
    ) {
        GraphClient::write_companion_preference_edges(self, user, style_tags, genre_ids, confidence, time_context).await;
    }

    async fn write_genre_preference_edges(&self, user: &str, genre_slugs: &[String]) {
        GraphClient::write_genre_preference_edges(self, user, genre_slugs).await;
    }
}

// ── Test support and tests ────────────────────────────────────────────────────

#[cfg(test)]
pub mod test_support;

#[cfg(test)]
mod tests;
