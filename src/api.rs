//! HTTP API Server for Recommendations
//!
//! Provides REST endpoints for the frontend to fetch personalized feeds.

use anyhow::Result;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Json,
    routing::{get, post},
    Router,
};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::sync::Arc;
use std::time::Duration;
use tower_http::cors::{Any, CorsLayer};
use tower_http::compression::CompressionLayer;
use tower_http::timeout::TimeoutLayer;
use tracing::{error, info};

use crate::recommendation::{
    engine::RecommendationEngine,
    preferences::{record_interaction, InteractionEvent, InteractionType},
    ScoredNft,
};
use crate::boards::{BoardCacheState, board_routes, warm_cache};
use metrics_exporter_prometheus::PrometheusHandle;

use crate::recommendation::cache::RecCache;

/// Shared application state
#[allow(dead_code)]
pub struct AppState {
    pub pool: PgPool,
    pub engine: RecommendationEngine,
    pub board_cache: Arc<BoardCacheState>,
    pub metrics_handle: PrometheusHandle,
}

/// Query params for feed endpoints
#[derive(Debug, Deserialize)]
pub struct FeedQuery {
    #[serde(default = "default_limit")]
    pub limit: usize,
    #[serde(default)]
    pub offset: usize,
    pub contract_type: Option<String>,
}

/// Query params for recommendations endpoint
#[derive(Debug, Deserialize)]
pub struct RecommendationsQuery {
    #[serde(default = "default_limit")]
    pub limit: usize,
    pub contract_type: Option<String>,
    #[serde(default)]
    pub exclude_seen: bool,
}

fn default_limit() -> usize {
    20
}

/// Response for feed endpoints
#[derive(Debug, Serialize)]
pub struct FeedResponse {
    pub items: Vec<ScoredNft>,
    pub total: usize,
    pub has_more: bool,
}

/// Request body for recording interactions
#[derive(Debug, Deserialize)]
pub struct InteractionRequest {
    pub user_address: String,
    pub nft_id: String,
    pub interaction_type: String,
    pub view_duration_ms: Option<i64>,
    pub source: Option<String>,
    pub nft_contract_type: Option<String>,
    pub nft_creator_address: Option<String>,
    pub nft_tags: Option<Vec<String>>,
}

/// Health check response
#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub status: String,
    pub version: String,
}

/// Start the API server
pub async fn start_server(pool: PgPool, port: u16, bundler_router: Option<Router>, rec_cache: Option<RecCache>) -> Result<()> {
    let engine = RecommendationEngine::new(pool.clone()).with_cache(rec_cache);
    let board_cache = Arc::new(BoardCacheState::new(pool.clone()));

    // Warm board caches on startup
    warm_cache(&board_cache).await;

    // Install Prometheus recorder — must happen before any metrics! are recorded
    let metrics_handle = metrics_exporter_prometheus::PrometheusBuilder::new()
        .install_recorder()
        .expect("failed to install Prometheus recorder");

    let state = Arc::new(AppState { pool, engine, board_cache: board_cache.clone(), metrics_handle });

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let app = Router::new()
        // Health check
        .route("/health", get(health_check))
        // Feed endpoints
        .route("/api/v1/feed/:user_address", get(get_following_feed))
        .route(
            "/api/v1/enhanced-feed/:user_address",
            get(get_enhanced_feed),
        )
        .route(
            "/api/v1/recommendations/:user_address",
            get(get_recommendations),
        )
        .route("/api/v1/trending", get(get_trending))
        // Interaction tracking
        .route("/api/v1/interactions", post(record_user_interaction))
        // Prometheus metrics scrape endpoint
        .route("/metrics", get(metrics_handler))
        // User preferences
        .route(
            "/api/v1/preferences/:user_address",
            get(get_user_preferences),
        )
        // Board cache endpoints
        .merge(board_routes().with_state(board_cache))
        // Middleware: gzip/br/zstd response compression for large JSON payloads
        .layer(CompressionLayer::new())
        // Middleware: 30s timeout prevents hung requests from consuming resources
        .layer(TimeoutLayer::new(Duration::from_secs(30)))
        .layer(cors)
        .with_state(state);

    // Nest bundler routes under /bundler/* (if bundler is configured)
    // Done after .with_state() so both routers are Router<()>
    let app = if let Some(br) = bundler_router {
        app.nest("/bundler", br)
    } else {
        app
    };

    let addr = format!("0.0.0.0:{}", port);
    info!("🚀 Starting recommendation API server on {}", addr);

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

/// Prometheus metrics endpoint
async fn metrics_handler(State(state): State<Arc<AppState>>) -> impl axum::response::IntoResponse {
    let body = state.metrics_handle.render();
    (
        [(axum::http::header::CONTENT_TYPE, "text/plain; version=0.0.4")],
        body,
    )
}

/// Health check endpoint
async fn health_check() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "healthy".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
    })
}

