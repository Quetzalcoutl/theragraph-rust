use crate::recommendation::engine::RecommendationEngine;
use crate::recommendation::graph_client::GraphTraversal;
use crate::recommendation::schema_consts::SPACE_THERAGRAPH;
use chrono::{Duration as ChronoDuration, NaiveDateTime, Utc};
use sqlx::PgPool;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{error, info, instrument, warn};

/// Counts from one recommendation update pass.
pub struct UpdateStats {
    pub success_count: usize,
    pub total_count: usize,
}

/// Return addresses active in the last `active_since` window.
///
/// Falls back to `social_users LIMIT 100` when no interactions exist (fresh DB / seed).
/// Pure-ish: no side effects beyond the DB read; safe to call in tests with a seeded pool.
// RS-08: add span so the active-user DB query is visible in traces.
#[instrument(skip(pool), fields(active_since = %active_since))]
pub async fn select_active_users(pool: &PgPool, active_since: NaiveDateTime) -> Vec<String> {
    let mut active_users: Vec<String> = sqlx::query_scalar!(
        r#"
        SELECT DISTINCT liker_address as "address!" FROM likes WHERE timestamp > $1
        UNION
        SELECT DISTINCT commenter_address as "address!" FROM comments WHERE timestamp > $1
        UNION
        SELECT DISTINCT buyer_address as "address!" FROM purchases WHERE timestamp > $1
        UNION
        SELECT DISTINCT s.address as "address!"
        FROM follows f
        JOIN social_users s ON f.follower_id = s.id
        WHERE f.inserted_at > $1
        "#,
        active_since
    )
    .fetch_all(pool)
    .await
    .unwrap_or_else(|e| {
        warn!("Failed to fetch active users: {}, falling back to recent users", e);
        vec![]
    });

    active_users.truncate(5_000);

    if active_users.is_empty() {
        sqlx::query_scalar!("SELECT address FROM social_users LIMIT 100")
            .fetch_all(pool)
            .await
            .unwrap_or_default()
    } else {
        active_users
    }
}

