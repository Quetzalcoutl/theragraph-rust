//! GraphSync — centralised Nebula graph write layer with bounded exponential-backoff retry.
//!
//! All event handlers that need to write to the Nebula social graph go through
//! this module instead of calling `GraphClient` write helpers directly.
//!
//! Benefits over the previous scattered approach:
//!   • Errors are returned to callers instead of being swallowed.
//!   • Transient connection/timeout failures are retried up to 3 times with
//!     exponential backoff (100 ms → 200 ms → 400 ms, capped at 4 s).
//!   • Logic errors (invalid input, not-found) are never retried.
//!   • Named operations (`sync_follow`, `sync_unfollow`, etc.) make the intent
//!     obvious in call-sites; raw nGQL strings are not visible outside this file.

use std::time::Duration;

use crate::recommendation::graph_client::{GraphClient, GraphTransport, NebulaConsoleTransport, map_reaction_weight, normalize_address};
use crate::recommendation::schema_consts::{
    SPACE_THERAGRAPH, EDGE_LIKES, EDGE_PURCHASES, EDGE_COMMENTS_ON, EDGE_FOLLOWS,
    EDGE_BOOKMARKED, EDGE_SHARED,
    PROP_EVENT_ID, PROP_LIKED_AT, PROP_PURCHASED_AT, PROP_REACTION_TYPE, PROP_WEIGHT,
    PROP_COMMENT_TEXT, PROP_COMMENTED_AT, PROP_FOLLOWED_AT, PROP_BOOKMARKED_AT, PROP_SHARED_AT,
    vid_user, vid_post, comment_rank,
    // B-02: consolidated validation fns (removed private duplicates below)
    is_safe_address, is_safe_id, is_safe_post_vid_id,
    // A-04: vertex upsert nGQL helpers (replaces 8 inline copies)
    ensure_user_vertex_nql, ensure_post_vertex_nql,
};
use thiserror::Error;
use tracing::{info, instrument, warn};

// ── Error type ────────────────────────────────────────────────────────────────

/// Errors that `GraphSync` can surface to callers.
///
/// Variants distinguish the two root causes so callers can decide whether to
/// retry, alert, or just log:
///  - `ConnectionError` wraps Nebula transport/circuit-breaker failures.
///  - `InvalidInput` means the supplied addresses/IDs failed the safety check
///    and the write was never attempted.
#[derive(Debug, Error)]
pub enum GraphSyncError {
    #[error("Nebula connection error during {operation}: {source}")]
    ConnectionError {
        operation: &'static str,
        #[source]
        source: anyhow::Error,
    },

    #[error("Invalid input for {operation}: {detail}")]
    InvalidInput {
        operation: &'static str,
        detail: String,
    },
}

impl GraphSyncError {
    /// Returns `true` for errors that are worth retrying.
    ///
    /// Connection refused, timeouts, and circuit-open transients are retryable.
    /// Logic errors (`InvalidInput`) and permanent failures are not.
    pub fn is_transient(&self) -> bool {
        match self {
            GraphSyncError::InvalidInput { .. } => false,
            GraphSyncError::ConnectionError { source, .. } => {
                let msg = source.to_string().to_lowercase();
                msg.contains("connection refused")
                    || msg.contains("timed out")
                    || msg.contains("timeout")
                    || msg.contains("reset by peer")
                    || msg.contains("broken pipe")
                    || msg.contains("circuit open")
                    || msg.contains("os error")
            }
        }
    }
}

// ── GraphSync ─────────────────────────────────────────────────────────────────

/// Centralised write layer over `GraphClient` with built-in retry.
///
/// Generic over `T: GraphTransport` so tests can inject a mock transport
/// without spawning a real nebula-console process.
///
/// Holds the client by value (which is `Clone`-cheap because it wraps `Arc`s
/// internally) and exposes named methods for every social-graph mutation the
/// event processor needs.
pub struct GraphSync<T: GraphTransport = NebulaConsoleTransport> {
    client: GraphClient<T>,
}

/// Manual `Clone` impl so that `GraphSync<T>` only requires `GraphClient<T>: Clone`,
/// not `T: Clone` directly. `GraphClient<T>` already implements `Clone` via its own
/// manual impl (it holds `Arc<T>`, so only `Arc` needs to be cloned, not `T`).
impl<T: GraphTransport> Clone for GraphSync<T> {
    fn clone(&self) -> Self {
        Self { client: self.client.clone() }
    }
}

impl<T: GraphTransport> GraphSync<T> {
    /// Create a `GraphSync` from any typed `GraphClient<T>`.
    ///
    /// Generic so tests can pass a mock-transport client and production code
    /// can pass either `GraphClient<NebulaConsoleTransport>` (subprocess) or
    /// `GraphClient<DynGraphTransport>` (trait-object pool adapter).
    pub fn new(client: GraphClient<T>) -> Self {
        Self { client }
    }

