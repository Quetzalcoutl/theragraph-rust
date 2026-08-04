//! Nebula Write DLQ — fire-and-forget with rate limiting.
//!
//! Every best-effort Nebula write that fails permanently is recorded via
//! `spawn_record()` — a non-blocking wrapper that:
//!
//!  1. Increments `nebula_edge_write_failures_total{operation}` (always, even if DB
//!     insert is skipped by the rate limiter).
//!  2. Checks the sliding-window rate limiter (100 inserts / 60 s by default,
//!     overridable via `NEBULA_DLQ_RATE_LIMIT_PER_MIN`). If the window is
//!     saturated, the DB insert is skipped and a separate counter is incremented.
//!  3. Spawns a tokio task capped at 5 s to INSERT into `nebula_write_failures`.
//!     The calling write path is never blocked.
//!
//! ## Replay
//!
//! ```sql
//! SELECT id, operation_type, user_address, post_id, error_message, created_at
//! FROM nebula_write_failures
//! WHERE replayed_at IS NULL AND created_at > now() - interval '1 hour'
//! ORDER BY created_at;
//!
//! UPDATE nebula_write_failures
//! SET replayed_at = now(), replay_count = replay_count + 1
//! WHERE id IN (...);
//! ```

use once_cell::sync::Lazy;
use sqlx::PgPool;
use std::sync::atomic::{AtomicU64, Ordering};
use tracing::{error, warn};

// ── Sliding-window rate limiter ───────────────────────────────────────────────

/// Timestamp (unix seconds) when the current 60-second rate window started.
static RATE_WINDOW_START: Lazy<AtomicU64> = Lazy::new(|| AtomicU64::new(0));
/// Number of DLQ inserts attempted in the current rate window.
static RATE_WINDOW_COUNT: Lazy<AtomicU64> = Lazy::new(|| AtomicU64::new(0));

/// Max DLQ DB inserts per 60-second window. Prevents a Nebula outage from
/// flooding the Postgres table. Prometheus counter always increments — only the
/// DB write is throttled.
///
/// Override via `NEBULA_DLQ_RATE_LIMIT_PER_MIN` env var (default: 100).
fn dlq_rate_limit() -> u64 {
    static LIMIT: Lazy<u64> = Lazy::new(|| {
        std::env::var("NEBULA_DLQ_RATE_LIMIT_PER_MIN")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(100)
    });
    *LIMIT
}

/// Returns `true` if the current insert is within the rate limit.
/// Uses a best-effort sliding window (not perfectly accurate under concurrent
/// writes, but sufficient for flood prevention).
fn within_rate_limit() -> bool {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let window_start = RATE_WINDOW_START.load(Ordering::Relaxed);
    if now.saturating_sub(window_start) >= 60 {
        // New window — reset counter.
        RATE_WINDOW_START.store(now, Ordering::Relaxed);
        RATE_WINDOW_COUNT.store(1, Ordering::Relaxed);
        true
    } else {
        RATE_WINDOW_COUNT.fetch_add(1, Ordering::Relaxed) < dlq_rate_limit()
    }
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Record a Nebula write failure.
///
/// Always increments the Prometheus counter. Spawns a tokio task (capped at 5 s)
/// to INSERT the failure into Postgres if the rate limit allows.
/// The calling write path is **never blocked** — the spawn returns immediately.
///
/// Jobs audit: previously named `spawn_record`. Renamed to `record_failure`
/// because callers care about *what* this does (record a failure), not *how*
/// (spawning a task is an implementation detail they should never need to know).
pub fn record_failure(
    pool: &PgPool,
    operation_type: &'static str,
    user_address: Option<String>,
    post_id: Option<String>,
    query: String,
    err: String,
) {
    // Always increment the counter — visible in /metrics even if DB insert is skipped.
    metrics::counter!(
        "nebula_edge_write_failures_total",
        "operation" => operation_type
    )
    .increment(1);

    if !within_rate_limit() {
        metrics::counter!("nebula_dlq_rate_limited_total", "operation" => operation_type)
            .increment(1);
        warn!(
            "NebulaDlq: rate limit reached for operation={} — DB insert skipped. \
             Consider investigating persistent Nebula connectivity.",
            operation_type
        );
        return;
    }

    let pool = pool.clone();
    tokio::spawn(async move {
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            insert_record(&pool, operation_type, user_address.as_deref(), post_id.as_deref(), &query, &err),
        )
        .await;

        match result {
            Err(_) => warn!(
                "NebulaDlq: DB insert timed out after 5s for operation={}",
                operation_type
            ),
            Ok(Err(db_err)) => warn!(
                "NebulaDlq: DB insert failed for operation={}: {}",
                operation_type, db_err
            ),
            Ok(Ok(())) => {}
        }
    });
}

