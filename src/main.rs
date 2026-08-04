//! TheraGraph Rust Engine
//!
//! A high-performance blockchain indexer and recommendation engine.
//!
//! # Architecture
//!
//! - **Indexers**: Poll blockchain for events and publish to Kafka
//! - **Recommendation Engine**: Personalized NFT recommendations
//! - **API Server**: REST endpoints for frontend consumption
//!
//! # Graceful Shutdown
//!
//! The engine handles SIGTERM and SIGINT signals, ensuring:
//! - In-flight requests complete
//! - Kafka messages are flushed
//! - Database connections are closed cleanly

use std::sync::Arc;
use std::time::Duration;
use futures::Future;
use tokio::signal;
use tokio::sync::broadcast;
use tracing::{error, info, warn};
use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

mod address;
mod api;
mod boards;
mod bundler;
mod config;
mod crypto;
mod database;
mod error;
mod event_processor;
mod events;
mod indexer;
mod kafka;
mod recommendation;

use config::Config;
use database::Database;
use error::Result;
use kafka::KafkaProducer;

/// Application state shared across components
pub struct AppState {
    pub config: Arc<Config>,
    pub db: Database,
    pub elixir_db: Database,
    pub kafka: KafkaProducer,
    pub shutdown: broadcast::Sender<()>,
    pub rec_cache: Option<recommendation::cache::RecCache>,
    pub graph_client: Arc<dyn recommendation::graph_client::GraphTraversal>,
    /// Set when KAFKA_ENABLED=false so indexers can write preference signals directly.
    pub direct_handlers: Option<Arc<event_processor::DirectHandlers>>,
    /// RS-03: TaskTracker for fire-and-forget graph write tasks.
    /// Tracked spawns (recommended_to batch writes, view_event / comments_on edges)
    /// are awaited during shutdown so no in-flight Nebula writes are orphaned.
    pub task_tracker: Arc<tokio_util::task::TaskTracker>,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Load .env before any env-var reads so local dev works without exporting vars.
    dotenvy::dotenv().ok();

    // SV-01: Fail fast if critical env vars are missing rather than discovering later at the
    // first auth check or DB connect.
    for var in &["DATABASE_URL", "INTERNAL_API_KEY"] {
        if std::env::var(var).is_err() {
            eprintln!("FATAL: required env var {} is not set — refusing to start", var);
            std::process::exit(1);
        }
    }

    // Initialize tracing with structured logging
    init_tracing();

    info!("═══════════════════════════════════════════════════════════════");
    info!("  🚀 TheraGraph Rust Engine v{}", env!("CARGO_PKG_VERSION"));
    info!("═══════════════════════════════════════════════════════════════");
    info!("  Components:");
    info!("    • Blockchain Indexers (2 indexers: friends, social)");
    info!("    • Recommendation Engine");
    info!("    • REST API Server");
    info!("═══════════════════════════════════════════════════════════════");

    // Cap rayon to half the logical CPUs so the freed threads stay available for
    // Tokio's async I/O runtime. Default (all CPUs) causes Tokio thread starvation
    // during flash-crowd scoring — spawn_blocking slots fill while async tasks queue.
    // min 1 prevents a zero-thread pool on single-core containers.
    let rayon_threads = std::thread::available_parallelism()
        .map(|n| (n.get() / 2).max(1))
        .unwrap_or(2);
    rayon::ThreadPoolBuilder::new()
        .num_threads(rayon_threads)
        .thread_name(|i| format!("rayon-score-{i}"))
        .build_global()
        .expect("failed to build rayon global thread pool");
    info!("✅ Rayon global thread pool initialized ({} workers)", rayon_threads);

    // Load configuration
    let config = Config::from_env()?;
    let config = Arc::new(config);
    info!("✅ Configuration loaded and validated");

    // Create shutdown channel
    let (shutdown_tx, _) = broadcast::channel::<()>(1);

    // Initialize Kafka producer
    let kafka_producer = KafkaProducer::new(&config.kafka)?;
    info!("✅ Kafka producer initialized");