    /// Create a `GraphSync` from a client backed by a custom transport.
    ///
    /// Intended for tests: pass a `GraphClient::with_transport(mock)` here.
    #[allow(dead_code)]
    pub fn with_client(client: GraphClient<T>) -> Self {
        Self { client }
    }

    // ── Retry helper ──────────────────────────────────────────────────────────

    /// Execute `f` with bounded exponential backoff.
    ///
    /// * Up to 3 retries (4 attempts total).
    /// * Initial delay: 100 ms, doubles each attempt, capped at 4 s.
    /// * Only `GraphSyncError::is_transient()` errors are retried; logic errors
    ///   and the final attempt propagate immediately.
    async fn with_retry<F, Fut, V>(&self, op_name: &str, f: F) -> Result<V, GraphSyncError>
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = Result<V, GraphSyncError>>,
    {
        let mut delay = Duration::from_millis(100);
        for attempt in 0..=3u32 {
            match f().await {
                Ok(v) => return Ok(v),
                Err(e) if attempt < 3 && e.is_transient() => {
                    warn!(
                        op = op_name,
                        attempt,
                        "graph write retry after {:?}", delay
                    );
                    tokio::time::sleep(delay).await;
                    delay = std::cmp::min(delay * 2, Duration::from_secs(4));
                }
                Err(e) => return Err(e),
            }
        }
        // PANIC-006: return Err instead of unreachable!() so a loop-invariant
        // violation (unforeseen compiler change, code refactor, etc.) surfaces as
        // a GraphSyncError rather than an unrecoverable panic.
        // NOTE: op_name is &'static str at all call sites (every caller passes a
        // `const OP: &str = "..."` literal), but the generic signature uses `&str`
        // with an implicit lifetime.  We use a fixed sentinel here so this cold
        // error path compiles without tying the Err to the borrowed lifetime.
        Err(GraphSyncError::ConnectionError {
            operation: "with_retry",
            source: anyhow::anyhow!(
                "with_retry: loop exited without returning (op={op_name}) — this is a bug"
            ),
        })
    }

    // ── Public operations ─────────────────────────────────────────────────────

    /// Upsert user vertices and insert a `follows` edge.
    ///
    /// Transient Nebula failures are retried up to 3 times with exponential
    /// backoff before the error is surfaced to the caller.
    #[instrument(skip(self), fields(op = "sync_follow", follower = %follower, target = %target))]
    pub async fn sync_follow(
        &self,
        follower: &str,
        target: &str,
        tx_hash: &str,
    ) -> Result<(), GraphSyncError> {
        const OP: &str = "sync_follow";

        if !is_safe_address(follower) || !is_safe_address(target) {
            return Err(GraphSyncError::InvalidInput {
                operation: OP,
                detail: format!("follower={follower} target={target}"),
            });
        }
        if !is_safe_id(tx_hash) {
            return Err(GraphSyncError::InvalidInput {
                operation: OP,
                detail: format!("tx_hash={tx_hash}"),
            });
        }

        // NEBULA-003: call inner_sync_follow via with_retry so transient Nebula
        // failures are retried and the error propagates to social.rs, which in turn
        // lets the Kafka consumer skip the offset commit and retry the whole event.
        // Cache invalidation is handled by social.rs (delete_following etc.) so
        // inner_sync_follow skips the duplicate cache invalidation path in
        // write_follows_edge.
        self.with_retry(OP, || self.inner_sync_follow(follower, target, tx_hash))
            .await
    }

    async fn inner_sync_follow(
        &self,
        follower: &str,
        target: &str,
        tx_hash: &str,
    ) -> Result<(), GraphSyncError> {
        const OP: &str = "sync_follow";

        let follower = normalize_address(follower);
        let target = normalize_address(target);
        let fwr_vid = vid_user(&follower);
        let fwe_vid = vid_user(&target);
        let query = format!(
            "USE {space};\n{upsert_fwr}\n{upsert_fwe}\nINSERT EDGE IF NOT EXISTS {e_follows}({p_eid}, {p_followed_at}, {p_weight}) VALUES \"{fwr_vid}\" -> \"{fwe_vid}\":(\"{eid}\", now(), 1.0);",
            space = SPACE_THERAGRAPH,
            upsert_fwr = ensure_user_vertex_nql(&fwr_vid, &follower),
            upsert_fwe = ensure_user_vertex_nql(&fwe_vid, &target),
            e_follows = EDGE_FOLLOWS,
            p_eid = PROP_EVENT_ID,
            p_followed_at = PROP_FOLLOWED_AT,
            p_weight = PROP_WEIGHT,
            fwr_vid = fwr_vid,
            fwe_vid = fwe_vid,
            eid = tx_hash,
        );

        self.client
            .execute_write(&query)
            .await
            .map(|_| {
                info!("GraphSync: follows edge written {} -> {}", follower, target);
            })
            .map_err(|e| {
                warn!(
                    "GraphSync: sync_follow failed {} -> {}: {e}",
                    follower, target
                );
                GraphSyncError::ConnectionError { operation: OP, source: e }
            })
    }

