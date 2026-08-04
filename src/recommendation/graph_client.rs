use anyhow::Result;
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use tracing::{debug, error, instrument, warn};

// A-01: transport layer extracted to graph_transport.rs.
// pub use keeps the graph_client:: import path working for all callsites.
pub use super::graph_transport::{GraphTransport, NebulaConsoleTransport};
use super::graph_transport::{NebulaPoolTransport, parse_nebula_table};

use super::cache::RecCache;
use super::schema_consts::{
    vid_user, vid_post, comment_rank,
    SPACE_THERAGRAPH,
    EDGE_FOLLOWS, EDGE_LIKES, EDGE_PURCHASES, EDGE_VIEW_EVENT, EDGE_CREATOR_AFFINITY,
    EDGE_RECOMMENDED_TO, EDGE_COMMENTS_ON, EDGE_BOOKMARKED, EDGE_SHARED,
    PROP_DURATION_SECONDS, PROP_WEIGHT, PROP_SCORE, PROP_SERVED,
    PROP_EVENT_ID, PROP_FOLLOWED_AT, PROP_LIKED_AT, PROP_PURCHASED_AT, PROP_COMMENTED_AT,
    PROP_COMPUTED_AT, PROP_EVENT_TIME, PROP_TOTAL_VIEWS,
    PROP_TOTAL_DURATION_SECS, PROP_AFFINITY_SCORE, PROP_LAST_INTERACTION_AT,
    PROP_COMMENT_TEXT, PROP_REACTION_TYPE, PROP_BOOKMARKED_AT, PROP_SHARED_AT,
    // B-02: consolidated validation SSOT (removed private duplicates below)
    is_safe_address, is_safe_id, is_safe_post_vid_id,
    // A-04: vertex upsert nGQL helpers (replaces 6 inline copies)
    ensure_user_vertex_nql, ensure_post_vertex_nql,
};
use super::graph_dlq;


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
    async fn write_recommended_to_batch(&self, user_address: &str, served: &[(&str, f32)]) -> Result<()>;
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
    async fn mark_recommendations_served_batch(&self, user_address: &str, nft_ids: &[&str]);

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
    /// Insert a `bookmarked` edge. Best-effort; never propagates.
    async fn write_bookmark_edge(&self, user: &str, post_id: &str, event_id: &str);
    /// Delete a `bookmarked` edge. Best-effort; never propagates.
    async fn delete_bookmark_edge(&self, user: &str, post_id: &str);
}

/// After this many consecutive write/query failures, the circuit opens and
/// Nebula calls are skipped until the next successful response.
const CIRCUIT_OPEN_THRESHOLD: u32 = 3;

// ── Transport seam: see graph_transport.rs ───────────────────────────────────

// ── CircuitBreaker ────────────────────────────────────────────────────────────

/// Shared, cheaply-cloneable circuit-breaker state.
///
/// Reads and writes each hold their own instance so they never interfere:
/// a slow bulk read traversal that trips the read breaker does not block
/// write events from reaching Nebula (and going to the DLQ on failure),
/// and a write-backpressure spike does not degrade the read path.
#[derive(Clone)]
struct CircuitBreaker {
    consecutive_failures: Arc<AtomicU32>,
    circuit_open: Arc<AtomicBool>,
    last_opened_at: Arc<AtomicU64>,
}

impl Default for CircuitBreaker {
    fn default() -> Self {
        Self {
            consecutive_failures: Arc::new(AtomicU32::new(0)),
            circuit_open: Arc::new(AtomicBool::new(false)),
            last_opened_at: Arc::new(AtomicU64::new(0)),
        }
    }
}