/// Get following feed - NFTs from creators user follows
async fn get_following_feed(
    State(state): State<Arc<AppState>>,
    Path(user_address): Path<String>,
    Query(query): Query<FeedQuery>,
) -> Result<Json<FeedResponse>, StatusCode> {
    match state
        .engine
        .get_following_feed(&user_address, query.limit, query.offset)
        .await
    {
        Ok(items) => {
            let total = items.len();
            let has_more = total == query.limit;
            Ok(Json(FeedResponse {
                items,
                total,
                has_more,
            }))
        }
        Err(e) => {
            error!("Failed to get following feed: {:?}", e);
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

/// Get enhanced feed - personalized recommendations
async fn get_enhanced_feed(
    State(state): State<Arc<AppState>>,
    Path(user_address): Path<String>,
    Query(query): Query<FeedQuery>,
) -> Result<Json<FeedResponse>, StatusCode> {
    // Check cache first
    if let Ok(Some(cached)) = crate::recommendation::engine::get_cached_recommendations(
        &state.pool,
        &user_address,
        "enhanced",
    )
    .await
    {
        let total = cached.len();
        return Ok(Json(FeedResponse {
            items: cached
                .into_iter()
                .skip(query.offset)
                .take(query.limit)
                .collect(),
            total,
            has_more: total > query.offset + query.limit,
        }));
    }

    match state
        .engine
        .get_enhanced_feed(
            &user_address,
            query.limit,
            query.offset,
            query.contract_type.as_deref(),
        )
        .await
    {
        Ok(items) => {
            // Cache for 5 minutes
            let _ = crate::recommendation::engine::cache_recommendations(
                &state.pool,
                &user_address,
                "enhanced",
                &items,
                5,
            )
            .await;

            let total = items.len();
            let has_more = total == query.limit;
            Ok(Json(FeedResponse {
                items,
                total,
                has_more,
            }))
        }
        Err(e) => {
            error!("Failed to get enhanced feed: {:?}", e);
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

/// Get personalized recommendations for a user
async fn get_recommendations(
    State(state): State<Arc<AppState>>,
    Path(user_address): Path<String>,
    Query(query): Query<RecommendationsQuery>,
) -> Result<Json<FeedResponse>, StatusCode> {
    match state
        .engine
        .get_recommendations(
            &user_address,
            query.limit,
            query.contract_type.as_deref(),
            query.exclude_seen,
        )
        .await
    {
        Ok(items) => {
            let total = items.len();
            // For recommendations, we don't have a concept of "has_more" since it's personalized
            Ok(Json(FeedResponse {
                items,
                total,
                has_more: false,
            }))
        }
        Err(e) => {
            error!("Failed to get recommendations: {:?}", e);
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

/// Get trending NFTs
async fn get_trending(
    State(state): State<Arc<AppState>>,
    Query(query): Query<FeedQuery>,
) -> Result<Json<FeedResponse>, StatusCode> {
    // For trending, we use the engine with a generic "user" that has neutral preferences
    // This gives us engagement-weighted results
    match state
        .engine
        .get_enhanced_feed(
            "0x0000000000000000000000000000000000000000",
            query.limit,
            query.offset,
            query.contract_type.as_deref(),
        )
        .await
    {
        Ok(items) => {
            let total = items.len();
            let has_more = total == query.limit;
            Ok(Json(FeedResponse {
                items,
                total,
                has_more,
            }))
        }
        Err(e) => {
            error!("Failed to get trending: {:?}", e);
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

/// Record a user interaction
async fn record_user_interaction(
    State(state): State<Arc<AppState>>,
    Json(req): Json<InteractionRequest>,
) -> Result<StatusCode, StatusCode> {
    let interaction_type = match req.interaction_type.as_str() {
        "view" => InteractionType::View,
        "like" => InteractionType::Like,
        "unlike" => InteractionType::Unlike,
        "purchase" => InteractionType::Purchase,
        "share" => InteractionType::Share,
        "save" => InteractionType::Save,
        "comment" => InteractionType::Comment,
        "unsave" => InteractionType::Unsave,
        _ => return Err(StatusCode::BAD_REQUEST),
    };

    let event = InteractionEvent {
        user_address: req.user_address,
        nft_id: req.nft_id,
        interaction_type,
        view_duration_ms: req.view_duration_ms,
        source: req.source,
        nft_contract_type: req.nft_contract_type,
        nft_creator_address: req.nft_creator_address,
        nft_tags: req.nft_tags.unwrap_or_default(),
    };

    match record_interaction(&state.pool, event).await {
        Ok(_) => Ok(StatusCode::CREATED),
        Err(e) => {
            error!("Failed to record interaction: {:?}", e);
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

/// Get user preferences (for debugging/admin)
async fn get_user_preferences(
    State(state): State<Arc<AppState>>,
    Path(user_address): Path<String>,
) -> Result<Json<crate::recommendation::UserPreferences>, StatusCode> {
    match crate::recommendation::preferences::get_or_create_preferences(&state.pool, &user_address)
        .await
    {
        Ok(prefs) => Ok(Json(prefs)),
        Err(e) => {
            error!("Failed to get preferences: {:?}", e);
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}