    /// Delete the `follows` edge on unfollow.
    ///
    /// Transient Nebula failures are retried up to 3 times with exponential backoff.
    #[instrument(skip(self), fields(op = "sync_unfollow", follower = %follower, target = %target))]
    pub async fn sync_unfollow(
        &self,
        follower: &str,
        target: &str,
    ) -> Result<(), GraphSyncError> {
        const OP: &str = "sync_unfollow";

        if !is_safe_address(follower) || !is_safe_address(target) {
            return Err(GraphSyncError::InvalidInput {
                operation: OP,
                detail: format!("follower={follower} target={target}"),
            });
        }

        self.with_retry(OP, || self.inner_sync_unfollow(follower, target)).await
    }

    async fn inner_sync_unfollow(
        &self,
        follower: &str,
        target: &str,
    ) -> Result<(), GraphSyncError> {
        const OP: &str = "sync_unfollow";
        let follower = normalize_address(follower);
        let target   = normalize_address(target);
        let fwr_vid  = vid_user(&follower);
        let fwe_vid  = vid_user(&target);
        let query = format!(
            "USE {space};\nDELETE EDGE {e} \"{src}\" -> \"{dst}\";",
            space = SPACE_THERAGRAPH,
            e     = EDGE_FOLLOWS,
            src   = fwr_vid,
            dst   = fwe_vid,
        );
        self.client
            .execute_write(&query)
            .await
            .map(|_| ())
            .map_err(|e| GraphSyncError::ConnectionError { operation: OP, source: e })
    }

    /// Upsert user + post vertices and insert a `likes` edge.
    ///
    /// `contract` and `token_id` together identify the post vertex
    /// (`post:{token_id}` in the VID space).
    ///
    /// Transient Nebula failures are retried up to 3 times with exponential backoff.
    #[instrument(skip(self), fields(op = "sync_like", user = %user, token_id = %token_id))]
    pub async fn sync_like(
        &self,
        contract: &str,
        token_id: &str,
        user: &str,
        tx_hash: &str,
    ) -> Result<(), GraphSyncError> {
        const OP: &str = "sync_like";

        if !is_safe_address(user) {
            return Err(GraphSyncError::InvalidInput {
                operation: OP,
                detail: format!("user={user}"),
            });
        }
        if !is_safe_id(tx_hash) {
            return Err(GraphSyncError::InvalidInput {
                operation: OP,
                detail: format!("tx_hash={tx_hash}"),
            });
        }
        if !is_safe_post_vid_id(token_id) {
            return Err(GraphSyncError::InvalidInput {
                operation: OP,
                detail: format!("contract={contract} token_id={token_id}"),
            });
        }

        self.with_retry(OP, || self.inner_sync_like(contract, token_id, user, tx_hash))
            .await
    }

    async fn inner_sync_like(
        &self,
        _contract: &str,
        token_id: &str,
        user: &str,
        tx_hash: &str,
    ) -> Result<(), GraphSyncError> {
        const OP: &str = "sync_like";

        // VID-CASE-001: normalise to lowercase.
        let user = normalize_address(user);
        let user_vid = vid_user(&user);
        let post_vid = vid_post(token_id);
        // Default reaction_type to "like" — sync_like callers do not currently
        // supply a reaction type. Using map_reaction_weight keeps the weight
        // consistent with the API-side write_likes_edge path.
        let reaction_type = "like";
        let weight = map_reaction_weight(reaction_type);
        // UPSERT-HOT-002 / A-04: vertex upserts via ensure_*_vertex_nql helper.
        let query = format!(
            "USE {space};\n{upsert_usr}\n{upsert_post}\nINSERT EDGE IF NOT EXISTS {e_likes}({p_eid}, {p_liked_at}, {p_rt}, {p_weight}) VALUES \"{user_vid}\" -> \"{post_vid}\":(\"{txh}\", now(), \"{rt}\", {wt});",
            space = SPACE_THERAGRAPH,
            upsert_usr = ensure_user_vertex_nql(&user_vid, &user),
            upsert_post = ensure_post_vertex_nql(&post_vid, token_id),
            e_likes = EDGE_LIKES,
            p_eid = PROP_EVENT_ID,
            p_liked_at = PROP_LIKED_AT,
            p_rt = PROP_REACTION_TYPE,
            p_weight = PROP_WEIGHT,
            user_vid = user_vid,
            post_vid = post_vid,
            txh = tx_hash,
            rt = reaction_type,
            wt = weight,
        );

        self.client
            .execute_write(&query)
            .await
            .map(|_| {
                info!("GraphSync: like edge written user={} post={}", user, token_id);
            })
            .map_err(|e| {
                warn!(
                    "GraphSync: sync_like failed user={} post={}: {e}",
                    user, token_id
                );
                GraphSyncError::ConnectionError { operation: OP, source: e }
            })
    }