    // Initialize database connection pool
    let db = Database::new(&config.database).await?;
    info!("✅ Database connection pool established");

    // Run migrations on the main recommendation DB
    info!("📦 Running database migrations...");
    database::run_migrations(db.pool()).await?;
    info!("✅ Database migrations applied");

    // Initialize Elixir database connection
    info!("🔗 Connecting to Elixir database...");
    let elixir_db = Database::new(&config.elixir_database).await?;
    info!("✅ Connected to Elixir database");

    // Ensure recommendation tables exist on the Elixir DB pool.
    // The API server queries `nfts` (Elixir-owned), but the rec engine also needs
    // its own tables (user_preferences, recommendation_cache, nft_features) on
    // the same pool. In prod the two DBs differ — if this fails the engine still
    // runs but recommendation features degrade silently.
    ensure_recommendation_tables(elixir_db.pool()).await;

    // Initialize Redis cache for recommendation engine (graceful degradation)
    let rec_cache = if let Some(ref redis_url) = config.recommendation.redis_url {
        info!("🔗 Connecting recommendation cache to Redis...");
        let cache = recommendation::cache::RecCache::connect(redis_url).await;
        if cache.is_some() {
            info!("✅ Recommendation Redis cache connected");
        } else {
            info!("⚠️ Recommendation Redis cache unavailable, using DB-only mode");
        }
        cache
    } else {
        info!("ℹ️ No REDIS_URL configured, recommendation cache disabled");
        None
    };

    // Initialize Nebula graph client (shares the same Redis cache layer).
    // FINAL-2 — pool transport is now the default.
    // `NEBULA_POOL=false` opts out to the subprocess-per-query transport (local
    // dev only — no Nebula required at startup).
    // Without an explicit opt-out, production always uses the persistent pool.
    let use_pool = std::env::var("NEBULA_POOL").as_deref() != Ok("false");
    let graph_client: Arc<dyn recommendation::graph_client::GraphTraversal> = if use_pool {
        info!("🔗 Nebula pool transport enabled (default; set NEBULA_POOL=false to use subprocess)");
        match recommendation::graph_client::GraphClient::from_env_pooled().await {
            Ok(gc) => {
                info!("✅ Nebula connection pool ready");
                Arc::new(gc.with_cache(rec_cache.clone()).with_dlq_pool(elixir_db.pool().clone()))
                    as Arc<dyn recommendation::graph_client::GraphTraversal>
            }
            Err(e) => {
                warn!("⚠️ Nebula pool failed to start ({e}), falling back to subprocess transport");
                Arc::new(recommendation::graph_client::GraphClient::new()
                    .with_cache(rec_cache.clone()).with_dlq_pool(elixir_db.pool().clone()))
                    as Arc<dyn recommendation::graph_client::GraphTraversal>
            }
        }
    } else {
        warn!("⚠️ NEBULA_POOL=false — using subprocess transport (dev mode, not for production)");
        Arc::new(recommendation::graph_client::GraphClient::new()
            .with_cache(rec_cache.clone()).with_dlq_pool(elixir_db.pool().clone()))
            as Arc<dyn recommendation::graph_client::GraphTraversal>
    };
    info!("✅ Nebula graph client initialized");

    // C4 / NASA-4: Schema version gate (hard).
    // Mismatched schema → startup fails unless NEBULA_SKIP_VERSION_CHECK=true
    // is set by an operator who explicitly acknowledges the risk.
    if let Err(e) = check_nebula_schema_version(&graph_client).await {
        if std::env::var("NEBULA_SKIP_VERSION_CHECK").as_deref() == Ok("true") {
            warn!(
                "⚠️  NEBULA_SKIP_VERSION_CHECK=true — proceeding despite schema mismatch: {}",
                e
            );
        } else {
            error!(
                "🚨 Startup aborted: {}. \
                 Set NEBULA_SKIP_VERSION_CHECK=true to override (at your own risk).",
                e
            );
            std::process::exit(1);
        }
    }