/// Update recommendations for all active users.
///
/// Accepts a pre-built `Arc<RecommendationEngine>` so callers control construction
/// (pool, cache, graph_client) and tests can inject a lightweight engine without a
/// live PgPool. Accepts `Arc<dyn GraphTraversal>` separately because FoF pre-warm
/// calls go directly to the graph layer, bypassing the engine's scoring path.
///
/// Callers construct and pass both:
/// ```ignore
/// let engine = Arc::new(RecommendationEngine::new(pool.clone())
///     .with_cache(cache.clone())
///     .with_graph_client(gc.clone()));
/// update_all_recommendations(engine, gc).await?;
/// ```
// RS-08: add span so each update cycle appears in traces.
#[instrument(skip(engine, graph_client), err)]
pub async fn update_all_recommendations(
    engine: Arc<RecommendationEngine>,
    graph_client: Arc<dyn GraphTraversal>,
) -> anyhow::Result<()> {
    let active_since = (Utc::now() - ChronoDuration::days(7)).naive_utc();
    let users_to_update = select_active_users(engine.pool(), active_since).await;

    if users_to_update.is_empty() {
        info!("No users to update recommendations for.");
        return Ok(());
    }

    info!(
        "🔄 Updating recommendations for {} users...",
        users_to_update.len()
    );

    // Limit concurrency to 10 simultaneous updates to prevent DB saturation
    // (Alex Crichton / Niko Matsakis style: explicit concurrency control)
    const CONCURRENCY_LIMIT: usize = 10;

    // Use a JoinSet to manage concurrent tasks and collect results
    let mut set = tokio::task::JoinSet::new();
    let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(CONCURRENCY_LIMIT));

    let total_count = users_to_update.len();
    for user_address in users_to_update {
        let engine = engine.clone(); // RecommendationEngine is cheap to clone
        let graph_client = graph_client.clone();
        // Semaphore only closes during shutdown — treat as clean stop.
        let permit = match semaphore.clone().acquire_owned().await {
            Ok(p) => p,
            Err(_) => {
                warn!("Semaphore closed during recommendation update — stopping early");
                break;
            }
        };

        set.spawn(async move {
            let _permit = permit; // Hold permit until task completion

            // Run all independent calls in parallel (tokio::join!)
            // enhanced_result uses warmup_enhanced_feed (70-min TTL) so the cache
            // survives the full 1-hour update interval; get_enhanced_feed_cached
            // only writes 5-min TTL which expires 55 mins before the next run.
            // genre_result mirrors that same 70-vs-short-TTL split for a user's
            // PRIMARY genre-filtered feed (GENRE-02 follow-up) — see
            // `engine::feeds::warmup_genre_feed`; it no-ops for users with no
            // declared `genre_preference` edges, so this costs nothing extra
            // for users who never set genre favorites.
            //
            // CC-002: use get_recommendations_coalesced (not get_recommendations)
            // so concurrent background update tasks for the same user are coalesced
            // through the per-user mutex rather than running duplicate scoring passes.
            //
            // WIRE-04: remove leading underscores — these values ARE used: each
            // get_*_fof_* call writes its results to the Redis cache so that
            // get_recommendations/get_enhanced_feed can read them during scoring.
            let (rec_result, follow_result, enhanced_result, genre_result, fof_recs, view_fof_recs, comment_fof_recs, purchase_fof_recs, share_fof_recs, bookmark_fof_recs, flix_fof_recs, style_prefs) = tokio::join!(
                engine.get_recommendations_coalesced(&user_address, 50, None, true, &[]),
                engine.get_following_feed(&user_address, 50, 0),
                engine.warmup_enhanced_feed(&user_address),
                engine.warmup_genre_feed(&user_address),
                // GraphTraversal impls are infallible (errors swallowed + logged inside).
                // All seven FoF variants are pre-warmed so the Redis cache is hot at request time.
                // ByteGraph signal hierarchy: purchase (0.15) → share (0.12) → follow-like (0.10) → flix_watch (0.08) → bookmark (0.08) → comment (0.05) → view (0.05)
                graph_client.get_fof_recommendations(&user_address),
                graph_client.get_view_event_fof_recommendations(&user_address),
                graph_client.get_comment_fof_recommendations(&user_address),
                graph_client.get_purchase_fof_recommendations(&user_address),
                graph_client.get_shared_fof_recommendations(&user_address),
                graph_client.get_bookmark_fof_recommendations(&user_address),
                graph_client.get_flix_watch_fof_recommendations(&user_address),
                // Companion chat signal (style_preference edges) — same pre-warm
                // pattern as FoF: apply_cache_boosts only ever reads the Redis
                // cache this populates, never traverses Nebula on the request path.
                graph_client.get_style_preferences(&user_address),
            );
            tracing::debug!(
                fof_like_count = fof_recs.len(),
                fof_view_count = view_fof_recs.len(),
                fof_comment_count = comment_fof_recs.len(),
                fof_purchase_count = purchase_fof_recs.len(),
                fof_share_count = share_fof_recs.len(),
                fof_bookmark_count = bookmark_fof_recs.len(),
                fof_flix_count = flix_fof_recs.len(),
                style_prefs_count = style_prefs.len(),
                "FoF cache pre-warm counts for {}",
                user_address,
            );

            (user_address, rec_result, follow_result, enhanced_result, genre_result)
        });
    }

    let mut stats = UpdateStats { success_count: 0, total_count };

    // Process results as they finish (stream-like processing)
    while let Some(res) = set.join_next().await {
        match res {
            Ok((addr, rec_res, follow_res, enhanced_res, genre_res)) => {
                match rec_res {
                    Ok(_) => stats.success_count += 1,
                    Err(e) => warn!("Failed to generate recommendations for {}: {}", addr, e),
                }
                if let Err(e) = follow_res {
                    warn!("Failed to warmup following feed for {}: {}", addr, e);
                }
                if let Err(e) = enhanced_res {
                    warn!("Failed to warmup enhanced feed for {}: {}", addr, e);
                }
                if let Err(e) = genre_res {
                    warn!("Failed to warmup genre feed for {}: {}", addr, e);
                }
            }
            Err(e) => error!("Task join error: {}", e),
        }
    }

    info!(
        "✅ Recommendations updated for {}/{} users",
        stats.success_count,
        stats.total_count
    );

    // Prune stale recommended_to edges (older than 30 days).
    // Called here so pruning runs once per update cycle rather than on a separate timer.
    // prune_stale_recommended_to uses rec_to_computed_index (migration 11) which is a
    // LOOKUP ON — index-backed and fast at steady state.
    prune_stale_recommended_to(graph_client.as_ref(), 30)
        .await
        .unwrap_or_else(|e| warn!("prune_stale_recommended_to failed: {e}"));

    // Prune stale comments_on edges (older than 90 days).
    // comments_on grows ~N_comments/day unboundedly. 90-day window keeps enough
    // signal for FoF traversals while bounding edge count.
    // Requires comments_on_prune_idx from migration 19.
    prune_stale_comments_on(graph_client.as_ref(), 90)
        .await
        .unwrap_or_else(|e| warn!("prune_stale_comments_on failed: {e}"));

    Ok(())
}