    /// Insert a `comments_on` edge.
    ///
    /// Uses epoch-milliseconds as the edge rank so each comment from the same
    /// (user, post) pair gets a unique identity — avoids the single-edge
    /// overwrite bug that `write_comments_on` in `graph_client.rs` addresses.
    ///
    /// `comment_preview` is filtered to alphanumeric + space and capped at 120
    /// characters before being stored. Pass an empty string when the Kafka
    /// event payload does not include comment text.
    ///
    /// Transient Nebula failures are retried up to 3 times with exponential backoff.
    #[instrument(skip(self), fields(op = "sync_comment", user = %user, token_id = %token_id))]
    pub async fn sync_comment(
        &self,
        token_id: &str,
        user: &str,
        event_id: &str,
        comment_preview: &str,
    ) -> Result<(), GraphSyncError> {
        const OP: &str = "sync_comment";

        if !is_safe_address(user) {
            return Err(GraphSyncError::InvalidInput {
                operation: OP,
                detail: format!("user={user}"),
            });
        }
        if !is_safe_post_vid_id(token_id) {
            return Err(GraphSyncError::InvalidInput {
                operation: OP,
                detail: format!("token_id={token_id}"),
            });
        }
        if !is_safe_id(event_id) {
            return Err(GraphSyncError::InvalidInput {
                operation: OP,
                detail: format!("event_id={event_id}"),
            });
        }

        self.with_retry(OP, || {
            self.inner_sync_comment(token_id, user, event_id, comment_preview)
        })
        .await
    }

    async fn inner_sync_comment(
        &self,
        token_id: &str,
        user: &str,
        event_id: &str,
        comment_preview: &str,
    ) -> Result<(), GraphSyncError> {
        const OP: &str = "sync_comment";

        // VID-CASE-001: normalise to lowercase.
        let user = normalize_address(user);
        let user = user.as_str();
        // Sanitise preview: keep alphanumeric + space, cap at 120 chars.
        let safe_preview: String = comment_preview
            .chars()
            .filter(|c| c.is_alphanumeric() || *c == ' ')
            .take(120)
            .collect();

        // POOL-003 / TAG-S29-06: shared with graph_client.rs's write_comments_on
        // via schema_consts::comment_rank — see its doc comment for the
        // dash-stripping / 15-digit-cap rationale.
        let rank = comment_rank(event_id);

        let user_vid = vid_user(user);
        let post_vid = vid_post(token_id);
        // UPSERT-HOT-002 / A-04: vertex upserts via ensure_*_vertex_nql helper.
        let query = format!(
            "USE {space};\n{upsert_usr}\n{upsert_post}\nINSERT EDGE IF NOT EXISTS {e_comments}({p_eid}, {p_comment_text}, {p_commented_at}) VALUES \"{user_vid}\" -> \"{post_vid}\"@{rank}:(\"{eid}\", \"{preview}\", now());",
            space = SPACE_THERAGRAPH,
            upsert_usr = ensure_user_vertex_nql(&user_vid, user),
            upsert_post = ensure_post_vertex_nql(&post_vid, token_id),
            e_comments = EDGE_COMMENTS_ON,
            p_eid = PROP_EVENT_ID,
            p_comment_text = PROP_COMMENT_TEXT,
            p_commented_at = PROP_COMMENTED_AT,
            user_vid = user_vid,
            post_vid = post_vid,
            rank = rank,
            eid = event_id,
            preview = safe_preview,
        );

        self.client
            .execute_write(&query)
            .await
            .map(|_| {
                info!("GraphSync: comment edge written user={} post={}", user, token_id);
            })
            .map_err(|e| {
                warn!(
                    "GraphSync: sync_comment failed user={} post={}: {e}",
                    user, token_id
                );
                GraphSyncError::ConnectionError { operation: OP, source: e }
            })
    }

    /// Delete the `likes` edge on unlike.
    ///
    /// Transient Nebula failures are retried up to 3 times with exponential backoff.
    #[instrument(skip(self), fields(op = "sync_unlike", user = %user, token_id = %token_id))]
    pub async fn sync_unlike(
        &self,
        contract: &str,
        token_id: &str,
        user: &str,
    ) -> Result<(), GraphSyncError> {
        const OP: &str = "sync_unlike";

        if !is_safe_address(user) {
            return Err(GraphSyncError::InvalidInput {
                operation: OP,
                detail: format!("user={user}"),
            });
        }
        if !is_safe_post_vid_id(token_id) {
            return Err(GraphSyncError::InvalidInput {
                operation: OP,
                detail: format!("contract={contract} token_id={token_id}"),
            });
        }

        self.with_retry(OP, || self.inner_sync_unlike(contract, token_id, user))
            .await
    }