    // When Kafka is disabled wire up DirectHandlers so indexers can still write
    // preference signals (follows, likes, purchases) into the rec DB directly.
    let direct_handlers: Option<Arc<event_processor::DirectHandlers>> =
        if !config.kafka.enabled {
            info!("ℹ️ Kafka disabled — DirectHandlers wired for inline preference writes");
            Some(Arc::new(event_processor::DirectHandlers::new(
                db.pool().clone(),
                elixir_db.pool().clone(),
                rec_cache.clone(),
                Arc::clone(&graph_client),
            )))
        } else {
            None
        };

    // RS-03: shared TaskTracker for fire-and-forget graph write tasks.
    // task_tracker.close() + task_tracker.wait() in shutdown_services drains
    // any in-flight writes before the process exits.
    let task_tracker = Arc::new(tokio_util::task::TaskTracker::new());

    // Create shared state
    let state = Arc::new(AppState {
        config: config.clone(),
        db: db.clone(),
        elixir_db: elixir_db.clone(),
        kafka: kafka_producer.clone(),
        shutdown: shutdown_tx.clone(),
        rec_cache,
        graph_client,
        direct_handlers,
        task_tracker,
    });

    // Spawn all services
    let mut handles = Vec::new();

    // Spawn blockchain indexers
    info!("🔍 Starting blockchain indexers (friends + social)...");
    handles.extend(spawn_indexers(state.clone()));
    info!("✅ {} blockchain indexers started", 2);

    // Spawn recommendation score updater
    info!("📊 Starting recommendation score updater...");
    handles.push(spawn_score_updater(state.clone()));

    // Spawn Nebula edge reconciler (replays Postgres interactions after Nebula outages)
    info!("🔁 Starting Nebula edge reconciler...");
    handles.push(spawn_nebula_reconciler(state.clone()));

    // Spawn Nebula DLQ replayer (re-fires failed write edges every 15 minutes)
    info!("♻️  Starting Nebula DLQ replayer...");
    handles.push(spawn_nebula_dlq_replayer(state.clone()));

    // Spawn real-time event processor
    info!("🎯 Starting real-time event processor...");
    handles.push(event_processor::spawn_event_processor(state.clone()));

    // Initialize ERC-4337 bundler (optional — disabled if PRIVATE_KEY is not set)
    info!("🌿 Initializing ERC-4337 bundler...");
    let bundler_router = bundler::init().await;
    if bundler_router.is_some() {
        info!("✅ ERC-4337 Bundler ready (routes under /bundler/*)");
    }

    // Spawn API server
    info!("🌐 Starting API server on port {}...", config.api.port);
    handles.push(spawn_api_server(state.clone(), bundler_router));

    info!("═══════════════════════════════════════════════════════════════");
    info!("  ✅ All services started successfully");
    info!("  📡 API: http://{}:{}", config.api.host, config.api.port);
    info!(
        "  🔗 Health: http://{}:{}/health",
        config.api.host, config.api.port
    );
    info!("═══════════════════════════════════════════════════════════════");

    // Wait for shutdown signal or service failure
    tokio::select! {
        _ = shutdown_signal() => {
            info!("📴 Shutdown signal received");
        }
        _ = wait_for_any_failure(&mut handles) => {
            warn!("⚠️ A service failed, initiating shutdown");
        }
    }

    // Graceful shutdown
    info!("🛑 Initiating graceful shutdown...");

    // Signal all services to stop
    let _ = shutdown_tx.send(());

    // Wait for services to finish with timeout
    // RS-03: pass task_tracker so shutdown_services can drain graph write tasks.
    let shutdown_timeout = Duration::from_secs(60);
    if tokio::time::timeout(shutdown_timeout, shutdown_services(handles, state.task_tracker.clone()))
        .await
        .is_err()
    {
        warn!("Shutdown timeout exceeded, forcing exit");
    }

    // Cleanup resources
    kafka_producer.flush(Duration::from_secs(5));
    db.close().await;

    info!("👋 TheraGraph Engine stopped gracefully");
    Ok(())
}