/// Parse edge rows from nebula-console LOOKUP output.
///
/// The transport operates in interactive mode where each `;`-terminated statement
/// runs as a separate query. The `$var` compound-query pattern (where one statement
/// assigns `$stale = LOOKUP ...` and the next does `DELETE EDGE ... $stale.*`) does
/// NOT work in interactive mode — `$stale` is scoped to a single execute() call and
/// is invisible to the next statement.
///
/// Solution: two-phase approach — LOOKUP first (via raw_write, which accepts reads
/// too), parse the edge rows here, then issue DELETE EDGE with explicit vertex IDs.
///
/// Format expected (nebula-console table output):
/// ```text
/// +----------+----------+------+
/// | src      | dst      | rank |
/// +----------+----------+------+
/// | "0xabc"  | "42"     | 0    |
/// +----------+----------+------+
/// Got 1 rows (time spent ...)
/// ```
fn parse_nebula_edge_rows(output: &str) -> Vec<(String, String, i64)> {
    let mut edges = Vec::with_capacity(512);
    let mut header_seen = false;

    for line in output.lines() {
        let line = line.trim();
        if line.starts_with('+') || line.is_empty() {
            continue;
        }
        if line.starts_with('|') {
            if !header_seen {
                header_seen = true;
                continue;
            }
            let mut cols = line.trim_matches('|').split('|').map(str::trim);
            let (src, dst, rank) = match (cols.next(), cols.next(), cols.next()) {
                (Some(s), Some(d), Some(r)) => (s, d, r.parse::<i64>().unwrap_or(0)),
                _ => continue,
            };
            if !src.is_empty() && !dst.is_empty() {
                edges.push((src.to_owned(), dst.to_owned(), rank));
            }
        }
    }

    edges
}