    async fn inner_sync_unlike(
        &self,
        _contract: &str,
        token_id: &str,
        user: &str,
    ) -> Result<(), GraphSyncError> {
        const OP: &str = "sync_unlike";

        // VID-CASE-001: normalise to lowercase.
        let user = normalize_address(user);
        let user_vid = vid_user(&user);
        let post_vid = vid_post(token_id);
        let query = format!(
            "USE {space};\nDELETE EDGE {e_likes} \"{user_vid}\" -> \"{post_vid}\";",
            space = SPACE_THERAGRAPH,
            e_likes = EDGE_LIKES,
            user_vid = user_vid,
            post_vid = post_vid,
        );

        self.client
            .execute_write(&query)
            .await
            .map(|_| {
                info!("GraphSync: unlike edge deleted user={} post={}", user, token_id);
            })
            .map_err(|e| {
                warn!(
                    "GraphSync: sync_unlike failed user={} post={}: {e}",
                    user, token_id
                );
                GraphSyncError::ConnectionError { operation: OP, source: e }
            })
    }

    /// Write a purchase (copy) edge to the graph.
    ///
    /// MIGRATION 15: writes to BOTH `likes` (reaction_type="purchase", for general
    /// FoF and backward compatibility) AND the dedicated `purchases` edge type (for
    /// storaged-level selectivity in get_purchase_fof_recommendations).
    ///
    /// TRANSITION: Once a backfill of historical purchases is complete and the
    /// `likes WHERE reaction_type="purchase"` rows are removed, the `likes` write
    /// here can be dropped. See theragraph-nebula/init/15-add-purchases-edge.ngql.
    ///
    /// Transient Nebula failures are retried up to 3 times with exponential backoff.
    #[instrument(skip(self), fields(op = "sync_purchase", buyer = %buyer, token_id = %token_id))]
    pub async fn sync_purchase(
        &self,
        contract: &str,
        token_id: &str,
        buyer: &str,
        tx_hash: &str,
    ) -> Result<(), GraphSyncError> {
        const OP: &str = "sync_purchase";

        if !is_safe_address(buyer) {
            return Err(GraphSyncError::InvalidInput {
                operation: OP,
                detail: format!("buyer={buyer}"),
            });
        }
        if !is_safe_id(tx_hash) {
            return Err(GraphSyncError::InvalidInput {
                operation: OP,
                detail: format!("tx_hash={tx_hash}"),
            });
        }
        if !is_safe_post_vid_id(token_id) {
            return Err(GraphSyncError::InvalidInput {
                operation: OP,
                detail: format!("contract={contract} token_id={token_id}"),
            });
        }

        self.with_retry(OP, || {
            self.inner_sync_purchase(token_id, buyer, tx_hash)
        })
        .await
    }

    async fn inner_sync_purchase(
        &self,
        token_id: &str,
        buyer: &str,
        tx_hash: &str,
    ) -> Result<(), GraphSyncError> {
        const OP: &str = "sync_purchase";
        // VID-CASE-001: normalise to lowercase.
        let buyer = normalize_address(buyer);
        let buyer = buyer.as_str();
        let buyer_vid = vid_user(buyer);
        let post_vid = vid_post(token_id);
        // Reaction type "purchase" maps to weight 2.0 in map_reaction_weight,
        // giving strong positive signal for FoF scoring.
        let reaction_type = "purchase";
        let weight = map_reaction_weight(reaction_type);
        // UPSERT-HOT-002 / A-04: vertex upserts via ensure_*_vertex_nql helper.
        // Writes both edges in one round-trip. The `likes` write maintains backward
        // compatibility for general FoF; the `purchases` write enables storaged-level
        // selectivity in get_purchase_fof_recommendations (migration 15).
        let query = format!(
            "USE {space};\n{upsert_usr}\n{upsert_post}\nINSERT EDGE IF NOT EXISTS {e_likes}({p_eid}, {p_liked_at}, {p_rt}, {p_weight}) VALUES \"{buyer_vid}\" -> \"{post_vid}\":(\"{txh}\", now(), \"{rt}\", {wt});\nINSERT EDGE IF NOT EXISTS {e_purchases}({p_eid}, {p_purchased_at}, {p_weight}) VALUES \"{buyer_vid}\" -> \"{post_vid}\":(\"{txh}\", now(), {wt});",
            space = SPACE_THERAGRAPH,
            upsert_usr = ensure_user_vertex_nql(&buyer_vid, buyer),
            upsert_post = ensure_post_vertex_nql(&post_vid, token_id),
            e_likes = EDGE_LIKES,
            e_purchases = EDGE_PURCHASES,
            p_eid = PROP_EVENT_ID,
            p_liked_at = PROP_LIKED_AT,
            p_purchased_at = PROP_PURCHASED_AT,
            p_rt = PROP_REACTION_TYPE,
            p_weight = PROP_WEIGHT,
            buyer_vid = buyer_vid,
            post_vid = post_vid,
            txh = tx_hash,
            rt = reaction_type,
            wt = weight,
        );

        self.client
            .execute_write(&query)
            .await
            .map(|_| {
                info!(
                    "GraphSync: purchase edge written buyer={} post={}",
                    buyer, token_id
                );
            })
            .map_err(|e| {
                warn!(
                    "GraphSync: sync_purchase failed buyer={} post={}: {e}",
                    buyer, token_id
                );
                GraphSyncError::ConnectionError { operation: OP, source: e }
            })
    }