/// Initialize structured logging with tracing
fn init_tracing() {
    // If KAFKA_RDKAFKA_DEBUG is set, enable more verbose rdkafka logs
    let filter = match std::env::var("KAFKA_RDKAFKA_DEBUG") {
        Ok(s) if !s.is_empty() => {
            // Honor RUST_LOG if provided, else include rdkafka=debug
            match std::env::var("RUST_LOG") {
                Ok(r) if !r.is_empty() => EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(&r)),
                _ => EnvFilter::new("theragraph_engine=debug,tower_http=debug,sqlx=warn,rdkafka=debug,info"),
            }
        }
        _ => EnvFilter::try_from_default_env().unwrap_or_else(|_| {
            // Default log levels
            EnvFilter::new("theragraph_engine=debug,tower_http=debug,sqlx=warn,rdkafka=warn,info")
        }),
    };

    tracing_subscriber::registry()
        .with(filter)
        .with(
            fmt::layer()
                .with_target(true)
                .with_thread_ids(false)
                .with_file(false)
                .with_line_number(false)
                .with_ansi(std::env::var("NO_COLOR").is_err()),
        )
        .init();
}

/// Spawn all blockchain indexers
///
/// indexer::friend and indexer::thera_friendz were, until this fix, BOTH spawned
/// here — they read the same contract address (state.config.contracts.thera_friendz),
/// use the identical event_parse_type() ("friends"), and differ only in their
/// cursor_type_name ("friend" vs "friends"). That meant every on-chain event on
/// this contract was decoded and processed twice, independently, from two separate
/// block cursors. Downstream, most writes happen to be idempotent (INSERT EDGE IF
/// NOT EXISTS, ON CONFLICT DO NOTHING on event_id) so this was mostly silent waste
/// — double the RPC calls, double the Kafka/direct-handler volume — but
/// event_processor::social's follow/unfollow affinity update
/// (`UPDATE ... SET x = x + 0.15` / `x - 0.2`) is NOT idempotent, so a duplicate
/// UserFollowed/UserUnfollowed genuinely double-applied the affinity delta.
/// indexer::friend's remaining code is already #[allow(dead_code)] elsewhere in
/// that module — nothing else depends on it running.
fn spawn_indexers(state: Arc<AppState>) -> Vec<tokio::task::JoinHandle<()>> {
    let mut handles = Vec::new();

    // TheraFriendz unified contract indexer
    let social_state = state.clone();
    handles.push(tokio::spawn(async move {
        if let Err(e) = indexer::thera_friendz::run_with_state(social_state).await {
            error!("Indexer 'thera_friendz' failed: {:?}", e);
        }
    }));

    handles
}

/// Spawn the recommendation score updater
fn spawn_score_updater(state: Arc<AppState>) -> tokio::task::JoinHandle<()> {
    let mut shutdown_rx = state.shutdown.subscribe();

    tokio::spawn(async move {
        let update_interval = state.config.recommendation.engagement_update_interval;
        let mut interval = tokio::time::interval(update_interval);

        // Skip first tick (runs immediately otherwise)
        interval.tick().await;

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    // TAG-S26-02: select_active_users queries likes/comments/purchases/follows/social_users
                    // which are all in the Elixir DB — not the rec DB. Using state.db caused every
                    // query to fail (relation not found) and the updater loop was permanently a no-op.
                    run_score_update_cycle(state.elixir_db.pool(), state.graph_client.clone(), state.rec_cache.clone()).await;
                }
                _ = shutdown_rx.recv() => {
                    info!("Score updater shutting down");
                    break;
                }
            }
        }
    })
}

