//! Nebula edge reconciliation — replays Postgres interactions as Nebula graph edges.
//!
//! Best-effort Nebula writes in interaction.rs log a warn! and continue on failure.
//! This reconciler runs periodically, queries Postgres for all interactions in a
//! lookback window, and re-fires sync_* for each — closing the gap where a Nebula
//! outage leaves the social graph incomplete.
//!
//! All three sync_* calls are idempotent:
//!   - sync_like:     INSERT EDGE rank 0 — duplicate upserts the edge in-place.
//!   - sync_purchase: INSERT EDGE IF NOT EXISTS — duplicate is a no-op.
//!   - sync_comment:  rank derived from tx_hash — same tx → same rank → no-op.

use chrono::{Duration as ChronoDuration, Utc};
use futures::StreamExt as _;
use sqlx::PgPool;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tracing::{info, instrument, warn};
use uuid::Uuid;

use super::graph_sync::{GraphSync, GraphSyncError};
use crate::recommendation::graph_client::GraphTransport;

// ── A-05: generic section runner ─────────────────────────────────────────────
// All 5 reconcile sections share: fetch rows → build futures → fan-out
// concurrently → tally failures. This function owns the fan-out + tally so
// each section only needs to build the future vec.

type RecFuture<'a> = Pin<Box<dyn Future<Output = Result<(), GraphSyncError>> + Send + 'a>>;

async fn run_section(label: &'static str, futures: Vec<RecFuture<'_>>) -> u64 {
    let failed = Arc::new(AtomicU64::new(0));
    futures::stream::iter(futures)
        .for_each_concurrent(RECONCILE_CONCURRENCY, |f| {
            let failed = Arc::clone(&failed);
            async move {
                if let Err(e) = f.await {
                    failed.fetch_add(1, Ordering::Relaxed);
                    warn!("reconcile {label}: failed: {e}");
                }
            }
        })
        .await;
    failed.load(Ordering::Relaxed)
}

/// Max concurrent Nebula subprocess writes per reconciliation pass.
/// NEBULA_SEMAPHORE(8) limits total concurrent writes across the process; 16
/// queued items here gives enough parallelism without overwhelming the semaphore.
const RECONCILE_CONCURRENCY: usize = 16;

/// TAG-S29-02: bound each fetch to prevent OOM on large lookback windows.
/// 10 000 rows × ~200 bytes/row ≈ 2 MB per section — safe to hold in memory.
/// The reconciler runs every 6 h; rows beyond this cap are picked up on the
/// next pass since the lookback window still covers them.
const RECONCILE_PAGE_SIZE: i64 = 10_000;

/// Counts from one reconciliation pass.
pub struct ReconcileStats {
    pub likes_attempted: u64,
    pub purchases_attempted: u64,
    pub comments_attempted: u64,
    /// TAG-S29-03: follow reconciliation was missing; added here.
    pub follows_attempted: u64,
    /// S30-04: bookmark reconciliation added; no `shares` table exists so only bookmarks.
    pub bookmarks_attempted: u64,
    pub total_failed: u64,
}

#[derive(sqlx::FromRow)]
struct LikeRow {
    liker_address: String,
    tx_hash: String,
    nft_uuid: Uuid,
    contract_address: String,
}

#[derive(sqlx::FromRow)]
struct PurchaseRow {
    buyer_address: String,
    tx_hash: String,
    nft_uuid: Uuid,
    contract_address: String,
}

/// S30-04: bookmark rows from the Elixir Postgres DB.
/// Note: no `shares` table exists — only bookmarks are reconcilable.
#[derive(sqlx::FromRow)]
struct BookmarkRow {
    user_address: String,
    tx_hash: Option<String>,
    nft_uuid: Uuid,
}

/// TAG-S29-03: follow rows from the Elixir Postgres DB.
/// follower_id and followee_id join to social_users.address.
#[derive(sqlx::FromRow)]
struct FollowRow {
    follower_address: String,
    followee_address: String,
    tx_hash: Option<String>,
}

#[derive(sqlx::FromRow)]
struct CommentRow {
    commenter_address: String,
    tx_hash: String,
    comment_text: String,
    nft_uuid: Uuid,
}