    // ── TAG-S29-04: Bookmark / Share edges ────────────────────────────────────

    /// Insert a `bookmarked` edge from user → post.
    ///
    /// Uses `INSERT EDGE IF NOT EXISTS` — bookmarks are a toggle: re-bookmarking
    /// the same NFT should be a no-op (not overwrite `bookmarked_at`).
    /// The matching unbookmark path is `sync_unbookmark`.
    #[instrument(skip(self), fields(op = "sync_bookmark", user = %user, token_id = %token_id))]
    pub async fn sync_bookmark(
        &self,
        token_id: &str,
        user: &str,
        tx_hash: &str,
    ) -> Result<(), GraphSyncError> {
        const OP: &str = "sync_bookmark";

        if !is_safe_address(user) {
            return Err(GraphSyncError::InvalidInput {
                operation: OP,
                detail: format!("user={user}"),
            });
        }
        if !is_safe_post_vid_id(token_id) {
            return Err(GraphSyncError::InvalidInput {
                operation: OP,
                detail: format!("token_id={token_id}"),
            });
        }
        if !is_safe_id(tx_hash) {
            return Err(GraphSyncError::InvalidInput {
                operation: OP,
                detail: format!("tx_hash={tx_hash}"),
            });
        }

        self.with_retry(OP, || self.inner_sync_bookmark(token_id, user, tx_hash))
            .await
    }

    async fn inner_sync_bookmark(
        &self,
        token_id: &str,
        user: &str,
        tx_hash: &str,
    ) -> Result<(), GraphSyncError> {
        const OP: &str = "sync_bookmark";
        let user = normalize_address(user);
        let user_vid = vid_user(&user);
        let post_vid = vid_post(token_id);
        let query = format!(
            "USE {space};\n{upsert_usr}\n{upsert_post}\nINSERT EDGE IF NOT EXISTS {e_bookmarked}({p_eid}, {p_bookmarked_at}) VALUES \"{user_vid}\" -> \"{post_vid}\":(\"{txh}\", now());",
            space = SPACE_THERAGRAPH,
            upsert_usr = ensure_user_vertex_nql(&user_vid, &user),
            upsert_post = ensure_post_vertex_nql(&post_vid, token_id),
            e_bookmarked = EDGE_BOOKMARKED,
            p_eid = PROP_EVENT_ID,
            p_bookmarked_at = PROP_BOOKMARKED_AT,
            user_vid = user_vid,
            post_vid = post_vid,
            txh = tx_hash,
        );

        self.client
            .execute_write(&query)
            .await
            .map(|_| {
                info!("GraphSync: bookmark edge written user={} post={}", user, token_id);
            })
            .map_err(|e| {
                warn!("GraphSync: sync_bookmark failed user={} post={}: {e}", user, token_id);
                GraphSyncError::ConnectionError { operation: OP, source: e }
            })
    }

    /// Delete the `bookmarked` edge on unbookmark.
    #[instrument(skip(self), fields(op = "sync_unbookmark", user = %user, token_id = %token_id))]
    pub async fn sync_unbookmark(
        &self,
        token_id: &str,
        user: &str,
    ) -> Result<(), GraphSyncError> {
        const OP: &str = "sync_unbookmark";

        if !is_safe_address(user) {
            return Err(GraphSyncError::InvalidInput {
                operation: OP,
                detail: format!("user={user}"),
            });
        }
        if !is_safe_post_vid_id(token_id) {
            return Err(GraphSyncError::InvalidInput {
                operation: OP,
                detail: format!("token_id={token_id}"),
            });
        }

        self.with_retry(OP, || self.inner_sync_unbookmark(token_id, user))
            .await
    }