/// Spawn the Nebula edge reconciler.
///
/// Runs every 6 hours with a 48-hour lookback window. Re-fires sync_like/sync_purchase/
/// sync_comment for every Postgres interaction in the window so edges lost during a
/// Nebula outage are recovered without manual intervention.
///
/// All sync_* calls are idempotent — re-writing an edge that already exists is a no-op
/// (sync_purchase uses IF NOT EXISTS; sync_like and sync_comment upsert by rank).
fn spawn_nebula_reconciler(state: Arc<AppState>) -> tokio::task::JoinHandle<()> {
    let mut shutdown_rx = state.shutdown.subscribe();

    tokio::spawn(async move {
        // Wait 5 minutes after startup: gives the Kafka consumer time to
        // catch up on recent blocks before the first reconciliation pass fires.
        tokio::time::sleep(Duration::from_secs(300)).await;

        let mut interval = tokio::time::interval(Duration::from_secs(6 * 3600));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    info!("🔁 Running Nebula edge reconciliation (48h lookback)...");
                    let dyn_client = recommendation::graph_client::GraphClient::from_dyn_traversal(
                        Arc::clone(&state.graph_client)
                    );
                    let graph_sync = event_processor::graph_sync::GraphSync::new(dyn_client);
                    match event_processor::reconciliation::reconcile_nebula_edges(
                        state.elixir_db.pool(),
                        &graph_sync,
                        48,
                    ).await {
                        Ok(stats) => info!(
                            "✅ Nebula reconciliation: likes={} purchases={} comments={} failed={}",
                            stats.likes_attempted, stats.purchases_attempted,
                            stats.comments_attempted, stats.total_failed
                        ),
                        Err(e) => error!("Nebula reconciliation failed: {e}"),
                    }
                }
                _ = shutdown_rx.recv() => {
                    info!("Nebula reconciler shutting down");
                    break;
                }
            }
        }
    })
}

/// Spawn the Nebula DLQ automated replayer.
///
/// Runs every 15 minutes. Re-fires the nGQL from `nebula_write_failures` rows
/// where `replayed_at IS NULL AND created_at > now() - 24h AND replay_count < 10`.
/// Marks successful rows with `replayed_at = now()`. After 10 failed attempts an
/// ERROR is emitted for manual triage (see RUNBOOK.md § 3).
///
/// All stored queries use IF NOT EXISTS / UPSERT — replaying is idempotent.
fn spawn_nebula_dlq_replayer(state: Arc<AppState>) -> tokio::task::JoinHandle<()> {
    let mut shutdown_rx = state.shutdown.subscribe();

    tokio::spawn(async move {
        // Jobs audit: run immediately on startup. If Nebula just came back after
        // an outage, the DLQ may have 50+ pending rows. Making operators wait
        // up to 17 minutes (2-min delay + 15-min interval) to see them replay
        // is bad UX. Run once at startup, then every 15 minutes.
        let run_once = async {
            match recommendation::graph_dlq::replay_pending(
                state.elixir_db.pool(),
                state.graph_client.as_ref(),
            ).await {
                Ok(stats) if stats.total > 0 => info!(
                    "♻️  DLQ startup replay: total={} replayed={} failed={}",
                    stats.total, stats.replayed, stats.failed
                ),
                Ok(_) => {}
                Err(e) => error!("DLQ replayer startup run failed: {e}"),
            }
        };
        run_once.await;

        let mut interval = tokio::time::interval(Duration::from_secs(15 * 60));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // Skip first tick since we just ran above.
        interval.tick().await;

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    match recommendation::graph_dlq::replay_pending(
                        state.elixir_db.pool(),
                        state.graph_client.as_ref(),
                    ).await {
                        Ok(stats) if stats.total > 0 => info!(
                            "♻️  DLQ replay: total={} replayed={} failed={}",
                            stats.total, stats.replayed, stats.failed
                        ),
                        Ok(_) => {}
                        Err(e) => error!("DLQ replayer query failed: {e}"),
                    }
                }
                _ = shutdown_rx.recv() => {
                    info!("DLQ replayer shutting down");
                    break;
                }
            }
        }
    })
}

