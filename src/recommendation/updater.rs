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

            // Run all six independent calls in parallel (tokio::join!)
            // enhanced_result uses warmup_enhanced_feed (70-min TTL) so the cache
            // survives the full 1-hour update interval; get_enhanced_feed_cached
            // only writes 5-min TTL which expires 55 mins before the next run.
            //
            // CC-002: use get_recommendations_coalesced (not get_recommendations)
            // so concurrent background update tasks for the same user are coalesced
            // through the per-user mutex rather than running duplicate scoring passes.
            //
            // WIRE-04: remove leading underscores — these values ARE used: each
            // get_*_fof_* call writes its results to the Redis cache so that
            // get_recommendations/get_enhanced_feed can read them during scoring.
            let (rec_result, follow_result, enhanced_result, fof_recs, view_fof_recs, comment_fof_recs, purchase_fof_recs, share_fof_recs, bookmark_fof_recs) = tokio::join!(
                engine.get_recommendations_coalesced(&user_address, 50, None, true),
                engine.get_following_feed(&user_address, 50, 0),
                engine.warmup_enhanced_feed(&user_address),
                // GraphTraversal impls are infallible (errors swallowed + logged inside).
                // All six FoF variants are pre-warmed so the Redis cache is hot at request time.
                // ByteGraph signal hierarchy: purchase (0.15) → share (0.12) → follow-like (0.10) → bookmark (0.08) → comment (0.05) → view (0.05)
                graph_client.get_fof_recommendations(&user_address),
                graph_client.get_view_event_fof_recommendations(&user_address),
                graph_client.get_comment_fof_recommendations(&user_address),
                graph_client.get_purchase_fof_recommendations(&user_address),
                graph_client.get_shared_fof_recommendations(&user_address),
                graph_client.get_bookmark_fof_recommendations(&user_address),
            );
            tracing::debug!(
                fof_like_count = fof_recs.len(),
                fof_view_count = view_fof_recs.len(),
                fof_comment_count = comment_fof_recs.len(),
                fof_purchase_count = purchase_fof_recs.len(),
                fof_share_count = share_fof_recs.len(),
                fof_bookmark_count = bookmark_fof_recs.len(),
                "FoF cache pre-warm counts for {}",
                user_address,
            );

            (user_address, rec_result, follow_result, enhanced_result)
        });
    }

    let mut stats = UpdateStats { success_count: 0, total_count };

    // Process results as they finish (stream-like processing)
    while let Some(res) = set.join_next().await {
        match res {
            Ok((addr, rec_res, follow_res, enhanced_res)) => {
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

/// Delete stale `recommended_to` edges older than `older_than_days`.
///
/// Implements the pruning strategy from theragraph-nebula/init/prune-recommended-to.ngql.
/// Migration 11 (rec_to_computed_index) must be applied first or this scan will
/// time-out on large clusters (LOOKUP ON without an index is a full edge-type scan).
///
/// Both served=true and served=false edges older than the cutoff are deleted:
///   - served=true:  feedback loop already consumed the click signal, edge is waste.
///   - served=false: stale unseen recommendation, user won't act on 30-day-old content.
///
/// Call from a periodic Oban-style timer or tokio::spawn loop (recommended: daily).
/// Default cutoff: 30 days.
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

    let nql = format!(
        "USE {space};\n\
         LOOKUP ON recommended_to \
           WHERE recommended_to.computed_at < {cutoff} \
           YIELD src(edge) AS src, dst(edge) AS dst, rank(edge) AS rank \
         | DELETE EDGE recommended_to $-.src -> $-.dst @ $-.rank;",
        space = SPACE_THERAGRAPH,
        cutoff = cutoff,
    );

    match graph_client.raw_write(&nql).await {
        Ok(_) => {
            info!(
                older_than_days,
                cutoff,
                "prune_stale_recommended_to: completed"
            );
            Ok(())
        }
        Err(e) => {
            error!("prune_stale_recommended_to: failed: {e}");
            Err(e)
        }
    }
}

/// Delete stale `comments_on` edges older than `older_than_days`.
///
/// Requires `comments_on_prune_idx` (migration 19) — without it NebulaGraph
/// falls back to a full edge-type scan which is expensive at scale.
///
/// 90-day default keeps enough recency signal for FoF traversal while bounding
/// unbounded growth (~N_comments/day at steady state).
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

    let nql = format!(
        "USE {space};\n\
         LOOKUP ON comments_on \
           WHERE comments_on.commented_at < {cutoff} \
           YIELD src(edge) AS src, dst(edge) AS dst, rank(edge) AS rank \
         | DELETE EDGE comments_on $-.src -> $-.dst @ $-.rank;",
        space = SPACE_THERAGRAPH,
        cutoff = cutoff,
    );

    match graph_client.raw_write(&nql).await {
        Ok(_) => {
            info!(older_than_days, cutoff, "prune_stale_comments_on: completed");
            Ok(())
        }
        Err(e) => {
            error!("prune_stale_comments_on: failed: {e}");
            Err(e)
        }
    }
}