    async fn inner_sync_unbookmark(
        &self,
        token_id: &str,
        user: &str,
    ) -> Result<(), GraphSyncError> {
        const OP: &str = "sync_unbookmark";
        let user = normalize_address(user);
        let user_vid = vid_user(&user);
        let post_vid = vid_post(token_id);
        let query = format!(
            "USE {space};\nDELETE EDGE {e} \"{src}\" -> \"{dst}\";",
            space = SPACE_THERAGRAPH,
            e = EDGE_BOOKMARKED,
            src = user_vid,
            dst = post_vid,
        );
        self.client
            .execute_write(&query)
            .await
            .map(|_| {
                info!("GraphSync: bookmark edge deleted user={} post={}", user, token_id);
            })
            .map_err(|e| GraphSyncError::ConnectionError { operation: OP, source: e })
    }

    /// Insert a `shared` edge from user → post.
    ///
    /// Shares are one of the two highest-intent signals (alongside bookmarks); uses
    /// `INSERT EDGE IF NOT EXISTS` so reconciler replays do not overwrite `shared_at`.
    /// Weight 1.5 — stronger signal than a like (1.0), weaker than a purchase (2.0).
    #[instrument(skip(self), fields(op = "sync_share", user = %user, token_id = %token_id))]
    pub async fn sync_share(
        &self,
        token_id: &str,
        user: &str,
        tx_hash: &str,
    ) -> Result<(), GraphSyncError> {
        const OP: &str = "sync_share";

        if !is_safe_address(user) {
            return Err(GraphSyncError::InvalidInput {
                operation: OP,
                detail: format!("user={user}"),
            });
        }
        if !is_safe_post_vid_id(token_id) {
            return Err(GraphSyncError::InvalidInput {
                operation: OP,
                detail: format!("token_id={token_id}"),
            });
        }
        if !is_safe_id(tx_hash) {
            return Err(GraphSyncError::InvalidInput {
                operation: OP,
                detail: format!("tx_hash={tx_hash}"),
            });
        }

        self.with_retry(OP, || self.inner_sync_share(token_id, user, tx_hash))
            .await
    }

    async fn inner_sync_share(
        &self,
        token_id: &str,
        user: &str,
        tx_hash: &str,
    ) -> Result<(), GraphSyncError> {
        const OP: &str = "sync_share";
        let user = normalize_address(user);
        let user_vid = vid_user(&user);
        let post_vid = vid_post(token_id);
        let weight: f64 = 1.5;
        let query = format!(
            "USE {space};\n{upsert_usr}\n{upsert_post}\nINSERT EDGE IF NOT EXISTS {e_shared}({p_eid}, {p_shared_at}, {p_weight}) VALUES \"{user_vid}\" -> \"{post_vid}\":(\"{txh}\", now(), {wt});",
            space = SPACE_THERAGRAPH,
            upsert_usr = ensure_user_vertex_nql(&user_vid, &user),
            upsert_post = ensure_post_vertex_nql(&post_vid, token_id),
            e_shared = EDGE_SHARED,
            p_eid = PROP_EVENT_ID,
            p_shared_at = PROP_SHARED_AT,
            p_weight = PROP_WEIGHT,
            user_vid = user_vid,
            post_vid = post_vid,
            txh = tx_hash,
            wt = weight,
        );

        self.client
            .execute_write(&query)
            .await
            .map(|_| {
                info!("GraphSync: share edge written user={} post={}", user, token_id);
            })
            .map_err(|e| {
                warn!("GraphSync: sync_share failed user={} post={}: {e}", user, token_id);
                GraphSyncError::ConnectionError { operation: OP, source: e }
            })
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result as AnyhowResult;
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};
    use std::sync::atomic::{AtomicUsize, Ordering};

    // ── Shared-state mock transport ───────────────────────────────────────────

    /// Pre-queued responses with an externally observable call counter.
    ///
    /// `Arc` internals allow the test to inspect counts after `GraphSync` takes
    /// ownership of the transport.
    #[derive(Clone)]
    struct MockTransport {
        responses: Arc<Mutex<VecDeque<AnyhowResult<String>>>>,
        call_count: Arc<AtomicUsize>,
    }

    impl MockTransport {
        /// Queue `ok_count` successes.
        fn always_ok(ok_count: usize) -> Self {
            Self {
                responses: Arc::new(Mutex::new(
                    (0..ok_count).map(|_| Ok(String::new())).collect(),
                )),
                call_count: Arc::new(AtomicUsize::new(0)),
            }
        }

        /// Queue `fail_count` "connection refused" errors followed by one success.
        fn fail_then_ok(fail_count: usize) -> Self {
            let mut q: VecDeque<AnyhowResult<String>> = (0..fail_count)
                .map(|_| Err(anyhow::anyhow!("connection refused (mock)")))
                .collect();
            q.push_back(Ok(String::new()));
            Self {
                responses: Arc::new(Mutex::new(q)),
                call_count: Arc::new(AtomicUsize::new(0)),
            }
        }

        fn call_count(&self) -> usize {
            self.call_count.load(Ordering::Relaxed)
        }
    }

