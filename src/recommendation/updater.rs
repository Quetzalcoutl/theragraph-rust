use crate::recommendation::engine::RecommendationEngine;
use chrono::{Duration as ChronoDuration, Utc};
use sqlx::PgPool;
use tracing::{error, info, warn};

/// Update recommendations for all active users
pub async fn update_all_recommendations(pool: &PgPool) -> anyhow::Result<()> {
    // 1. Identify active users (interacted in last 7 days)
    // We look at interactions, or just users who have logged in/connected
    // For now, let's use the social_users table if it has last_seen, or just interactions.
    // If we don't have last_seen, getting all users might be fine for MVP.
    // But let's try to filter by interactions to be efficient.

    let active_since = (Utc::now() - ChronoDuration::days(7)).naive_utc();

    // Find users who have interacted in the last 7 days
    let active_users: Vec<String> = sqlx::query_scalar!(
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
        WHERE f.block_timestamp > $1
        "#,
        active_since
    )
    .fetch_all(pool)
    .await
    .unwrap_or_else(|e| {
        warn!(
            "Failed to fetch active users: {}, falling back to recent users",
            e
        );
        vec![]
    });

    // If no interactions found (e.g. fresh DB), maybe just get all users for now to seed?
    let users_to_update = if active_users.is_empty() {
        sqlx::query_scalar!("SELECT address FROM social_users LIMIT 100")
            .fetch_all(pool)
            .await
            .unwrap_or_default()
    } else {
        active_users
    };

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

    // Shared engine instance (cheap to clone as it just holds a pool)
    let engine = RecommendationEngine::new(pool.clone());
    let graph_client = std::sync::Arc::new(crate::recommendation::graph_client::GraphClient::new());

    for user_address in users_to_update.clone() {
        let engine = engine.clone(); // RecommendationEngine is cheap to clone
        let graph_client = graph_client.clone();
        let permit = semaphore.clone().acquire_owned().await.unwrap();

        set.spawn(async move {
            let _permit = permit; // Hold permit until task completion

            // 1. Generate standard SQL/ML recommendations
            let rec_result = engine
                .get_recommendations(&user_address, 50, None, true)
                .await;

            // 2. Warm up following feed (fire and forget inside this task)
            let follow_result = engine.get_following_feed(&user_address, 50, 0).await;

            // 3. "ByteGraph" Power: Offload traversal to NebulaGraph
            // (Fire and forget for now as we just log the output in PoC)
            let graph_result = graph_client.get_fof_recommendations(&user_address).await;

            (user_address, rec_result, follow_result, graph_result)
        });
    }

    let mut success_count = 0;
    let _total_users = users_to_update.len();

    // Process results as they finish (stream-like processing)
    while let Some(res) = set.join_next().await {
        match res {
            Ok((addr, rec_res, follow_res, graph_res)) => {
                match rec_res {
                    Ok(_) => success_count += 1,
                    Err(e) => warn!("Failed to generate recommendations for {}: {}", addr, e),
                }

                if let Err(e) = follow_res {
                    warn!("Failed to warmup following feed for {}: {}", addr, e);
                }

                if let Err(e) = graph_res {
                    // Log but don't fail the job, as graph might be optional/offline in some envs
                    warn!("Graph traversal failed for {}: {}", addr, e);
                }
            }
            Err(e) => error!("Task join error: {}", e),
        }
    }

    info!(
        "✅ Recommendations updated for {}/{} users",
        success_count,
        users_to_update.len()
    );
    Ok(())
}