async fn insert_record(
    pool: &PgPool,
    operation_type: &str,
    user_address: Option<&str>,
    post_id: Option<&str>,
    query: &str,
    error_message: &str,
) -> anyhow::Result<()> {
    let query_preview: String = query.chars().take(1000).collect();

    sqlx::query(
        r#"
        INSERT INTO nebula_write_failures
            (operation_type, user_address, post_id, query_preview, error_message)
        VALUES ($1, $2, $3, $4, $5)
        "#,
    )
    .bind(operation_type)
    .bind(user_address)
    .bind(post_id)
    .bind(&query_preview)
    .bind(error_message)
    .execute(pool)
    .await?;

    Ok(())
}

/// Replay unreplayed failures from the last 24 hours.
///
/// Called by `spawn_nebula_dlq_replayer` every 15 minutes.  Processes up to 50
/// rows per pass in creation-date order (oldest first so high-priority failures
/// — follows/likes — don't starve behind newer view_event noise).
///
/// All Nebula write queries stored in `query_preview` use `IF NOT EXISTS` or
/// `UPSERT` semantics — replaying is safe and idempotent.
///
/// A failure after `replay_count >= 10` is left unreplayed and emits a
/// structured ERROR log for manual triage; it does not block the replay loop.
pub async fn replay_pending(
    pool: &PgPool,
    graph: &dyn crate::recommendation::graph_client::GraphTraversal,
) -> anyhow::Result<ReplayStats> {
    #[derive(sqlx::FromRow)]
    struct FailureRow {
        id: i64,
        operation_type: String,
        query_preview: String,
        replay_count: i32,
    }

    let rows: Vec<FailureRow> = sqlx::query_as(
        r#"
        SELECT id, operation_type, query_preview, replay_count
        FROM nebula_write_failures
        WHERE replayed_at IS NULL
          AND created_at > now() - interval '24 hours'
          AND replay_count < 10
        ORDER BY created_at ASC
        LIMIT 50
        "#,
    )
    .fetch_all(pool)
    .await?;

    let total = rows.len();
    if total == 0 {
        return Ok(ReplayStats::default());
    }

    let mut stats = ReplayStats { total: total as u64, ..Default::default() };

    for row in rows {
        let op = &row.operation_type;
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            graph.raw_write(&row.query_preview),
        )
        .await;

        let success = match result {
            Ok(Ok(_)) => true,
            Ok(Err(ref e)) => {
                warn!(
                    "NebulaDlq: replay failed id={} op={} attempt={}: {}",
                    row.id, op, row.replay_count + 1, e
                );
                false
            }
            Err(_) => {
                warn!(
                    "NebulaDlq: replay timed out id={} op={} attempt={}",
                    row.id, op, row.replay_count + 1
                );
                false
            }
        };

        if success {
            let _ = sqlx::query(
                "UPDATE nebula_write_failures \
                 SET replayed_at = now(), replay_count = replay_count + 1 \
                 WHERE id = $1",
            )
            .bind(row.id)
            .execute(pool)
            .await;
            stats.replayed += 1;
            metrics::counter!("nebula_dlq_replayed_total", "operation" => op.clone()).increment(1);
        } else {
            let new_count = row.replay_count + 1;
            let _ = sqlx::query(
                "UPDATE nebula_write_failures SET replay_count = replay_count + 1 WHERE id = $1",
            )
            .bind(row.id)
            .execute(pool)
            .await;
            stats.failed += 1;

            if new_count >= 10 {
                error!(
                    "NebulaDlq: id={} op={} exhausted 10 replay attempts — manual triage required. \
                     See RUNBOOK.md § 3.",
                    row.id, op
                );
                metrics::counter!("nebula_dlq_exhausted_total", "operation" => op.clone()).increment(1);
            }
        }
    }

    Ok(stats)
}

/// Statistics from one replay pass.
#[derive(Debug, Default)]
pub struct ReplayStats {
    /// Total rows considered in this pass.
    pub total:    u64,
    /// Rows successfully replayed and marked `replayed_at`.
    pub replayed: u64,
    /// Rows that failed or timed out during this pass.
    pub failed:   u64,
}
/// Returns None if the query fails (Postgres unavailable).
pub async fn failure_count_last_hour(pool: &PgPool, operation_type: &str) -> Option<i64> {
    sqlx::query_scalar::<_, i64>(
        r#"
        SELECT count(*)
        FROM nebula_write_failures
        WHERE operation_type = $1
          AND created_at > now() - interval '1 hour'
        "#,
    )
    .bind(operation_type)
    .fetch_one(pool)
    .await
    .map_err(|e| {
        error!("NebulaDlq::failure_count_last_hour query failed: {e}");
        e
    })
    .ok()
}

/// Total unreplayed failures in the last 5 minutes — used by the health check.
pub async fn recent_unreplayed_count(pool: &PgPool) -> Option<i64> {
    sqlx::query_scalar::<_, i64>(
        r#"
        SELECT count(*)
        FROM nebula_write_failures
        WHERE replayed_at IS NULL
          AND created_at > now() - interval '5 minutes'
        "#,
    )
    .fetch_one(pool)
    .await
    .map_err(|e| {
        error!("NebulaDlq::recent_unreplayed_count query failed: {e}");
        e
    })
    .ok()
}