    impl GraphTransport for MockTransport {
        // RS-11: updated to match the simplified trait signature (no explicit lifetimes).
        fn execute(&self, _query: &str) -> impl std::future::Future<Output = AnyhowResult<String>> + Send {
            self.call_count.fetch_add(1, Ordering::Relaxed);
            let response = self
                .responses
                .lock()
                .unwrap()
                .pop_front()
                .expect("MockTransport: no more queued responses — add more to the queue");
            async move { response }
        }
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

    const FOLLOWER: &str = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const TARGET:   &str = "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const TX:       &str = "abc123tx";

    fn make_sync(transport: MockTransport) -> GraphSync<MockTransport> {
        GraphSync::with_client(GraphClient::with_transport(transport))
    }

    // ── Test 1: succeeds on first try — single transport call ─────────────────

    /// Verifies that a successful write goes through with exactly one transport call.
    #[tokio::test(flavor = "current_thread")]
    async fn sync_follow_succeeds_on_first_try() {
        let transport = MockTransport::always_ok(1);
        // Keep a clone of the Arc-shared call counter before moving transport.
        let call_count = transport.call_count.clone();

        let sync = make_sync(transport);
        let result = sync.sync_follow(FOLLOWER, TARGET, TX).await;

        assert!(result.is_ok(), "expected Ok on first try, got: {:?}", result);
        assert_eq!(
            call_count.load(Ordering::Relaxed),
            1,
            "expected exactly 1 transport call on first-try success"
        );
    }

    // ── Test 2: follows are retried — inner_sync_follow propagates Nebula errors ─

    /// NEBULA-003: sync_follow now routes through inner_sync_follow (uses execute_write
    /// directly via with_retry) instead of calling write_follows_edge (best-effort,
    /// swallows errors). This ensures:
    ///   - Transient Nebula failures are retried up to 3 times.
    ///   - Permanent failures propagate to social.rs → Kafka skips the offset commit
    ///     → the event is retried until Nebula is reachable.
    ///
    /// Cache invalidation after a successful follow is handled by social.rs (lines
    /// cache.delete_following / delete_user_prefs / etc.) so inner_sync_follow's
    /// execute_write-only path remains correct.
    #[tokio::test(flavor = "current_thread")]
    async fn sync_follow_retries_transient_errors() {
        // Two failures then success — with_retry fires 3 total transport calls.
        let transport = MockTransport::fail_then_ok(2);
        let call_count = transport.call_count.clone();

        let sync = make_sync(transport);
        let result = sync.sync_follow(FOLLOWER, TARGET, TX).await;

        // After 2 retries the third attempt succeeds — result is Ok.
        assert!(result.is_ok(), "expected Ok after retries succeeded: {:?}", result);
        // with_retry: 1 initial attempt + 2 retries = 3 total transport calls.
        assert_eq!(
            call_count.load(Ordering::Relaxed),
            3,
            "sync_follow should retry transient failures (expected 3 transport calls)"
        );
    }

    // ── Test 3: invalid input is rejected immediately, no transport calls ─────

    #[tokio::test(flavor = "current_thread")]
    async fn sync_follow_invalid_input_not_retried() {
        // Provide more responses than should ever be consumed — a retry would
        // exhaust the queue and panic, surfacing the bug in this test.
        let transport = MockTransport::always_ok(10);
        let call_count = transport.call_count.clone();

        let sync = make_sync(transport);
        let result = sync.sync_follow("not-an-address", TARGET, TX).await;

        assert!(
            matches!(result, Err(GraphSyncError::InvalidInput { .. })),
            "expected InvalidInput error, got: {:?}",
            result
        );
        assert_eq!(
            call_count.load(Ordering::Relaxed),
            0,
            "transport must not be called for InvalidInput"
        );
    }

    // ── Test 4: is_transient classification ──────────────────────────────────

    #[test]
    fn connection_refused_is_transient() {
        let e = GraphSyncError::ConnectionError {
            operation: "test",
            source: anyhow::anyhow!("connection refused"),
        };
        assert!(e.is_transient());
    }

    #[test]
    fn timeout_is_transient() {
        let e = GraphSyncError::ConnectionError {
            operation: "test",
            source: anyhow::anyhow!("nebula-console query timed out (30s)"),
        };
        assert!(e.is_transient());
    }

    #[test]
    fn circuit_open_is_transient() {
        let e = GraphSyncError::ConnectionError {
            operation: "test",
            source: anyhow::anyhow!("Nebula circuit open — consecutive failures exceeded threshold"),
        };
        assert!(e.is_transient());
    }

    #[test]
    fn invalid_input_is_not_transient() {
        let e = GraphSyncError::InvalidInput {
            operation: "test",
            detail: "bad address".to_string(),
        };
        assert!(!e.is_transient());
    }
}