/// Delete stale `recommended_to` edges older than `older_than_days`.
///
/// Migration 11 (rec_to_computed_index) must be applied first or the LOOKUP scan
/// degrades to a full edge-type scan on large clusters.
///
/// Uses a two-phase approach (see `parse_nebula_edge_rows`) because nebula-console's
/// interactive mode isolates `$var` per statement — compound queries with `$var`
/// spanning statement boundaries always fail with -1009 "Missing yield clause".
///
/// Call from a periodic timer (recommended: daily). Default cutoff: 30 days.
#[instrument(skip(graph_client), fields(older_than_days))]
pub async fn prune_stale_recommended_to(
    graph_client: &dyn GraphTraversal,
    older_than_days: u32,
) -> anyhow::Result<()> {
    let cutoff = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .saturating_sub(older_than_days as u64 * 86_400);

    // Phase 1: discover stale edges.
    let lookup_nql = format!(
        "USE {space}; LOOKUP ON recommended_to \
           WHERE recommended_to.computed_at < {cutoff} \
           YIELD src(edge) AS src, dst(edge) AS dst, rank(edge) AS rank;",
        space = SPACE_THERAGRAPH,
        cutoff = cutoff,
    );

    let lookup_out = graph_client.raw_write(&lookup_nql).await
        .map_err(|e| { error!("prune_stale_recommended_to: lookup failed: {e}"); e })?;

    let edges = parse_nebula_edge_rows(&lookup_out);

    if edges.is_empty() {
        info!(older_than_days, cutoff, "prune_stale_recommended_to: no stale edges");
        return Ok(());
    }

    info!(older_than_days, cutoff, count = edges.len(), "prune_stale_recommended_to: deleting stale edges");

    // Phase 2: delete in 100-edge batches to keep statement size bounded.
    for chunk in edges.chunks(100) {
        let edge_list: String = chunk
            .iter()
            .map(|(src, dst, rank)| format!("{} -> {} @ {}", src, dst, rank))
            .collect::<Vec<_>>()
            .join(", ");
        let delete_nql = format!(
            "USE {space}; DELETE EDGE recommended_to {edges};",
            space = SPACE_THERAGRAPH,
            edges = edge_list,
        );
        if let Err(e) = graph_client.raw_write(&delete_nql).await {
            error!("prune_stale_recommended_to: batch delete failed: {e}");
            return Err(e);
        }
    }

    info!(older_than_days, cutoff, "prune_stale_recommended_to: completed");
    Ok(())
}

/// Delete stale `comments_on` edges older than `older_than_days`.
///
/// Requires `comments_on_prune_idx` (migration 19) — without it NebulaGraph
/// falls back to a full edge-type scan which is expensive at scale.
///
/// 90-day default keeps enough recency signal for FoF traversal while bounding
/// unbounded growth (~N_comments/day at steady state).
///
/// Same two-phase approach as `prune_stale_recommended_to` — see that function's
/// doc for why the `$var` compound-query pattern fails in interactive mode.
#[instrument(skip(graph_client), fields(older_than_days))]
pub async fn prune_stale_comments_on(
    graph_client: &dyn GraphTraversal,
    older_than_days: u32,
) -> anyhow::Result<()> {
    let cutoff = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .saturating_sub(older_than_days as u64 * 86_400);

    let lookup_nql = format!(
        "USE {space}; LOOKUP ON comments_on \
           WHERE comments_on.commented_at < {cutoff} \
           YIELD src(edge) AS src, dst(edge) AS dst, rank(edge) AS rank;",
        space = SPACE_THERAGRAPH,
        cutoff = cutoff,
    );

    let lookup_out = graph_client.raw_write(&lookup_nql).await
        .map_err(|e| { error!("prune_stale_comments_on: lookup failed: {e}"); e })?;

    let edges = parse_nebula_edge_rows(&lookup_out);

    if edges.is_empty() {
        info!(older_than_days, cutoff, "prune_stale_comments_on: no stale edges");
        return Ok(());
    }

    info!(older_than_days, cutoff, count = edges.len(), "prune_stale_comments_on: deleting stale edges");

    for chunk in edges.chunks(100) {
        let edge_list: String = chunk
            .iter()
            .map(|(src, dst, rank)| format!("{} -> {} @ {}", src, dst, rank))
            .collect::<Vec<_>>()
            .join(", ");
        let delete_nql = format!(
            "USE {space}; DELETE EDGE comments_on {edges};",
            space = SPACE_THERAGRAPH,
            edges = edge_list,
        );
        if let Err(e) = graph_client.raw_write(&delete_nql).await {
            error!("prune_stale_comments_on: batch delete failed: {e}");
            return Err(e);
        }
    }

    info!(older_than_days, cutoff, "prune_stale_comments_on: completed");
    Ok(())
}