/// One full score-update pass. Extracted so it can be tested and called independently.
async fn run_score_update_cycle(
    pool: &sqlx::PgPool,
    graph_client: Arc<dyn recommendation::graph_client::GraphTraversal>,
    rec_cache: Option<recommendation::cache::RecCache>,
) {
    info!("📊 Running scheduled score updates...");

    // These three write to disjoint tables — run concurrently.
    let (eng_res, trend_res, decay_res) = tokio::join!(
        recommendation::features::update_engagement_scores(pool),
        recommendation::features::update_trending_scores(pool),
        // CC-001: pass cache so stale Redis/PG recs invalidated after decay.
        recommendation::preferences::apply_preference_decay(pool, rec_cache.as_ref()),
    );
    if let Err(e) = eng_res   { error!("Failed to update engagement scores: {:?}", e); }
    if let Err(e) = trend_res { error!("Failed to update trending scores: {:?}", e); }
    if let Err(e) = decay_res { error!("Failed to apply preference decay: {:?}", e); }
    let update_engine = Arc::new(
        recommendation::engine::RecommendationEngine::new(pool.clone())
            .with_cache(rec_cache.clone())
            .with_graph_client(graph_client.clone()),
    );
    if let Err(e) = recommendation::updater::update_all_recommendations(update_engine, graph_client.clone()).await {
        error!("Failed to update user recommendations: {:?}", e);
    }
    // NASA-S20-02: prune recommended_to edges older than 30 days.
    if let Err(e) = recommendation::updater::prune_stale_recommended_to(graph_client.as_ref(), 30).await {
        error!("Failed to prune stale recommended_to edges: {:?}", e);
    }

    info!("✅ Score updates completed");
}

/// Ensure recommendation tables exist on the given pool.
/// Non-fatal: logs a warning and continues if migrations fail (prod DB isolation).
/// NASA-4: Read the `schema_meta` vertex from Nebula and compare its `version`
/// property to `NEBULA_SCHEMA_VERSION`.
///
/// Returns `Ok(())` when versions match or Nebula is unreachable (circuit open).
/// Returns `Err(...)` when the version is explicitly wrong — startup must abort
/// unless `NEBULA_SKIP_VERSION_CHECK=true` is set by a human operator.
///
/// Missing vertex (fresh cluster, `schema_meta` TAG not yet applied) → `Ok(())`
/// with a WARN so a first-boot without the TAG doesn't block startup.
async fn check_nebula_schema_version(
    graph: &Arc<dyn recommendation::graph_client::GraphTraversal>,
) -> anyhow::Result<()> {
    use recommendation::schema_consts::{parse_schema_version_table, NEBULA_SCHEMA_VERSION, SPACE_THERAGRAPH};

    let query = format!(
        "USE {space}; FETCH PROP ON schema_meta \"schema_meta\" YIELD properties(vertex).version AS version;",
        space = SPACE_THERAGRAPH,
    );

    let output = match graph.raw_write(&query).await {
        Ok(o) => o,
        Err(e) => {
            // Nebula unreachable (circuit open or not yet available): let the
            // circuit-breaker handle recovery. Don't block startup.
            warn!(
                "⚠️ Nebula schema version check skipped — Nebula unreachable: {}. \
                 Verify schema manually before serving traffic.",
                e
            );
            return Ok(());
        }
    };

    let nebula_version: Option<u32> = parse_schema_version_table(&output);

    match nebula_version {
        Some(v) if v == NEBULA_SCHEMA_VERSION => {
            info!(
                "✅ Nebula schema version {} matches expected (NEBULA_SCHEMA_VERSION={})",
                v, NEBULA_SCHEMA_VERSION
            );
            Ok(())
        }
        Some(v) => {
            anyhow::bail!(
                "Nebula schema version mismatch — DB has v{}, code expects v{}. \
                 Apply pending migrations in theragraph-nebula/init/ then restart. \
                 See RUNBOOK.md § 'Schema version check fires'.",
                v, NEBULA_SCHEMA_VERSION
            )
        }
        None => {
            // schema_meta vertex absent: fresh cluster or migration 16 not yet applied.
            // Warn but allow startup — operators can apply migration 16 online.
            warn!(
                "⚠️ schema_meta vertex not found in Nebula (expected version {}). \
                 Re-run init-entrypoint.sh to write it. \
                 Set NEBULA_SKIP_VERSION_CHECK=true to silence this warning.",
                NEBULA_SCHEMA_VERSION
            );
            Ok(())
        }
    }
}