impl CircuitBreaker {
    fn is_open(&self) -> bool {
        self.circuit_open.load(Ordering::Acquire)
    }
}

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
/// Passed to `GraphClient::fof_traverse` to select the correct Redis cache slot
/// without duplicating the getter/setter call-site per method.
#[derive(Copy, Clone)]
enum FofBucket { FollowLike, ViewEvent, Comment, Purchase, Share, Bookmark }

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
        Self::run_circuit_breaker(
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
        let result = Self::run_circuit_breaker(
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
        Self::run_circuit_breaker(
            &self.read_cb,
            async move { transport.execute(&query_owned).await },
            "nebula_queries_total",
            "nebula_query_errors_total",
        ).await
    }

    /// Execute an operation through a specific circuit-breaker instance.
    ///
    /// Called by `execute_write` (write_cb) and `execute_query*` (read_cb) so that
    /// read and write failures trip independent breakers. The caller selects the
    /// correct breaker — this function is purely mechanical.
    ///
    /// `success_counter` and `error_counter` are the metrics counter names for
    /// the operation type (e.g. `"nebula_writes_total"` / `"nebula_write_errors_total"`).
    async fn run_circuit_breaker<Fut>(
        cb: &CircuitBreaker,
        op: Fut,
        success_counter: &'static str,
        error_counter: &'static str,
    ) -> Result<String>
    where
        Fut: std::future::Future<Output = Result<String>> + Send,
    {
        // CB-01: use Acquire on the flag load so the Release store of last_opened_at
        // (written before setting circuit_open=true) is visible in this thread.
        let was_already_open = cb.circuit_open.load(Ordering::Acquire);
        if was_already_open {
            let last = cb.last_opened_at.load(Ordering::Relaxed);
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            if now.saturating_sub(last) < 30 {
                // Still in cooldown — skip this call
                metrics::counter!("nebula_circuit_skipped_total").increment(1);
                anyhow::bail!("Nebula circuit open — consecutive failures exceeded threshold");
            }
            // CB-02: CAS true→false to atomically claim the half-open probe slot.
            // Only one thread wins; all others bail so we don't thundering-herd the
            // recovering Nebula process.
            if cb.circuit_open
                .compare_exchange(true, false, Ordering::AcqRel, Ordering::Relaxed)
                .is_err()
            {
                anyhow::bail!("Nebula circuit open — probe already in flight");
            }
            debug!("Nebula circuit half-open: probe claimed (sole prober)");
        }

        match op.await {
            Ok(stdout) => {
                cb.consecutive_failures.store(0, Ordering::Relaxed);
                // If we just successfully closed via the probe CAS above, emit metric.
                if was_already_open {
                    metrics::counter!("nebula_circuit_closed_total").increment(1);
                    warn!("Nebula circuit CLOSED — connection restored");
                } else if cb.circuit_open.swap(false, Ordering::Release) {
                    // Closed by success path without going through probe.
                    metrics::counter!("nebula_circuit_closed_total").increment(1);
                    warn!("Nebula circuit CLOSED — connection restored");
                }
                metrics::counter!(success_counter).increment(1);
                Ok(stdout)
            }
            Err(e) => {
                let failures = cb.consecutive_failures.fetch_add(1, Ordering::Relaxed) + 1;
                metrics::counter!(error_counter).increment(1);
                if failures >= CIRCUIT_OPEN_THRESHOLD || was_already_open {
                    // CB-01: write last_opened_at BEFORE releasing circuit_open=true
                    // so any concurrent Acquire load of circuit_open sees a valid timestamp.
                    let ts = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();
                    cb.last_opened_at.store(ts, Ordering::Relaxed);
                    // Release store: all prior Relaxed stores (including last_opened_at)
                    // are visible to threads that Acquire-load circuit_open.
                    cb.circuit_open.store(true, Ordering::Release);
                    metrics::counter!("nebula_circuit_opened_total").increment(1);
                    warn!("Nebula circuit OPENED after {} consecutive failures", failures);
                }
                Err(e)
            }
        }
    }

    /// Execute a write query and, on failure, log the error and record it in the DLQ.
    ///
    /// Centralises the repeated `error! + record_failure` block so each write
    /// method collapses to a single call instead of 5 lines of repeated boilerplate.
    async fn execute_write_or_dlq(
        &self,
        op: &'static str,
        src: String,
        dst: String,
        query: &str,
    ) {
        if let Err(e) = self.execute_write(query).await {
            error!("Nebula {op} failed (best-effort): {e}");
            if let Some(ref pool) = self.dlq_pool {
                graph_dlq::record_failure(pool, op, Some(src), Some(dst), query.to_string(), e.to_string());
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
        self.execute_write_or_dlq(op, src_vid.to_string(), dst_vid.to_string(), &query).await;
    }

    /// Shared scaffold for `DELETE EDGE type src -> dst;` — used by
    /// delete_follows_edge and delete_bookmark_edge.
    async fn delete_edge(&self, op: &'static str, edge_type: &'static str, src_vid: &str, dst_vid: &str) {
        let query = format!(
            "USE {space};\nDELETE EDGE {edge_type} \"{src_vid}\" -> \"{dst_vid}\";",
            space = SPACE_THERAGRAPH,
        );
        self.execute_write_or_dlq(op, src_vid.to_string(), dst_vid.to_string(), &query).await;
    }

    /// Upsert user vertices and insert a `follows` edge.
    ///
    /// Best-effort: logs on failure, never propagates — caller must persist
    /// the canonical follow record in PostgreSQL for durability.
    #[allow(dead_code)] // NEBULA-003: graph_sync bypasses this via execute_write for retry reliability
    pub async fn write_follows_edge(&self, follower: &str, followee: &str, event_id: &str) {
        // VID-CASE-001: normalise before validating so EIP-55 checksummed addresses
        // (mixed-case hex) are accepted instead of silently dropping the edge write.
        let follower = normalize_address(follower);
        let followee = normalize_address(followee);
        if !is_safe_address(&follower) || !is_safe_address(&followee) || !is_safe_id(event_id) {
            warn!("write_follows_edge: invalid input — follower={follower} followee={followee}");
            return;
        }
        // UPSERT-HOT-001: replaced UPSERT VERTEX with INSERT VERTEX IF NOT EXISTS.
        // UPSERT reads then conditionally writes; IF NOT EXISTS is a pure conditional
        // insert — no read when the vertex already exists.  Also stops the silent
        // reset of counter properties (followers_count etc.) on every repeat event.
        // SCHEMA-FOLLOWS-TYPE: removed follows.type column — the literal "follow"
        // is always deducible from the edge type name and was never read by any query.
        let fwr_vid = vid_user(&follower);
        let fwe_vid = vid_user(&followee);
        // BLOOM-001: skip vertex upsert NQL for vertices already confirmed in Nebula.
        let (fwr_seen, fwe_seen) = self.vertex_bloom_check(&fwr_vid, &fwe_vid).await;
        self.write_edge(
            "follows_edge",
            EDGE_FOLLOWS,
            true,
            &fwr_vid,
            if fwr_seen { String::new() } else { ensure_user_vertex_nql(&fwr_vid, &follower) },
            &fwe_vid,
            if fwe_seen { String::new() } else { ensure_user_vertex_nql(&fwe_vid, &followee) },
            None,
            &format!("{PROP_EVENT_ID}, {PROP_FOLLOWED_AT}, {PROP_WEIGHT}"),
            &format!("\"{event_id}\", now(), 1.0"),
        ).await;
        // Invalidate all follow-related caches so the next feed open reflects the new
        // edge rather than stale data served for the full TTL (300s for following/prefs,
        // 600s for recs).  Mirrors the five invalidations in the Kafka-path handle_follow
        // (social.rs) so both write paths are complete and permanently in sync.
        if let Some(ref cache) = self.cache {
            cache.delete_following(&follower).await;
            cache.delete_user_prefs(&follower).await;
            cache.delete_recommendations(&follower).await;
            cache.delete_fof_all(&follower).await;
            cache.delete_user_suggestions(&follower, &followee).await;
        }
        self.mark_vertices_seen(fwr_vid, fwe_vid);
    }

    /// Delete the `follows` edge on unfollow.
    #[allow(dead_code)] // NEBULA-003: same bypass as write_follows_edge; available for future HTTP API
    pub async fn delete_follows_edge(&self, follower: &str, followee: &str) {
        // VID-CASE-001: normalise before validating (EIP-55 checksummed → accepted).
        let follower = normalize_address(follower);
        let followee = normalize_address(followee);
        if !is_safe_address(&follower) || !is_safe_address(&followee) {
            warn!("delete_follows_edge: invalid input — follower={follower} followee={followee}");
            return;
        }
        let fwr_vid = vid_user(&follower);
        let fwe_vid = vid_user(&followee);
        self.delete_edge("delete_follows_edge", EDGE_FOLLOWS, &fwr_vid, &fwe_vid).await;
        // Mirrors the five invalidations in write_follows_edge — unfollow changes
        // the same graph topology, so all five caches must be cleared here too.
        if let Some(ref cache) = self.cache {
            cache.delete_following(&follower).await;
            cache.delete_user_prefs(&follower).await;
            cache.delete_recommendations(&follower).await;
            cache.delete_fof_all(&follower).await;
            cache.delete_user_suggestions(&follower, &followee).await;
        }
    }

    /// Upsert a `view_event` edge — accumulates dwell time across repeated views.
    ///
    /// Uses UPSERT (rank 0) so each (viewer, post) pair has exactly one edge.
    /// `duration_seconds` accumulates: a user who watches 3 times gets the total
    /// dwell recorded, not just the most-recent view. This bounds storage at
    /// O(users × posts_viewed) and makes `sum(duration_seconds)` in FoF queries
    /// return total dwell rather than the last view's duration.
    ///
    /// Only writes when duration_seconds > 0 so zero-dwell noise is filtered.
    /// Best-effort: never propagates errors to caller.
    pub async fn write_view_event(
        &self,
        viewer: &str,
        post_id: &str,
        event_id: &str,
        duration_seconds: u32,
    ) {
        // VID-CASE-001: normalise before validating (EIP-55 checksummed → accepted).
        let viewer = normalize_address(viewer);
        if !is_safe_address(&viewer)
            || !is_safe_post_vid_id(post_id)
            || !is_safe_id(event_id)
        {
            warn!("write_view_event: invalid input — viewer={viewer} post_id={post_id}");
            return;
        }
        if duration_seconds == 0 {
            return;
        }
        // UPSERT-HOT-001 / A-04: vertex upserts via ensure_*_vertex_nql helper.
        let vwr_vid = vid_user(&viewer);
        let pid_vid = vid_post(post_id);
        // BLOOM-001: skip vertex upsert NQL for vertices already confirmed in Nebula.
        let (vwr_seen, pid_seen) = self.vertex_bloom_check(&vwr_vid, &pid_vid).await;
        let query = format!(
            "USE {space};\n{upsert_vwr}\n{upsert_pid}\nUPSERT EDGE ON {e_view_event} \"{vwr_vid}\" -> \"{pid_vid}\"\nSET {p_eid} = \"{eid}\",\n    {p_event_time} = now(),\n    {p_dur} = {p_dur} + {dur};",
            space = SPACE_THERAGRAPH,
            upsert_vwr = if vwr_seen { String::new() } else { ensure_user_vertex_nql(&vwr_vid, &viewer) },
            upsert_pid = if pid_seen { String::new() } else { ensure_post_vertex_nql(&pid_vid, post_id) },
            vwr_vid = vwr_vid,
            pid_vid = pid_vid,
            e_view_event = EDGE_VIEW_EVENT,
            eid = event_id,
            dur = duration_seconds,
            p_eid = PROP_EVENT_ID,
            p_event_time = PROP_EVENT_TIME,
            p_dur = PROP_DURATION_SECONDS,
        );
        self.execute_write_or_dlq("view_event_edge", viewer.clone(), post_id.to_string(), &query).await;
        self.mark_vertices_seen(vwr_vid, pid_vid);
    }

    /// UPSERT the `creator_affinity` edge, accumulating view count and dwell time.
    ///
    /// Uses NebulaGraph UPSERT semantics: on first insert defaults are applied,
    /// then SET expressions reference the current (now-defaulted) values so the
    /// counters accumulate correctly across repeated calls.
    pub async fn write_creator_affinity(
        &self,
        viewer: &str,
        creator: &str,
        view_duration_secs: u32,
    ) {
        // VID-CASE-001: normalise before validating (EIP-55 checksummed → accepted).
        let viewer = normalize_address(viewer);
        let creator = normalize_address(creator);
        if !is_safe_address(&viewer) || !is_safe_address(&creator) {
            warn!("write_creator_affinity: invalid input — viewer={viewer} creator={creator}");
            return;
        }
        if viewer == creator {
            return;
        }
        // affinity_score uses exponential decay toward a 0-10 ceiling.
        // Old formula (total_secs / 300) grew unboundedly — a user who watched
        // 500 minutes accumulated affinity_score=100, making early interactions
        // dwarf everything else. New formula: score = 10 * (1 - e^(-total/1800))
        // which saturates at 10.0 after ~30 min of total watch time, never
        // exceeds 10.0, and is monotonically increasing.
        // The SET clause evaluates after the accumulation, so total_duration_secs
        // already includes {dur} when affinity_score is recomputed.
        // UPSERT-HOT-001 / A-04: vertex upserts via ensure_user_vertex_nql helper.
        let vwr_vid = vid_user(&viewer);
        let ctr_vid = vid_user(&creator);
        // BLOOM-001: skip vertex upsert NQL for vertices already confirmed in Nebula.
        let (vwr_seen, ctr_seen) = self.vertex_bloom_check(&vwr_vid, &ctr_vid).await;
        let query = format!(
            "USE {space};\n{upsert_vwr}\n{upsert_ctr}\nUPSERT EDGE ON {e_creator_affinity} \"{vwr_vid}\" -> \"{ctr_vid}\"\nSET {p_total_views} = {p_total_views} + 1,\n    {p_dur_secs} = {p_dur_secs} + {dur},\n    {p_affinity} = 10.0 * (1.0 - exp(-(toFloat({p_dur_secs} + {dur}) / 1800.0))),\n    {p_last_at} = now();",
            space = SPACE_THERAGRAPH,
            upsert_vwr = if vwr_seen { String::new() } else { ensure_user_vertex_nql(&vwr_vid, &viewer) },
            upsert_ctr = if ctr_seen { String::new() } else { ensure_user_vertex_nql(&ctr_vid, &creator) },
            vwr_vid = vwr_vid,
            ctr_vid = ctr_vid,
            e_creator_affinity = EDGE_CREATOR_AFFINITY,
            dur = view_duration_secs,
            p_total_views = PROP_TOTAL_VIEWS,
            p_dur_secs = PROP_TOTAL_DURATION_SECS,
            p_affinity = PROP_AFFINITY_SCORE,
            p_last_at = PROP_LAST_INTERACTION_AT,
        );
        self.execute_write_or_dlq("creator_affinity_edge", viewer.clone(), creator.clone(), &query).await;
        self.mark_vertices_seen(vwr_vid, ctr_vid);
    }

    /// Insert a `comments_on` edge.
    pub async fn write_comments_on(
        &self,
        commenter: &str,
        post_id: &str,
        event_id: &str,
        comment_preview: &str,
    ) {
        // VID-CASE-001: normalise before validating (EIP-55 checksummed → accepted).
        let commenter = normalize_address(commenter);
        if !is_safe_address(&commenter)
            || !is_safe_post_vid_id(post_id)
            || !is_safe_id(event_id)
        {
            warn!("write_comments_on: invalid input — commenter={commenter} post_id={post_id}");
            return;
        }
        // Truncate preview to 120 chars and strip any characters that would break nGQL strings.
        let safe_preview: String = comment_preview
            .chars()
            .filter(|c| c.is_alphanumeric() || *c == ' ')
            .take(120)
            .collect();
        // NEBULA-002: shared with graph_sync.rs's inner_sync_comment via
        // schema_consts::comment_rank — see its doc comment for why (dash
        // stripping for UUID event_ids, 15-digit cap to avoid i64 overflow).
        // The two call sites previously hand-derived this independently and
        // drifted, leaving this REST path with a stale, buggy version.
        let rank = comment_rank(event_id);
        // UPSERT-HOT-001 / A-04: vertex upserts via ensure_*_vertex_nql helper.
        let cmtr_vid = vid_user(&commenter);
        let pid_vid = vid_post(post_id);
        // BLOOM-001: skip vertex upsert NQL for vertices already confirmed in Nebula.
        let (cmtr_seen, pid_seen) = self.vertex_bloom_check(&cmtr_vid, &pid_vid).await;
        self.write_edge(
            "comments_on_edge",
            EDGE_COMMENTS_ON,
            false,
            &cmtr_vid,
            if cmtr_seen { String::new() } else { ensure_user_vertex_nql(&cmtr_vid, &commenter) },
            &pid_vid,
            if pid_seen { String::new() } else { ensure_post_vertex_nql(&pid_vid, post_id) },
            Some(rank),
            &format!("{PROP_EVENT_ID}, {PROP_COMMENT_TEXT}, {PROP_COMMENTED_AT}"),
            &format!("\"{event_id}\", \"{safe_preview}\", now()"),
        ).await;
        self.mark_vertices_seen(cmtr_vid, pid_vid);
    }

    /// Upsert user + post vertices and insert a `likes` edge.
    pub async fn write_likes_edge(
        &self,
        liker: &str,
        post_id: &str,
        event_id: &str,
        reaction_type: &str,
    ) {
        // VID-CASE-001: normalise before validating (EIP-55 checksummed → accepted).
        let liker = normalize_address(liker);
        if !is_safe_address(&liker)
            || !is_safe_post_vid_id(post_id)
            || !is_safe_id(event_id)
            || !is_safe_id(reaction_type)
        {
            warn!("write_likes_edge: invalid input — liker={liker} post_id={post_id} reaction_type={reaction_type}");
            return;
        }
        // Map reaction_type to edge weight via shared function so the Kafka event
        // processor and API interaction handler both produce consistent weights.
        let weight = map_reaction_weight(reaction_type);
        // UPSERT-HOT-001 / A-04: vertex upserts via ensure_*_vertex_nql helper.
        let lkr_vid = vid_user(&liker);
        let pid_vid = vid_post(post_id);
        // BLOOM-001: skip vertex upsert NQL for vertices already confirmed in Nebula.
        let (lkr_seen, pid_seen) = self.vertex_bloom_check(&lkr_vid, &pid_vid).await;
        self.write_edge(
            "likes_edge",
            EDGE_LIKES,
            true,
            &lkr_vid,
            if lkr_seen { String::new() } else { ensure_user_vertex_nql(&lkr_vid, &liker) },
            &pid_vid,
            if pid_seen { String::new() } else { ensure_post_vertex_nql(&pid_vid, post_id) },
            None,
            &format!("{PROP_EVENT_ID}, {PROP_LIKED_AT}, {PROP_REACTION_TYPE}, {PROP_WEIGHT}"),
            &format!("\"{event_id}\", now(), \"{reaction_type}\", {weight}"),
        ).await;
        self.mark_vertices_seen(lkr_vid, pid_vid);
    }

    /// Write a dedicated `purchases` edge (migration 15).
    ///
    /// Separate from `write_likes_edge` so the purchases edge type carries only
    /// purchase events and `get_purchase_fof_recommendations` can traverse it at
    /// storaged level without a graphd-side WHERE filter on reaction_type.
    ///
    /// Best-effort: never propagates errors to the caller.
    pub async fn write_purchases_edge(
        &self,
        buyer: &str,
        post_id: &str,
        event_id: &str,
    ) {
        // VID-CASE-001: normalise before validating (EIP-55 checksummed → accepted).
        let buyer = normalize_address(buyer);
        if !is_safe_address(&buyer)
            || !is_safe_post_vid_id(post_id)
            || !is_safe_id(event_id)
        {
            warn!("write_purchases_edge: invalid input — buyer={buyer} post_id={post_id}");
            return;
        }
        let buyer_vid = vid_user(&buyer);
        let pid_vid = vid_post(post_id);
        // BLOOM-001: skip vertex upsert NQL for vertices already confirmed in Nebula.
        let (buyer_seen, pid_seen) = self.vertex_bloom_check(&buyer_vid, &pid_vid).await;
        self.write_edge(
            "purchases_edge",
            EDGE_PURCHASES,
            false,
            &buyer_vid,
            if buyer_seen { String::new() } else { ensure_user_vertex_nql(&buyer_vid, &buyer) },
            &pid_vid,
            if pid_seen { String::new() } else { ensure_post_vertex_nql(&pid_vid, post_id) },
            None,
            &format!("{PROP_EVENT_ID}, {PROP_PURCHASED_AT}, {PROP_WEIGHT}"),
            &format!("\"{event_id}\", now(), 2.0"),
        ).await;
        // Invalidate the buyer's FoF and recommendation caches so their followers
        // see the purchase signal on the next feed open rather than after the full TTL.
        if let Some(ref cache) = self.cache {
            cache.delete_fof_all(&buyer).await;
            cache.delete_recommendations(&buyer).await;
        }
        self.mark_vertices_seen(buyer_vid, pid_vid);
    }

    /// Write a `bookmarked` edge when a user saves content via the REST API.
    ///
    /// S30-05: InteractionType::Save fell to the `_ => {}` arm — no Nebula write
    /// happened for bookmark events submitted through the API interaction endpoint.
    /// This mirrors graph_sync::sync_bookmark: IF NOT EXISTS preserves the original
    /// bookmarked_at timestamp on duplicate save events (idempotent).
    pub async fn write_bookmark_edge(&self, user: &str, post_id: &str, event_id: &str) {
        let user = normalize_address(user);
        if !is_safe_address(&user)
            || !is_safe_post_vid_id(post_id)
            || !is_safe_id(event_id)
        {
            warn!("write_bookmark_edge: invalid input — user={user} post_id={post_id}");
            return;
        }
        let usr_vid = vid_user(&user);
        let pid_vid = vid_post(post_id);
        // BLOOM-001: skip vertex upsert NQL for vertices already confirmed in Nebula.
        let (usr_seen, pid_seen) = self.vertex_bloom_check(&usr_vid, &pid_vid).await;
        self.write_edge(
            "bookmarked_edge",
            EDGE_BOOKMARKED,
            true,
            &usr_vid,
            if usr_seen { String::new() } else { ensure_user_vertex_nql(&usr_vid, &user) },
            &pid_vid,
            if pid_seen { String::new() } else { ensure_post_vertex_nql(&pid_vid, post_id) },
            None,
            &format!("{PROP_EVENT_ID}, {PROP_BOOKMARKED_AT}"),
            &format!("\"{event_id}\", now()"),
        ).await;
        self.mark_vertices_seen(usr_vid, pid_vid);
    }

    /// Delete the `bookmarked` edge when a user removes a save via the REST API.
    pub async fn delete_bookmark_edge(&self, user: &str, post_id: &str) {
        let user = normalize_address(user);
        if !is_safe_address(&user) || !is_safe_post_vid_id(post_id) {
            warn!("delete_bookmark_edge: invalid input — user={user} post_id={post_id}");
            return;
        }
        let usr_vid = vid_user(&user);
        let pid_vid = vid_post(post_id);
        self.delete_edge("delete_bookmark_edge", EDGE_BOOKMARKED, &usr_vid, &pid_vid).await;
    }

    // ── FoF traversal helpers ─────────────────────────────────────────────────

    /// Common 5-step scaffold shared by the three FoF methods:
    /// (1) cache read, (2) execute_query, (3) parse, (4) cache write, (5) return.
    ///
    /// Each public method validates and normalises `user_address`, builds its
    /// specific nGQL query, then delegates here.
    async fn fof_traverse(
        &self,
        user_address: &str,
        query: &str,
        bucket: FofBucket,
    ) -> Result<Vec<(String, f64)>> {
        if let Some(ref cache) = self.cache {
            let cached = match bucket {
                FofBucket::FollowLike => cache.get_fof_recommendations(user_address).await,
                FofBucket::ViewEvent  => cache.get_view_fof_recommendations(user_address).await,
                FofBucket::Comment    => cache.get_comment_fof_recommendations(user_address).await,
                FofBucket::Purchase   => cache.get_purchase_fof_recommendations(user_address).await,
                FofBucket::Share      => cache.get_share_fof_recommendations(user_address).await,
                FofBucket::Bookmark   => cache.get_bookmark_fof_recommendations(user_address).await,
            };
            if let Some(results) = cached {
                debug!("FoF cache HIT for {}", user_address);
                metrics::counter!("nebula_fof_cache_hits_total").increment(1);
                // Upcast f32→f64: Redis stores f32 (half the JSON bytes); public API
                // returns f64 for the updater pre-warm path. Engine reads f32 directly
                // via cache.get_fof_recommendations() and never calls fof_traverse.
                return Ok(results.into_iter().map(|(k, v)| (k, v as f64)).collect());
            }
        }

        // Use execute_query_uncached to bypass the nGQL query-string cache (tier-2).
        // delete_fof_all clears the per-user FoF slot (tier-1) on follow/unfollow,
        // but execute_query has its own query-string cache that would re-promote stale
        // data for up to the nGQL TTL (~5 min), silently defeating the invalidation.
        let output = self.execute_query_uncached(query).await?;
        let results = parse_nebula_table(&output, 1, 2);

        if let Some(ref cache) = self.cache {
            match bucket {
                FofBucket::FollowLike => cache.set_fof_recommendations(user_address, &results).await,
                FofBucket::ViewEvent  => cache.set_view_fof_recommendations(user_address, &results).await,
                FofBucket::Comment    => cache.set_comment_fof_recommendations(user_address, &results).await,
                FofBucket::Purchase   => cache.set_purchase_fof_recommendations(user_address, &results).await,
                FofBucket::Share      => cache.set_share_fof_recommendations(user_address, &results).await,
                FofBucket::Bookmark   => cache.set_bookmark_fof_recommendations(user_address, &results).await,
            }
        }

        Ok(results)
    }

    /// ByteGraph FoF traversal — friends-of-friends who liked the same content.
    ///
    /// Changji Li / Hongzhi Chen pattern: multi-hop walk + temporal decay scoring.
    /// Score = (fof_count × 2 + friend_count × 1.5) × avg_engagement × exp(-age_days/7)
    ///
    /// Per-signal exponential decay: likes use a 7-day half-life. A like from 7 days ago
    /// retains e^(-1) ≈ 37% signal weight; harmonic decay gave ~12.5% by comparison,
    /// making the like queue fade too fast. ByteDance/TikTok tuning: purchases 30d,
    /// likes 7d, views 3d, comments 14d — each matched to the real-world shelf life
    /// of that intent signal.
    pub async fn get_fof_recommendations(&self, user_address: &str) -> Result<Vec<(String, f64)>> {
        // S30-09: normalize BEFORE is_safe_address check — EIP-55 checksummed addresses
        // (mixed-case hex) were rejected before normalization, silently returning no recs.
        let user_address = normalize_address(user_address);
        let user_address = user_address.as_str();
        if !is_safe_address(user_address) {
            anyhow::bail!("get_fof_recommendations: invalid address: {user_address}");
        }

        // P3-03: collapse (fof, f) pairs → (fof, friend_count) BEFORE the LIMIT.
        // The old query carried pairs across LIMIT 1000, so a popular fof reached by
        // 10 friends consumed 10 slots and crowded out other fof vertices entirely.
        // avg(l.weight) over the truncated set was then biased toward fof with many friends.
        // Now LIMIT 1000 is on unique fof vertices; friend_count is pre-aggregated.
        // FOF-GO-001: replaced 3-hop MATCH with GO FROM pipe chain.
        // MATCH invokes the property-index planner on every hop even when the
        // source VID is fully known.  GO FROM uses a direct VID-indexed edge
        // scan and is 3-5× faster for known-VID traversals (Vesoft benchmark).
        // friend_count survives the third hop via the $- carry-through pattern.
        let addr_vid = vid_user(user_address);
        // Note: purchase likes (reaction_type="purchase") are excluded here via
        // WHERE != "purchase" to prevent double-counting with get_purchase_fof_recommendations.
        // Migration 15 writes purchases to both `likes` (with reaction_type="purchase")
        // and the dedicated `purchases` edge type. Without this filter, a purchase is
        // boosted once by get_fof_recommendations and again by get_purchase_fof_recommendations,
        // producing ~2× the intended purchase weight in apply_cache_boosts.
        // Long-term: backfill + delete reaction_type="purchase" rows from `likes`.
        let query = format!(
            r#"USE {space};
GO 1 STEPS FROM "{addr_vid}" OVER {e_follows}
  YIELD dst(edge) AS f_vid
  ORDER BY {e_follows}.{p_weight} DESC
  LIMIT 500
| GO 1 STEPS FROM $-.f_vid OVER {e_follows}
  YIELD $-.f_vid AS f_vid, dst(edge) AS fof_vid
  LIMIT 5000
| GROUP BY $-.fof_vid
  YIELD $-.fof_vid AS fof_vid,
        count(DISTINCT $-.f_vid) AS friend_count
| ORDER BY friend_count DESC
  LIMIT 1000
| GO 1 STEPS FROM $-.fof_vid OVER {e_likes}
  WHERE {e_likes}.{p_rt} != "purchase"
  YIELD $-.fof_vid AS fof_vid, $-.friend_count AS friend_count,
        dst(edge) AS n_vid, {e_likes}.{p_weight} AS w, {e_likes}.{p_liked_at} AS liked_at
  LIMIT 5000
| GROUP BY $-.n_vid
  YIELD $-.n_vid AS post_id,
        (count(DISTINCT $-.fof_vid) * 2.0 + sum($-.friend_count) * 1.5)
            * avg($-.w)
            * CASE WHEN max($-.liked_at) IS NULL OR max($-.liked_at) > timestamp() THEN 1.0
               ELSE exp(-1.0 * toFloat(timestamp() - max($-.liked_at)) / 86400.0 / 7.0)
               END AS score
| ORDER BY score DESC LIMIT 50;"#,
            space = SPACE_THERAGRAPH,
            addr_vid = addr_vid,
            e_follows = EDGE_FOLLOWS,
            e_likes = EDGE_LIKES,
            p_weight = PROP_WEIGHT,
            p_liked_at = PROP_LIKED_AT,
            p_rt = PROP_REACTION_TYPE,
        );

        self.fof_traverse(user_address, &query, FofBucket::FollowLike).await
    }

    /// Dwell-weighted FoF — traverses view_event edges instead of likes.
    ///
    /// Content your friends watched for a long time is a stronger signal than
    /// a like (which takes one tap). Score weights dwell time exponentially:
    /// 30s view > 10 quick likes in terms of prediction quality.
    ///
    /// 3-day exponential half-life: views are ephemeral — content you watched
    /// three days ago is no longer in active consideration. ByteGraph signal
    /// shelf life: view=3d is the shortest decay, matched to session-length browsing.
    pub async fn get_view_event_fof_recommendations(
        &self,
        user_address: &str,
    ) -> Result<Vec<(String, f64)>> {
        // S30-09: normalize BEFORE is_safe_address check — same fix as get_fof_recommendations.
        let user_address = normalize_address(user_address);
        let user_address = user_address.as_str();
        if !is_safe_address(user_address) {
            anyhow::bail!("get_view_event_fof_recommendations: invalid address: {user_address}");
        }

        // FOF-GO-002: replaced 2-hop MATCH with GO FROM pipe chain.
        // NOTE: GO WHERE on edge properties is evaluated at graphd, NOT storaged.
        // view_event_duration_idx (migration 06) is only consumed by LOOKUP ON,
        // not by GO FROM — storaged deserializes and ships all edges to graphd,
        // which then applies the WHERE filter. The filter still reduces result set
        // size for downstream stages, but does not avoid edge deserialization.
        let addr_vid = vid_user(user_address);
        let query = format!(
            r#"USE {space};
GO 1 STEPS FROM "{addr_vid}" OVER {e_follows}
  YIELD dst(edge) AS f_vid
  ORDER BY {e_follows}.{p_weight} DESC
  LIMIT 500
| GO 1 STEPS FROM $-.f_vid OVER {e_view_event}
  WHERE {e_view_event}.{p_dur} > 5
  YIELD $-.f_vid AS f_vid, dst(edge) AS n_vid,
        {e_view_event}.{p_dur} AS dur_secs,
        {e_view_event}.{p_event_time} AS etime
  LIMIT 10000
| GROUP BY $-.n_vid
  YIELD $-.n_vid AS post_id,
        count(DISTINCT $-.f_vid) AS friend_count,
        sum(toFloat($-.dur_secs)) AS total_dwell,
        max($-.etime) AS most_recent_view
| YIELD $-.post_id AS post_id,
        $-.friend_count * ($-.total_dwell / 60.0)
            * CASE WHEN $-.most_recent_view IS NULL OR $-.most_recent_view > timestamp() THEN 1.0
               ELSE exp(-1.0 * toFloat(timestamp() - $-.most_recent_view) / 86400.0 / 3.0)
               END AS score
| ORDER BY score DESC LIMIT 50;"#,
            space = SPACE_THERAGRAPH,
            addr_vid = addr_vid,
            e_follows = EDGE_FOLLOWS,
            e_view_event = EDGE_VIEW_EVENT,
            p_dur = PROP_DURATION_SECONDS,
            p_event_time = PROP_EVENT_TIME,
            p_weight = PROP_WEIGHT,
        );

        self.fof_traverse(user_address, &query, FofBucket::ViewEvent).await
    }

    /// Graph-walked user suggestions for the "Who to Follow" surface on a profile page.
    ///
    /// Strategy: people who have viewed `viewing_creator`'s content are the audience
    /// most likely to also enjoy related creators. We walk their follow edges to surface
    /// users not yet followed by `viewer_address`. Ranked by how many of the creator's
    /// audience follow them — a proxy for "well-known in this community."
    ///
    /// Two-pass approach eliminates the last remaining MATCH query:
    /// Pass 1 (GO FROM viewer OVER follows) builds a Rust HashSet of already-followed VIDs.
    /// Pass 2 (GO FROM creator OVER creator_affinity REVERSELY → follows) finds suggested users.
    /// Rust-side filter replaces the MATCH WITH-collect anti-join, which was 3-5× slower
    /// than VID-indexed GO FROM traversals (Vesoft benchmark).
    pub async fn get_viewer_based_user_suggestions(
        &self,
        viewer_address: &str,
        viewing_creator: &str,
        limit: usize,
    ) -> Result<Vec<(String, f64)>> {
        // S30-09: normalize BEFORE is_safe_address — EIP-55 checksummed addresses rejected otherwise.
        let viewer_address = normalize_address(viewer_address);
        let viewer_address = viewer_address.as_str();
        let viewing_creator = normalize_address(viewing_creator);
        let viewing_creator = viewing_creator.as_str();
        if !is_safe_address(viewer_address) || !is_safe_address(viewing_creator) {
            anyhow::bail!("get_viewer_based_user_suggestions: invalid address");
        }
        let safe_limit = limit.min(50);

        if let Some(ref cache) = self.cache {
            if let Some(cached) = cache.get_user_suggestions(viewer_address, viewing_creator).await {
                return Ok(cached.into_iter().map(|(k, v)| (k, v as f64)).collect());
            }
        }

        let viewer_vid = vid_user(viewer_address);
        let creator_vid = vid_user(viewing_creator);

        // Pass 1: collect the set of VIDs the viewer already follows.
        // Direct VID-indexed scan — no index planner overhead.
        let query1 = format!(
            r#"USE {space};
GO 1 STEPS FROM "{viewer_vid}" OVER {e_follows}
  YIELD dst(edge) AS fid
  LIMIT 2000;"#,
            space = SPACE_THERAGRAPH,
            viewer_vid = viewer_vid,
            e_follows = EDGE_FOLLOWS,
        );
        // NEBULA-005: get_viewer_based_user_suggestions must use execute_query_uncached
        // so each call reflects the current follow graph — a cached stale result would
        // filter the wrong "already following" set and surface users the viewer just
        // followed. fof_traverse already uses execute_query_uncached; align here.
        let output1 = self.execute_query_uncached(&query1).await?;
        let already_following: HashSet<String> = output1
            .lines()
            .filter_map(|line| {
                let line = line.trim();
                if line.starts_with('+') || line.is_empty() {
                    return None;
                }
                let parts: Vec<&str> = line.split('|').map(str::trim).collect();
                if parts.len() <= 1 {
                    return None;
                }
                let vid = parts[1].trim_matches('"');
                if vid.is_empty() || vid.contains(' ') {
                    None
                } else {
                    Some(vid.to_string())
                }
            })
            .collect();

        // Pass 2: walk from the creator via creator_affinity REVERSELY to find viewers
        // of their content, then follow those viewers' follows edges to surface suggested
        // users. REVERSELY uses storaged's incoming-edge scan — no additional index required.
        // Fetch safe_limit*4 candidates so the Rust filter has enough to fill safe_limit slots.
        let query2 = format!(
            r#"USE {space};
GO 1 STEPS FROM "{creator_vid}" OVER {e_creator_affinity} REVERSELY
  YIELD src(edge) AS viewer_vid
  LIMIT 300
| GO 1 STEPS FROM $-.viewer_vid OVER {e_follows}
  YIELD $-.viewer_vid AS src_vid, dst(edge) AS suggested_vid
  LIMIT 10000
| GROUP BY $-.suggested_vid
  YIELD $-.suggested_vid AS user_id,
        toFloat(count(DISTINCT $-.src_vid)) AS mutual_count
| ORDER BY mutual_count DESC LIMIT {over_limit};"#,
            space = SPACE_THERAGRAPH,
            creator_vid = creator_vid,
            e_creator_affinity = EDGE_CREATOR_AFFINITY,
            e_follows = EDGE_FOLLOWS,
            over_limit = safe_limit * 4,
        );
        let output2 = self.execute_query_uncached(&query2).await?;
        let raw = parse_nebula_table(&output2, 1, 2);

        // Rust-side anti-join: remove already-followed users, the viewer themselves,
        // and the creator whose profile the viewer is already on (suggesting "follow
        // this person" while you're browsing their profile is redundant and confusing).
        let creator_vid = vid_user(viewing_creator);
        let results: Vec<(String, f64)> = raw
            .into_iter()
            .filter(|(id, _)| {
                id.as_str() != viewer_vid.as_str()
                    && id.as_str() != creator_vid.as_str()
                    && !already_following.contains(id.as_str())
            })
            .take(safe_limit)
            .collect();

        if let Some(ref cache) = self.cache {
            cache.set_user_suggestions(viewer_address, viewing_creator, &results).await;
        }

        Ok(results)
    }

    /// Comment-weighted FoF — traverses comments_on edges instead of likes.
    ///
    /// Comments are the highest-intent positive signal in the graph: a user who
    /// typed about a post spent deliberate attention on it. Friends who commented
    /// on a post are a near-certain quality signal for recommendation.
    ///
    /// Score = comment_count × friend_count × exp(-age_days/14)
    ///
    /// 14-day exponential half-life: comments represent deliberate engagement
    /// (the user composed text). Signal decays slower than a passive view (3d)
    /// or a single-tap like (7d) — commenting on an NFT means it occupied
    /// working memory long enough to type.
    pub async fn get_comment_fof_recommendations(
        &self,
        user_address: &str,
    ) -> Result<Vec<(String, f64)>> {
        // S30-09: normalize BEFORE is_safe_address — EIP-55 checksummed addresses rejected otherwise.
        let user_address = normalize_address(user_address);
        let user_address = user_address.as_str();
        if !is_safe_address(user_address) {
            anyhow::bail!("get_comment_fof_recommendations: invalid address: {user_address}");
        }

        // FOF-GO-003: replaced 2-hop MATCH with GO FROM pipe chain.
        // Only YIELD the two properties used in scoring (f_vid for dedup,
        // commented_at for recency decay).  Dropping event_id and comment_text
        // eliminates ~250 bytes of intermediate data per edge at the 5000-edge
        // limit (~1.2 MB per query execution).
        let addr_vid = vid_user(user_address);
        let query = format!(
            r#"USE {space};
GO 1 STEPS FROM "{addr_vid}" OVER {e_follows}
  YIELD dst(edge) AS f_vid
  LIMIT 200
| GO 1 STEPS FROM $-.f_vid OVER {e_comments_on}
  YIELD $-.f_vid AS f_vid, dst(edge) AS n_vid,
        {e_comments_on}.{p_commented_at} AS commented_at
  LIMIT 5000
| GROUP BY $-.n_vid
  YIELD $-.n_vid AS post_id,
        count(DISTINCT $-.f_vid) AS friend_count,
        count($-.f_vid)          AS comment_count,
        max($-.commented_at)     AS most_recent_comment
| YIELD $-.post_id AS post_id,
        toFloat($-.comment_count) * toFloat($-.friend_count)
            * CASE WHEN $-.most_recent_comment IS NULL OR $-.most_recent_comment > timestamp() THEN 1.0
               ELSE exp(-1.0 * toFloat(timestamp() - $-.most_recent_comment) / 86400.0 / 14.0)
               END AS score
| ORDER BY score DESC LIMIT 50;"#,
            space = SPACE_THERAGRAPH,
            addr_vid = addr_vid,
            e_follows = EDGE_FOLLOWS,
            e_comments_on = EDGE_COMMENTS_ON,
            p_commented_at = PROP_COMMENTED_AT,
        );

        self.fof_traverse(user_address, &query, FofBucket::Comment).await
    }

    /// Purchase-weighted FoF — traverses the dedicated `purchases` edge type (migration 15).
    ///
    /// Purchases are the strongest intent signal in the TheraGraph model: a user paid
    /// real value for this NFT. ByteGraph tier: purchase half-life is 30 days — 4× longer
    /// than a like — because ownership persists. A friend who bought an NFT 20 days ago
    /// is still a strong taste signal.
    ///
    /// MIGRATION 15: purchases now use the dedicated `purchases` edge type rather than
    /// `likes WHERE reaction_type="purchase"`. The edge-type scan is the selectivity filter
    /// at storaged — no graphd-side WHERE clause, no over-reading of non-purchase like edges.
    ///
    /// BACKFILL NOTE: Historical purchases written before migration 15 remain in the `likes`
    /// edge as reaction_type="purchase". They are NOT returned by this query. Run the
    /// backfill script in theragraph-nebula/init/15-add-purchases-edge.ngql to migrate them.
    ///
    /// Score = friend_count × 3.0 × exp(-age_days/30): the 3.0 multiplier reflects that
    /// a single friend purchase outweighs a single friend like by 3× in prediction quality.
    pub async fn get_purchase_fof_recommendations(
        &self,
        user_address: &str,
    ) -> Result<Vec<(String, f64)>> {
        // S30-09: normalize BEFORE is_safe_address — EIP-55 checksummed addresses rejected otherwise.
        let user_address = normalize_address(user_address);
        let user_address = user_address.as_str();
        if !is_safe_address(user_address) {
            anyhow::bail!("get_purchase_fof_recommendations: invalid address: {user_address}");
        }

        let addr_vid = vid_user(user_address);
        let query = format!(
            r#"USE {space};
GO 1 STEPS FROM "{addr_vid}" OVER {e_follows}
  YIELD dst(edge) AS f_vid
  LIMIT 200
| GO 1 STEPS FROM $-.f_vid OVER {e_purchases}
  YIELD $-.f_vid AS f_vid, dst(edge) AS n_vid,
        {e_purchases}.{p_purchased_at} AS purchased_at
  LIMIT 5000
| GROUP BY $-.n_vid
  YIELD $-.n_vid AS post_id,
        count(DISTINCT $-.f_vid) AS friend_count,
        max($-.purchased_at)     AS most_recent_purchase
| YIELD $-.post_id AS post_id,
        toFloat($-.friend_count) * 3.0
            * CASE WHEN $-.most_recent_purchase IS NULL OR $-.most_recent_purchase > timestamp() THEN 1.0
               ELSE exp(-1.0 * toFloat(timestamp() - $-.most_recent_purchase) / 86400.0 / 30.0)
               END AS score
| ORDER BY score DESC LIMIT 50;"#,
            space = SPACE_THERAGRAPH,
            addr_vid = addr_vid,
            e_follows = EDGE_FOLLOWS,
            e_purchases = EDGE_PURCHASES,
            p_purchased_at = PROP_PURCHASED_AT,
        );

        self.fof_traverse(user_address, &query, FofBucket::Purchase).await
    }

    /// Share-weighted FoF — traverses the `shared` edge type (migration 009).
    ///
    /// Shares are the strongest social broadcast signal in TheraGraph: a user explicitly
    /// chose to distribute content to their network. Unlike likes (passive approval) or
    /// views (passive exposure), sharing requires intent — the user staked their social
    /// reputation on the content.
    ///
    /// ByteGraph tier: share half-life is 15 days — between purchase (30d) and like (7d)
    /// because shares persist in social context longer than a like but lose relevance
    /// faster than an economic commitment. Score multiplier: 1.5× (stronger than like,
    /// weaker than purchase which carries real THERA value).
    ///
    /// Score = friend_count × 1.5 × exp(-age_days/15)
    pub async fn get_shared_fof_recommendations(
        &self,
        user_address: &str,
    ) -> Result<Vec<(String, f64)>> {
        // S30-09: normalize BEFORE is_safe_address — EIP-55 checksummed addresses rejected otherwise.
        let user_address = normalize_address(user_address);
        let user_address = user_address.as_str();
        if !is_safe_address(user_address) {
            anyhow::bail!("get_shared_fof_recommendations: invalid address: {user_address}");
        }

        let addr_vid = vid_user(user_address);
        let query = format!(
            r#"USE {space};
GO 1 STEPS FROM "{addr_vid}" OVER {e_follows}
  YIELD dst(edge) AS f_vid
  LIMIT 200
| GO 1 STEPS FROM $-.f_vid OVER {e_shared}
  YIELD $-.f_vid AS f_vid, dst(edge) AS n_vid,
        {e_shared}.{p_shared_at} AS shared_at
  LIMIT 5000
| GROUP BY $-.n_vid
  YIELD $-.n_vid AS post_id,
        count(DISTINCT $-.f_vid) AS friend_count,
        max($-.shared_at)        AS most_recent_share
| YIELD $-.post_id AS post_id,
        toFloat($-.friend_count) * 1.5
            * CASE WHEN $-.most_recent_share IS NULL OR $-.most_recent_share > timestamp() THEN 1.0
               ELSE exp(-1.0 * toFloat(timestamp() - $-.most_recent_share) / 86400.0 / 15.0)
               END AS score
| ORDER BY score DESC LIMIT 50;"#,
            space = SPACE_THERAGRAPH,
            addr_vid = addr_vid,
            e_follows = EDGE_FOLLOWS,
            e_shared = EDGE_SHARED,
            p_shared_at = PROP_SHARED_AT,
        );

        self.fof_traverse(user_address, &query, FofBucket::Share).await
    }

    /// Bookmark-weighted FoF — traverses the `bookmarked` edge type.
    ///
    /// Bookmarks are a strong private save-for-later signal: unlike likes (passive approval)
    /// or shares (public broadcast), a bookmark is a deliberate "I want to return to this"
    /// gesture. No social incentive — pure personal interest.
    ///
    /// ByteGraph tier: bookmark half-life is 10 days — shorter than share (15d) because
    /// saved content loses relevance faster as the feed evolves. Score multiplier: 1.2×
    /// (above view/comment 1.0×, below share 1.5× since there's no social amplification).
    ///
    /// Score = friend_count × 1.2 × exp(-age_days/10)
    pub async fn get_bookmark_fof_recommendations(
        &self,
        user_address: &str,
    ) -> Result<Vec<(String, f64)>> {
        // S30-09: normalize BEFORE is_safe_address — EIP-55 checksummed addresses rejected otherwise.
        let user_address = normalize_address(user_address);
        let user_address = user_address.as_str();
        if !is_safe_address(user_address) {
            anyhow::bail!("get_bookmark_fof_recommendations: invalid address: {user_address}");
        }

        let addr_vid = vid_user(user_address);
        let query = format!(
            r#"USE {space};
GO 1 STEPS FROM "{addr_vid}" OVER {e_follows}
  YIELD dst(edge) AS f_vid
  LIMIT 200
| GO 1 STEPS FROM $-.f_vid OVER {e_bm}
  YIELD $-.f_vid AS f_vid, dst(edge) AS n_vid,
        {e_bm}.{p_bm_at} AS bookmarked_at
  LIMIT 5000
| GROUP BY $-.n_vid
  YIELD $-.n_vid AS post_id,
        count(DISTINCT $-.f_vid) AS friend_count,
        max($-.bookmarked_at)    AS most_recent_bookmark
| YIELD $-.post_id AS post_id,
        toFloat($-.friend_count) * 1.2
            * CASE WHEN $-.most_recent_bookmark IS NULL OR $-.most_recent_bookmark > timestamp() THEN 1.0
               ELSE exp(-1.0 * toFloat(timestamp() - $-.most_recent_bookmark) / 86400.0 / 10.0)
               END AS score
| ORDER BY score DESC LIMIT 50;"#,
            space = SPACE_THERAGRAPH,
            addr_vid = addr_vid,
            e_follows = EDGE_FOLLOWS,
            e_bm = EDGE_BOOKMARKED,
            p_bm_at = PROP_BOOKMARKED_AT,
        );

        self.fof_traverse(user_address, &query, FofBucket::Bookmark).await
    }

    /// Batch-write `recommended_to` edges for a set of served NFTs.
    ///
    /// Called after the engine computes and returns a feed. Each edge records
    /// the score at serve-time and `served = false` (unclicked). When the user
    /// clicks/purchases, `mark_recommendation_served` flips `served = true`.
    ///
    /// Fires in a background task — must not block the API response path.
    /// Best-effort: errors are logged but never propagated.
    pub async fn write_recommended_to_batch(
        &self,
        user_address: &str,
        served: &[(&str, f32)],
    ) -> Result<()> {
        // VID-CASE-001: normalise before validating (EIP-55 checksummed → accepted).
        let user_address = normalize_address(user_address);
        let user_address = user_address.as_str();
        if !is_safe_address(user_address) || served.is_empty() {
            return Ok(());
        }
        // P3-05: pre-allocate with a capacity estimate to avoid N=50 intermediate allocations
        // from push_str(&format!(...)). Each NFT block is ~540 bytes; user vertex is ~130.
        use std::fmt::Write as _;
        let est_capacity = 32 + 130 + served.len() * 540;
        let mut query = String::with_capacity(est_capacity);
        let _ = write!(query, "USE {SPACE_THERAGRAPH};\n");

        // P3-04: emit the user vertex INSERT once outside the loop — 50 identical INSERTs
        // in the old code were guaranteed no-ops after the first one and wasted CPU + Nebula I/O.
        let user_vid = vid_user(&user_address);
        let _ = write!(
            query,
            "INSERT VERTEX IF NOT EXISTS user(id, username, followers_count, following_count, total_likes_given, total_posts) VALUES \"{user_vid}\":(\"{addr}\", \"\", 0, 0, 0, 0);\n",
            user_vid = user_vid,
            addr = user_address,
        );

        // Min-max normalize scores within this batch so the recommended_to edge
        // preserves relative ranking. Clamping to [0,1] would flatten the top ~20-30%
        // of scores to 1.0 (since FoF scores >> 1.0 are common), destroying the
        // differentiation needed by the feedback loop to learn which rank drove engagement.
        let max_score = served.iter().map(|(_, s)| *s).fold(f32::MIN, f32::max);
        let min_score = served.iter().map(|(_, s)| *s).fold(f32::MAX, f32::min);
        let score_range = max_score - min_score;

        let mut has_valid = false;
        for (nft_id, score) in served {
            if !is_safe_post_vid_id(nft_id) {
                continue;
            }
            has_valid = true;
            // When all items share the same score (common after FoF clamping), store 1.0
            // rather than 0.0 (what EPSILON division would produce). 0.0 tells the
            // feedback loop "lowest quality ever shown", corrupting future ranking.
            let sc = if score_range < f32::EPSILON {
                1.0f32
            } else {
                ((score - min_score) / score_range).clamp(0.0, 1.0)
            };
            let nft_vid = vid_post(nft_id);
            // F01: INSERT VERTEX IF NOT EXISTS — never overwrites real user/post data.
            // F02: Split into INSERT (new edges, served=false) + conditional UPDATE
            // (existing edges, score/time only, WHEN served==false).
            let _ = write!(
                query,
                "INSERT VERTEX IF NOT EXISTS post(id, content, author_id, views, likes, hashtags, content_type) VALUES \"{nft_vid}\":(\"{nft}\", \"\", \"\", 0, 0, \"\", \"\");\n\
                 INSERT EDGE IF NOT EXISTS {e_rec_to}({p_score}, {p_served}, {p_computed_at}) VALUES \"{user_vid}\" -> \"{nft_vid}\"@0:({sc}, false, now());\n\
                 UPDATE EDGE ON {e_rec_to} \"{user_vid}\" -> \"{nft_vid}\"@0 SET {p_score} = {sc}, {p_computed_at} = now() WHEN {e_rec_to}.{p_served} == false;\n",
                nft_vid = nft_vid,
                nft = nft_id,
                user_vid = user_vid,
                e_rec_to = EDGE_RECOMMENDED_TO,
                sc = sc,
                p_score = PROP_SCORE,
                p_served = PROP_SERVED,
                p_computed_at = PROP_COMPUTED_AT,
            );
        }
        if !has_valid {
            return Ok(());
        }
        // S30-15: propagate error so spawn_feedback_write can log at the call site.
        // Internal error! logging preserved here for structured tracing; caller gets
        // the Result to increment failure metrics without swallowing the root cause.
        self.execute_write(&query).await
            .map(|_| ())
            .map_err(|e| {
                error!("Nebula write_recommended_to_batch failed ({} items): {}", served.len(), e);
                e
            })
    }

    /// Mark a `recommended_to` edge as clicked/purchased (served = true).
    ///
    /// Called by the interaction API when the user opens or purchases an NFT
    /// that was served via the recommendation engine. This closes the feedback
    /// loop: the engine now knows its recommendation was acted upon.
    pub async fn mark_recommendation_served(&self, user_address: &str, nft_id: &str) {
        if !is_safe_address(user_address) || !is_safe_post_vid_id(nft_id) {
            warn!("mark_recommendation_served: invalid input — addr={user_address} nft={nft_id}");
            return;
        }
        // VID-CASE-001: normalise to lowercase.
        let user_address = normalize_address(user_address);
        // P3-02: UPDATE EDGE (not UPSERT EDGE) — if the edge doesn't exist (no prior
        // write_recommended_to_batch call for this pair), UPDATE is a no-op.
        // UPSERT would create a phantom served=true edge with score=0 and no computed_at,
        // poisoning the feedback-loop query that reads served=true edges.
        let addr_vid = vid_user(&user_address);
        let nft_vid = vid_post(nft_id);
        let query = format!(
            r#"USE {space};
UPDATE EDGE ON {e_rec_to} "{addr_vid}" -> "{nft_vid}"@0
SET {p_served} = true,
    {p_computed_at} = now()
WHEN {e_rec_to}.{p_served} == false;"#,
            space = SPACE_THERAGRAPH,
            e_rec_to = EDGE_RECOMMENDED_TO,
            addr_vid = addr_vid,
            nft_vid = nft_vid,
            p_served = PROP_SERVED,
            p_computed_at = PROP_COMPUTED_AT,
        );
        if let Err(e) = self.execute_write(&query).await {
            error!("Nebula mark_recommendation_served failed (best-effort): {e}");
        }
    }

    /// Batch-flip `served = true` on multiple `recommended_to` edges in a single nGQL call.
    ///
    /// Replaces the N-subprocess-per-request loop in the API handler (finding 2 from S24
    /// audit). Same UPDATE EDGE ON semantics as the single variant — no-op when edge doesn't
    /// exist, never creates phantom edges.
    pub async fn mark_recommendations_served_batch(&self, user_address: &str, nft_ids: &[&str]) {
        use std::fmt::Write as _;
        if nft_ids.is_empty() {
            return;
        }
        if !is_safe_address(user_address) {
            warn!("mark_recommendations_served_batch: invalid address={user_address}");
            return;
        }
        let user_address = normalize_address(user_address);
        let addr_vid = vid_user(&user_address);
        let mut query = format!("USE {space};\n", space = SPACE_THERAGRAPH);
        let mut has_valid = false;
        for nft_id in nft_ids {
            if !is_safe_post_vid_id(nft_id) {
                warn!("mark_recommendations_served_batch: unsafe nft_id={nft_id} — skipping");
                continue;
            }
            let nft_vid = vid_post(nft_id);
            let _ = write!(
                query,
                "UPDATE EDGE ON {e_rec_to} \"{addr_vid}\" -> \"{nft_vid}\"@0 \
                 SET {p_served} = true, {p_computed_at} = now() \
                 WHEN {e_rec_to}.{p_served} == false;\n",
                e_rec_to = EDGE_RECOMMENDED_TO,
                addr_vid = addr_vid,
                nft_vid = nft_vid,
                p_served = PROP_SERVED,
                p_computed_at = PROP_COMPUTED_AT,
            );
            has_valid = true;
        }
        if !has_valid {
            return;
        }
        if let Err(e) = self.execute_write(&query).await {
            error!("Nebula mark_recommendations_served_batch failed ({} items): {e}", nft_ids.len());
        }
    }

} // end impl<T: GraphTransport> GraphClient<T>

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

    async fn write_recommended_to_batch(&self, user_address: &str, served: &[(&str, f32)]) -> Result<()> {
        GraphClient::write_recommended_to_batch(self, user_address, served).await
    }

    async fn mark_recommendation_served(&self, user_address: &str, nft_id: &str) {
        GraphClient::mark_recommendation_served(self, user_address, nft_id).await;
    }

    async fn write_purchases_edge(&self, buyer: &str, post_id: &str, event_id: &str) {
        GraphClient::write_purchases_edge(self, buyer, post_id, event_id).await;
    }

    async fn mark_recommendations_served_batch(&self, user_address: &str, nft_ids: &[&str]) {
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
}

// ── RS-09: Mock transport seam — tests for circuit-breaker and transport wiring ──

#[cfg(test)]
pub mod test_support {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// Configurable failing transport for circuit-breaker and recovery tests.
    pub struct FailingTransport {
        pub fail_count: AtomicU32,
        pub fail_limit: u32, // fail this many times then succeed
    }

    impl FailingTransport {
        pub fn always_fail() -> Self {
            Self { fail_count: AtomicU32::new(0), fail_limit: u32::MAX }
        }
        pub fn fail_then_recover(n: u32) -> Self {
            Self { fail_count: AtomicU32::new(0), fail_limit: n }
        }
    }

    impl GraphTransport for FailingTransport {
        fn execute(&self, _query: &str) -> impl std::future::Future<Output = Result<String>> + Send {
            let calls = self.fail_count.fetch_add(1, Ordering::Relaxed);
            let fail_limit = self.fail_limit;
            async move {
                if calls < fail_limit {
                    anyhow::bail!("injected failure #{}", calls + 1)
                }
                Ok(String::new())
            }
        }
    }

    /// No-op transport that always succeeds — use to test cache layer in isolation.
    pub struct NopTransport;
    impl GraphTransport for NopTransport {
        fn execute(&self, _query: &str) -> impl std::future::Future<Output = Result<String>> + Send {
            async { Ok(String::new()) }
        }
    }

    /// Records every query string passed through it — used as a golden-string
    /// safety net for write_* method refactors: capture the exact nGQL a
    /// method emits before changing its implementation, then assert the
    /// refactored version emits byte-identical output for the same inputs.
    pub struct CapturingTransport {
        pub queries: std::sync::Mutex<Vec<String>>,
    }

    impl CapturingTransport {
        pub fn new() -> Self {
            Self { queries: std::sync::Mutex::new(Vec::new()) }
        }

        pub fn last_query(&self) -> String {
            self.queries.lock().unwrap().last().cloned().unwrap_or_default()
        }
    }

    impl GraphTransport for CapturingTransport {
        fn execute(&self, query: &str) -> impl std::future::Future<Output = Result<String>> + Send {
            self.queries.lock().unwrap().push(query.to_string());
            async { Ok(String::new()) }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_support::{CapturingTransport, FailingTransport};

    // ── Golden-string safety net for the write_edge/delete_edge consolidation ──
    // No cache is configured on these GraphClients, so vertex_bloom_check always
    // returns (false, false) and every upsert clause is always emitted — the
    // generated query is a pure deterministic function of the inputs below.
    // These lock in the exact nGQL each write_* method produced BEFORE the
    // write_edge/delete_edge helper existed; the refactor must keep them
    // byte-identical.

    const ADDR_A: &str = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const ADDR_B: &str = "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const POST_ID: &str = "post-uuid-1234";
    const EVENT_ID: &str = "0xdeadbeef";

    #[tokio::test]
    async fn golden_write_follows_edge() {
        let gc = GraphClient::with_transport(CapturingTransport::new());
        let transport = Arc::clone(&gc.transport);
        gc.write_follows_edge(ADDR_A, ADDR_B, EVENT_ID).await;
        let expected = format!(
            "USE {space};\n{upsert_fwr}\n{upsert_fwe}\nINSERT EDGE IF NOT EXISTS {e_follows}({p_eid}, {p_followed_at}, {p_weight}) VALUES \"{fwr_vid}\" -> \"{fwe_vid}\":(\"{eid}\", now(), 1.0);",
            space = SPACE_THERAGRAPH,
            upsert_fwr = ensure_user_vertex_nql(&vid_user(ADDR_A), ADDR_A),
            upsert_fwe = ensure_user_vertex_nql(&vid_user(ADDR_B), ADDR_B),
            e_follows = EDGE_FOLLOWS,
            p_eid = PROP_EVENT_ID,
            p_followed_at = PROP_FOLLOWED_AT,
            p_weight = PROP_WEIGHT,
            fwr_vid = vid_user(ADDR_A),
            fwe_vid = vid_user(ADDR_B),
            eid = EVENT_ID,
        );
        assert_eq!(transport.last_query(), expected);
    }

    #[tokio::test]
    async fn golden_delete_follows_edge() {
        let gc = GraphClient::with_transport(CapturingTransport::new());
        let transport = Arc::clone(&gc.transport);
        gc.delete_follows_edge(ADDR_A, ADDR_B).await;
        let expected = format!(
            "USE {space};\nDELETE EDGE {e_follows} \"{fwr_vid}\" -> \"{fwe_vid}\";",
            space = SPACE_THERAGRAPH,
            e_follows = EDGE_FOLLOWS,
            fwr_vid = vid_user(ADDR_A),
            fwe_vid = vid_user(ADDR_B),
        );
        assert_eq!(transport.last_query(), expected);
    }

    #[tokio::test]
    async fn golden_write_comments_on() {
        let gc = GraphClient::with_transport(CapturingTransport::new());
        let transport = Arc::clone(&gc.transport);
        gc.write_comments_on(ADDR_A, POST_ID, EVENT_ID, "nice post").await;
        let rank = comment_rank(EVENT_ID);
        let expected = format!(
            "USE {space};\n{upsert_cmtr}\n{upsert_pid}\nINSERT EDGE {e_comments_on}({p_eid}, {p_comment_text}, {p_commented_at}) VALUES \"{cmtr_vid}\" -> \"{pid_vid}\"@{rank}:(\"{eid}\", \"{preview}\", now());",
            space = SPACE_THERAGRAPH,
            upsert_cmtr = ensure_user_vertex_nql(&vid_user(ADDR_A), ADDR_A),
            upsert_pid = ensure_post_vertex_nql(&vid_post(POST_ID), POST_ID),
            cmtr_vid = vid_user(ADDR_A),
            pid_vid = vid_post(POST_ID),
            e_comments_on = EDGE_COMMENTS_ON,
            rank = rank,
            eid = EVENT_ID,
            preview = "nice post",
            p_eid = PROP_EVENT_ID,
            p_comment_text = PROP_COMMENT_TEXT,
            p_commented_at = PROP_COMMENTED_AT,
        );
        assert_eq!(transport.last_query(), expected);
    }

    #[tokio::test]
    async fn golden_write_likes_edge() {
        let gc = GraphClient::with_transport(CapturingTransport::new());
        let transport = Arc::clone(&gc.transport);
        gc.write_likes_edge(ADDR_A, POST_ID, EVENT_ID, "like").await;
        let weight = map_reaction_weight("like");
        let expected = format!(
            "USE {space};\n{upsert_lkr}\n{upsert_pid}\nINSERT EDGE IF NOT EXISTS {e_likes}({p_eid}, {p_liked_at}, {p_rt}, {p_weight}) VALUES \"{lkr_vid}\" -> \"{pid_vid}\":(\"{eid}\", now(), \"{rt}\", {wt});",
            space = SPACE_THERAGRAPH,
            upsert_lkr = ensure_user_vertex_nql(&vid_user(ADDR_A), ADDR_A),
            upsert_pid = ensure_post_vertex_nql(&vid_post(POST_ID), POST_ID),
            lkr_vid = vid_user(ADDR_A),
            pid_vid = vid_post(POST_ID),
            e_likes = EDGE_LIKES,
            eid = EVENT_ID,
            rt = "like",
            wt = weight,
            p_eid = PROP_EVENT_ID,
            p_liked_at = PROP_LIKED_AT,
            p_rt = PROP_REACTION_TYPE,
            p_weight = PROP_WEIGHT,
        );
        assert_eq!(transport.last_query(), expected);
    }

    #[tokio::test]
    async fn golden_write_purchases_edge() {
        let gc = GraphClient::with_transport(CapturingTransport::new());
        let transport = Arc::clone(&gc.transport);
        gc.write_purchases_edge(ADDR_A, POST_ID, EVENT_ID).await;
        let expected = format!(
            "USE {space};\n{upsert_usr}\n{upsert_pid}\nINSERT EDGE {e_purchases}({p_eid}, {p_purchased_at}, {p_weight}) VALUES \"{buyer_vid}\" -> \"{pid_vid}\":(\"{eid}\", now(), 2.0);",
            space = SPACE_THERAGRAPH,
            upsert_usr = ensure_user_vertex_nql(&vid_user(ADDR_A), ADDR_A),
            upsert_pid = ensure_post_vertex_nql(&vid_post(POST_ID), POST_ID),
            buyer_vid = vid_user(ADDR_A),
            pid_vid = vid_post(POST_ID),
            e_purchases = EDGE_PURCHASES,
            eid = EVENT_ID,
            p_eid = PROP_EVENT_ID,
            p_purchased_at = PROP_PURCHASED_AT,
            p_weight = PROP_WEIGHT,
        );
        assert_eq!(transport.last_query(), expected);
    }

    #[tokio::test]
    async fn golden_write_bookmark_edge() {
        let gc = GraphClient::with_transport(CapturingTransport::new());
        let transport = Arc::clone(&gc.transport);
        gc.write_bookmark_edge(ADDR_A, POST_ID, EVENT_ID).await;
        let expected = format!(
            "USE {space};\n{upsert_usr}\n{upsert_pid}\nINSERT EDGE IF NOT EXISTS {e_bm}({p_eid}, {p_bm_at}) VALUES \"{usr_vid}\" -> \"{pid_vid}\":(\"{eid}\", now());",
            space = SPACE_THERAGRAPH,
            upsert_usr = ensure_user_vertex_nql(&vid_user(ADDR_A), ADDR_A),
            upsert_pid = ensure_post_vertex_nql(&vid_post(POST_ID), POST_ID),
            usr_vid = vid_user(ADDR_A),
            pid_vid = vid_post(POST_ID),
            e_bm = EDGE_BOOKMARKED,
            p_eid = PROP_EVENT_ID,
            p_bm_at = PROP_BOOKMARKED_AT,
            eid = EVENT_ID,
        );
        assert_eq!(transport.last_query(), expected);
    }

    #[tokio::test]
    async fn golden_delete_bookmark_edge() {
        let gc = GraphClient::with_transport(CapturingTransport::new());
        let transport = Arc::clone(&gc.transport);
        gc.delete_bookmark_edge(ADDR_A, POST_ID).await;
        let expected = format!(
            "USE {space};\nDELETE EDGE {e_bm} \"{usr_vid}\" -> \"{pid_vid}\";",
            space = SPACE_THERAGRAPH,
            e_bm = EDGE_BOOKMARKED,
            usr_vid = vid_user(ADDR_A),
            pid_vid = vid_post(POST_ID),
        );
        assert_eq!(transport.last_query(), expected);
    }

    // RS-09: circuit opens after CIRCUIT_OPEN_THRESHOLD consecutive failures.
    #[tokio::test]
    async fn circuit_opens_after_threshold_failures() {
        let gc = GraphClient::with_transport(FailingTransport::always_fail());
        for _ in 0..CIRCUIT_OPEN_THRESHOLD {
            let _ = gc.execute_query("USE theragraph;").await;
        }
        assert!(
            gc.read_cb.circuit_open.load(Ordering::Relaxed),
            "circuit should be open after {CIRCUIT_OPEN_THRESHOLD} failures"
        );
    }

    // RS-09: circuit stays closed when failures are below threshold.
    #[tokio::test]
    async fn circuit_stays_closed_below_threshold() {
        let gc = GraphClient::with_transport(FailingTransport::fail_then_recover(
            CIRCUIT_OPEN_THRESHOLD - 1,
        ));
        for _ in 0..(CIRCUIT_OPEN_THRESHOLD - 1) {
            let _ = gc.execute_query("USE theragraph;").await;
        }
        assert!(
            !gc.read_cb.circuit_open.load(Ordering::Relaxed),
            "circuit should still be closed below threshold"
        );
    }

    // RS-09: consecutive_failures resets to 0 after a successful call.
    #[tokio::test]
    async fn success_resets_consecutive_failures() {
        // fail THRESHOLD-1 times, then succeed
        let gc = GraphClient::with_transport(FailingTransport::fail_then_recover(
            CIRCUIT_OPEN_THRESHOLD - 1,
        ));
        for _ in 0..(CIRCUIT_OPEN_THRESHOLD - 1) {
            let _ = gc.execute_query("USE theragraph;").await;
        }
        // should succeed now
        let result = gc.execute_query("USE theragraph;").await;
        assert!(result.is_ok(), "expected success after recovery");
        assert_eq!(
            gc.read_cb.consecutive_failures.load(Ordering::Relaxed),
            0,
            "consecutive_failures should reset to 0 on success"
        );
    }

    // RS-09: open circuit returns Err immediately without hitting the transport.
    #[tokio::test]
    async fn open_circuit_returns_err_without_transport_call() {
        let transport = FailingTransport::always_fail();
        let gc = GraphClient::with_transport(transport);

        // Force circuit open
        for _ in 0..CIRCUIT_OPEN_THRESHOLD {
            let _ = gc.execute_query("USE theragraph;").await;
        }
        assert!(gc.read_cb.circuit_open.load(Ordering::Relaxed));

        // Force last_opened_at to the recent past so the 30-second cooldown blocks
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        gc.read_cb.last_opened_at.store(now, Ordering::Relaxed);

        let call_count_before = gc.read_cb.consecutive_failures.load(Ordering::Relaxed);
        let result = gc.execute_query("USE theragraph;").await;
        // Should bail early with circuit-open error, not increment failure counter
        assert!(result.is_err());
        // consecutive_failures should NOT change — transport was not called
        assert_eq!(
            gc.read_cb.consecutive_failures.load(Ordering::Relaxed),
            call_count_before,
            "open circuit should skip transport; consecutive_failures must not increment"
        );
    }

    // RS-09: parse_nebula_table handles __NULL__ scores without dropping the row.
    #[test]
    fn parse_nebula_table_handles_null_scores() {
        let output = "\
+--------+---------+\n\
| post_id | score  |\n\
+--------+---------+\n\
| \"nft1\" | 1.5    |\n\
| \"nft2\" | __NULL__ |\n\
| \"nft3\" | 0.8    |\n\
+--------+---------+\n\
";
        let results = parse_nebula_table(output, 1, 2);
        // nft1 and nft3 have numeric scores; nft2 is NULL → score 0.0
        let nft2 = results.iter().find(|(k, _)| k == "nft2");
        assert!(nft2.is_some(), "nft2 (__NULL__ score) should be present");
        assert_eq!(nft2.unwrap().1, 0.0, "NULL score should map to 0.0");
        assert_eq!(results.len(), 3, "all three rows should be parsed");
    }
}

