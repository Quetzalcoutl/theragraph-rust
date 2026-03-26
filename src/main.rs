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

mod api;
mod boards;
mod bundler;
mod config;
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
}

#[tokio::main]
async fn main() -> Result<()> {
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

    // Also run recommendation migrations on the Elixir DB.
    // The API server uses elixir_db.pool() so that it can query the `nfts` table,
    // but the recommendation engine also needs its own tables (user_preferences,
    // recommendation_cache, nft_features) on that same pool.
    // In local dev both DATABASE_URL and ELIXIR_DATABASE_URL often point to the
    // same DB, so this is a no-op. In production they differ — hence the 500s.
    info!("📦 Applying recommendation tables to Elixir DB...");
    if let Err(e) = database::run_migrations(elixir_db.pool()).await {
        warn!("⚠️ Could not apply recommendation migrations to Elixir DB (non-fatal): {:?}", e);
    } else {
        info!("✅ Recommendation tables ensured on Elixir DB");
    }

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

    // Create shared state
    let state = Arc::new(AppState {
        config: config.clone(),
        db: db.clone(),
        elixir_db: elixir_db.clone(),
        kafka: kafka_producer.clone(),
        shutdown: shutdown_tx.clone(),
        rec_cache,
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
    let shutdown_timeout = Duration::from_secs(30);
    if tokio::time::timeout(shutdown_timeout, shutdown_services(handles))
        .await
        .is_err()
    {
        warn!("⚠️ Shutdown timeout exceeded, forcing exit");
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
fn spawn_indexers(state: Arc<AppState>) -> Vec<tokio::task::JoinHandle<()>> {
    let mut handles = Vec::new();
    // Spawn only active indexers: friends and thera_social (unified contract)
    let friend_state = state.clone();
    handles.push(tokio::spawn(async move {
        if let Err(e) = indexer::friend::run_with_state(friend_state).await {
            error!("Indexer 'friends' failed: {:?}", e);
        }
    }));

    // TheraFriends unified contract indexer
    let social_state = state.clone();
    handles.push(tokio::spawn(async move {
        if let Err(e) = indexer::thera_friends::run_with_state(social_state).await {
            error!("Indexer 'thera_friends' failed: {:?}", e);
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
                    info!("📊 Running scheduled score updates...");

                    let pool = state.db.pool();

                    if let Err(e) = recommendation::features::update_engagement_scores(pool).await {
                        error!("Failed to update engagement scores: {:?}", e);
                    }

                    if let Err(e) = recommendation::features::update_trending_scores(pool).await {
                        error!("Failed to update trending scores: {:?}", e);
                    }

                    if let Err(e) = recommendation::preferences::apply_preference_decay(pool).await {
                        error!("Failed to apply preference decay: {:?}", e);
                    }

                    // Generate personalized recommendations for active users
                    if let Err(e) = recommendation::updater::update_all_recommendations(pool, state.rec_cache.clone()).await {
                        error!("Failed to update user recommendations: {:?}", e);
                    }

                    info!("✅ Score updates completed");
                }
                _ = shutdown_rx.recv() => {
                    info!("Score updater shutting down");
                    break;
                }
            }
        }
    })
}

/// Spawn the API server
fn spawn_api_server(
    state: Arc<AppState>,
    bundler_router: Option<axum::Router>,
) -> tokio::task::JoinHandle<()> {
    let port = state.config.api.port;
    let pool = state.elixir_db.pool().clone();  // Use Elixir DB for NFT queries
    let rec_cache = state.rec_cache.clone();
    let mut shutdown_rx = state.shutdown.subscribe();

    tokio::spawn(async move {
        tokio::select! {
            result = api::start_server(pool, port, bundler_router, rec_cache) => {
                if let Err(e) = result {
                    error!("API server error: {:?}", e);
                }
            }
            _ = shutdown_rx.recv() => {
                info!("API server shutting down");
            }
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

/// Wait for all services to complete shutdown
async fn shutdown_services(handles: Vec<tokio::task::JoinHandle<()>>) {
    for handle in handles {
        let _ = handle.await;
    }
}

/// Wait for shutdown signal (SIGTERM or SIGINT)
async fn shutdown_signal() {
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("Failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("Failed to install signal handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}