async fn ensure_recommendation_tables(pool: &sqlx::PgPool) {
    info!("📦 Applying recommendation tables...");
    match database::run_migrations(pool).await {
        Ok(_) => info!("✅ Recommendation tables ensured"),
        Err(e) => warn!("⚠️ Could not apply recommendation migrations (non-fatal): {:?}", e),
    }
}

/// Spawn the API server
fn spawn_api_server(
    state: Arc<AppState>,
    bundler_router: Option<axum::Router>,
) -> tokio::task::JoinHandle<()> {
    let port = state.config.api.port;
    let cors_origins = state.config.api.cors_origins.clone();
    let pool = state.elixir_db.pool().clone();  // Use Elixir DB for NFT queries
    let rec_cache = state.rec_cache.clone();
    let graph_client = Arc::clone(&state.graph_client);
    let task_tracker = Arc::clone(&state.task_tracker);

    // RS-02: Axum's with_graceful_shutdown() handles the shutdown signal internally
    // inside start_server, so we no longer need a tokio::select! here to kill it.
    // The task simply runs until start_server returns (which happens when the OS
    // signal fires and all in-flight connections have drained).
    tokio::spawn(async move {
        if let Err(e) = api::start_server(pool, port, bundler_router, rec_cache, cors_origins, Some(graph_client), task_tracker).await {
            error!("API server error: {:?}", e);
        }
    })
}

/// Wait for any task to fail (async waker-based — no busy-poll)
async fn wait_for_any_failure(handles: &mut [tokio::task::JoinHandle<()>]) {
    if handles.is_empty() {
        return;
    }
    use std::pin::Pin;
    use std::task::Poll;
    futures::future::poll_fn(|cx| {
        for handle in handles.iter_mut() {
            if let Poll::Ready(_) = Pin::new(handle).poll(cx) {
                return Poll::Ready(());
            }
        }
        Poll::Pending
    })
    .await
}

/// Wait for all services to complete shutdown.
///
/// RS-03: drain tracked fire-and-forget tasks (graph writes) with a timeout
/// before joining service handles, so no Nebula writes are abandoned on exit.
async fn shutdown_services(
    handles: Vec<tokio::task::JoinHandle<()>>,
    task_tracker: Arc<tokio_util::task::TaskTracker>,
) {
    // ASYNC-001: join service handles FIRST so they stop spawning new fire-and-
    // forget graph-write tasks before we close the tracker.  Draining first (old
    // order) let services continue spawning new tasks *after* we had already
    // drained, so the final batch of writes was silently dropped.
    for handle in handles {
        // A handle that was already driven to completion by wait_for_any_failure
        // must not be awaited again — tokio panics "JoinHandle polled after completion".
        if !handle.is_finished() {
            let _ = handle.await;
        }
    }

    // Close the tracker so no new tasks can be spawned after this point.
    task_tracker.close();
    // Drain any outstanding graph-write tasks within the 30-second window.
    tokio::time::timeout(
        std::time::Duration::from_secs(30),
        task_tracker.wait(),
    )
    .await
    .unwrap_or_else(|_| {
        warn!("Timed out waiting for background graph writes to complete (>30s)");
    });
}

/// Wait for shutdown signal (SIGTERM or SIGINT)
///
/// PANIC-004 / PANIC-002: neither ctrl_c() nor signal() may panic at runtime —
/// degrade gracefully so the process keeps running and can still be shut down
/// via the other signal path.
async fn shutdown_signal() {
    // PANIC-004: ctrl_c returns Err only if the OS signal subsystem is broken;
    // log and continue rather than panicking.
    let ctrl_c = async {
        if let Err(e) = signal::ctrl_c().await {
            warn!("Ctrl-C signal error: {e}");
        }
    };

    // PANIC-002: signal() can return Err if the platform doesn't support the
    // signal (e.g. sandboxed env).  Fall back to pending so ctrl_c still works.
    #[cfg(unix)]
    let terminate = async {
        match signal::unix::signal(signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(e) => {
                warn!("SIGTERM handler unavailable: {e} — shutdown via Ctrl-C only");
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}