/// Re-write Nebula edges for all Postgres interactions in the lookback window.
///
/// Queries the Elixir Postgres DB so it sees the same `likes`, `purchases`, and
/// `comments` rows that the Kafka consumer wrote. Safe to call at any time —
/// only interactions in the window are touched, and all sync_* calls are idempotent.
///
/// Call from a periodic timer (recommended: every 6 hours with lookback_hours = 48).
#[instrument(skip(elixir_pool, graph_sync), fields(lookback_hours))]
pub async fn reconcile_nebula_edges<T: GraphTransport>(
    elixir_pool: &PgPool,
    graph_sync: &GraphSync<T>,
    lookback_hours: u64,
) -> anyhow::Result<ReconcileStats> {
    let cutoff = (Utc::now() - ChronoDuration::hours(lookback_hours as i64)).naive_utc();
    let mut stats = ReconcileStats {
        likes_attempted: 0,
        purchases_attempted: 0,
        comments_attempted: 0,
        follows_attempted: 0,
        bookmarks_attempted: 0,
        total_failed: 0,
    };

    // ── Likes ─────────────────────────────────────────────────────────────────
    // TAG-S29-02: ORDER BY + LIMIT bounds memory. Most-recent rows first so that
    // a high-volume window always reconciles the newest interactions first.
    let likes: Vec<LikeRow> = sqlx::query_as(
        "SELECT l.liker_address, l.tx_hash, n.id AS nft_uuid, n.contract_address \
         FROM likes l \
         JOIN nfts n ON n.id = l.nft_id \
         WHERE l.inserted_at > $1 \
         ORDER BY l.inserted_at DESC \
         LIMIT $2",
    )
    .bind(cutoff)
    .bind(RECONCILE_PAGE_SIZE)
    .fetch_all(elixir_pool)
    .await
    .map_err(|e| anyhow::anyhow!("reconcile: likes query failed: {e}"))?;

    stats.likes_attempted = likes.len() as u64;
    let likes_futures: Vec<RecFuture<'_>> = likes.iter().map(|row| -> RecFuture<'_> {
        let uuid_str = row.nft_uuid.to_string();
        let liker = row.liker_address.to_lowercase();
        let contract = row.contract_address.to_lowercase();
        let tx_hash = row.tx_hash.clone();
        Box::pin(async move {
            graph_sync.sync_like(&contract, &uuid_str, &liker, &tx_hash).await
        })
    }).collect();
    stats.total_failed += run_section("likes", likes_futures).await;

    // ── Purchases ─────────────────────────────────────────────────────────────
    let purchases: Vec<PurchaseRow> = sqlx::query_as(
        "SELECT p.buyer_address, p.tx_hash, n.id AS nft_uuid, n.contract_address \
         FROM purchases p \
         JOIN nfts n ON n.id = p.nft_id \
         WHERE p.inserted_at > $1 \
         ORDER BY p.inserted_at DESC \
         LIMIT $2",
    )
    .bind(cutoff)
    .bind(RECONCILE_PAGE_SIZE)
    .fetch_all(elixir_pool)
    .await
    .map_err(|e| anyhow::anyhow!("reconcile: purchases query failed: {e}"))?;

    stats.purchases_attempted = purchases.len() as u64;
    let purchases_futures: Vec<RecFuture<'_>> = purchases.iter().map(|row| -> RecFuture<'_> {
        let uuid_str = row.nft_uuid.to_string();
        let buyer = row.buyer_address.to_lowercase();
        let contract = row.contract_address.to_lowercase();
        let tx_hash = row.tx_hash.clone();
        Box::pin(async move {
            graph_sync.sync_purchase(&contract, &uuid_str, &buyer, &tx_hash).await
        })
    }).collect();
    stats.total_failed += run_section("purchases", purchases_futures).await;

    // ── Comments ──────────────────────────────────────────────────────────────
    let comments: Vec<CommentRow> = sqlx::query_as(
        "SELECT c.commenter_address, c.tx_hash, c.comment_text, n.id AS nft_uuid \
         FROM comments c \
         JOIN nfts n ON n.id = c.nft_id \
         WHERE c.inserted_at > $1 \
         ORDER BY c.inserted_at DESC \
         LIMIT $2",
    )
    .bind(cutoff)
    .bind(RECONCILE_PAGE_SIZE)
    .fetch_all(elixir_pool)
    .await
    .map_err(|e| anyhow::anyhow!("reconcile: comments query failed: {e}"))?;

    stats.comments_attempted = comments.len() as u64;
    let comments_futures: Vec<RecFuture<'_>> = comments.iter().map(|row| -> RecFuture<'_> {
        let uuid_str = row.nft_uuid.to_string();
        // Truncate to 120 chars — same cap as inner_sync_comment's safe_preview filter.
        let preview: String = row.comment_text.chars().take(120).collect();
        let commenter = row.commenter_address.to_lowercase();
        let tx_hash = row.tx_hash.clone();
        Box::pin(async move {
            graph_sync.sync_comment(&uuid_str, &commenter, &tx_hash, &preview).await
        })
    }).collect();
    stats.total_failed += run_section("comments", comments_futures).await;

    // ── Follows ───────────────────────────────────────────────────────────────
    // TAG-S29-03: follow edges were permanently absent from reconciliation.
    // A Nebula outage during any UserFollowed event left the social graph
    // incomplete with no automatic recovery — FoF traversal quality degrades
    // silently. JOIN social_users twice to get addresses from UUID FKs.
    let follows: Vec<FollowRow> = sqlx::query_as(
        "SELECT su1.address AS follower_address, su2.address AS followee_address, f.tx_hash \
         FROM follows f \
         JOIN social_users su1 ON su1.id = f.follower_id \
         JOIN social_users su2 ON su2.id = f.followee_id \
         WHERE f.inserted_at > $1 \
           AND f.is_active = true \
         ORDER BY f.inserted_at DESC \
         LIMIT $2",
    )
    .bind(cutoff)
    .bind(RECONCILE_PAGE_SIZE)
    .fetch_all(elixir_pool)
    .await
    .map_err(|e| anyhow::anyhow!("reconcile: follows query failed: {e}"))?;

    stats.follows_attempted = follows.len() as u64;
    let follows_futures: Vec<RecFuture<'_>> = follows.iter().map(|row| -> RecFuture<'_> {
        let tx_hash = row.tx_hash.clone().unwrap_or_default();
        let follower = row.follower_address.to_lowercase();
        let followee = row.followee_address.to_lowercase();
        Box::pin(async move {
            graph_sync.sync_follow(&follower, &followee, &tx_hash).await
        })
    }).collect();
    stats.total_failed += run_section("follows", follows_futures).await;

    // ── Bookmarks ─────────────────────────────────────────────────────────────
    // S30-04: bookmark edges were absent from reconciliation — a Nebula outage
    // during any ContentBookmarked event left `bookmarked` edges permanently
    // missing with no recovery path. JOIN social_users to resolve address FK.
    // Note: no `shares` table exists in the Elixir DB, so only bookmarks are
    // reconcilable; share events are captured via Kafka and graph_sync::sync_share.
    let bookmarks: Vec<BookmarkRow> = sqlx::query_as(
        // bookmarks table uses user_address (string) not user_id FK — no social_users join needed
        "SELECT b.user_address, b.tx_hash, n.id AS nft_uuid \
         FROM bookmarks b \
         JOIN nfts n ON n.id = b.nft_id \
         WHERE b.inserted_at > $1 \
         ORDER BY b.inserted_at DESC \
         LIMIT $2",
    )
    .bind(cutoff)
    .bind(RECONCILE_PAGE_SIZE)
    .fetch_all(elixir_pool)
    .await
    .map_err(|e| anyhow::anyhow!("reconcile: bookmarks query failed: {e}"))?;

    stats.bookmarks_attempted = bookmarks.len() as u64;
    let bookmarks_futures: Vec<RecFuture<'_>> = bookmarks.iter().map(|row| -> RecFuture<'_> {
        let uuid_str = row.nft_uuid.to_string();
        let tx_hash = row.tx_hash.clone().unwrap_or_default();
        let user = row.user_address.to_lowercase();
        Box::pin(async move {
            graph_sync.sync_bookmark(&uuid_str, &user, &tx_hash).await
        })
    }).collect();
    stats.total_failed += run_section("bookmarks", bookmarks_futures).await;

    info!(
        likes = stats.likes_attempted,
        purchases = stats.purchases_attempted,
        comments = stats.comments_attempted,
        follows = stats.follows_attempted,
        bookmarks = stats.bookmarks_attempted,
        failed = stats.total_failed,
        lookback_hours,
        "reconcile_nebula_edges: pass complete"
    );

    Ok(stats)
}
