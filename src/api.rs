//! HTTP API Server for Recommendations
//!
//! Provides REST endpoints for the frontend to fetch personalized feeds.

use anyhow::Result;
use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{Json, Response},
    routing::{get, post},
    Router,
};
use axum::extract::Request;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::sync::{atomic::{AtomicBool, Ordering as AtomicOrdering}, Arc, OnceLock};
use std::time::Duration;
use axum::http::HeaderValue;
use tokio::signal;
use tower_http::cors::{Any, CorsLayer};
use tower_http::compression::CompressionLayer;
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::timeout::TimeoutLayer;
use tracing::{error, info, warn};

/// Typed request-ID wrapper stored as an Axum extension.
///
/// Extracted via `Extension<RequestId>` in handlers that need to record it
/// on the current tracing span.
#[derive(Clone, Debug)]
pub struct RequestId(pub String);

/// Axum middleware that attaches a `RequestId` to every request.
///
/// Reads `x-request-id` from incoming headers; falls back to a 12-character
/// random hex ID generated with `uuid::Uuid::new_v4()` when the header is
/// absent.  The value is stored as an `Extension<RequestId>` so any handler
/// in the chain can access it.
async fn with_request_id(mut req: Request, next: Next) -> Response {
    let id = req
        .headers()
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        // Sanitize to [0-9a-zA-Z_:.-]{1,64} before storing in tracing spans.
        // Unsanitized header values can contain newlines that poison SIEM log streams
        // when tracing outputs structured records with embedded request_id fields.
        .and_then(|s| {
            let clean: String = s
                .chars()
                .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | ':' | '.' | '-'))
                .take(64)
                .collect();
            if clean.is_empty() { None } else { Some(clean) }
        })
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()[..12].to_owned());
    req.extensions_mut().insert(RequestId(id));
    next.run(req).await
}

use crate::recommendation::{
    engine::RecommendationEngine,
    feedsource_impls::{PersonalizedFeed, FollowingFeed, TrendingFeed, EnhancedFeed},
    graph_client::GraphTraversal,
    preferences::{record_interaction, InteractionEvent, InteractionType},
    types::ContentType,
    FeedSource,
    ScoredNft,
};
use crate::boards::{BoardCacheState, board_routes, warm_cache};
use crate::address::EthAddress;
use crate::crypto;
use metrics_exporter_prometheus::PrometheusHandle;

use crate::recommendation::cache::RecCache;

/// Shared application state
#[allow(dead_code)]
pub struct AppState {
    pub pool: PgPool,
    pub engine: RecommendationEngine,
    pub board_cache: Arc<BoardCacheState>,
    pub metrics_handle: PrometheusHandle,
    /// TAG-S28-14: METRICS_SECRET read once at startup — no per-request env-var syscall.
    pub metrics_secret: Option<String>,
    /// TAG-S29-05: INTERNAL_API_KEY read once at startup alongside metrics_secret.
    /// Consistent with metrics_secret: both env-gate secrets live in AppState, not OnceLocks.
    pub internal_api_key: Option<String>,
    /// Graph client for writing interaction edges (view_event, creator_affinity, etc.)
    /// and serving user suggestions. Optional so the server starts even when Nebula
    /// is temporarily unreachable; graph writes degrade to no-ops.
    pub graph: Option<Arc<dyn GraphTraversal>>,
    /// RS-03: task tracker for fire-and-forget graph-write spawns.
    /// Using tracker.spawn() instead of tokio::spawn() lets shutdown_services
    /// drain all outstanding Nebula writes before the process exits.
    pub task_tracker: Option<Arc<tokio_util::task::TaskTracker>>,
    /// C6: live FeedSource adapters — dispatch through these instead of calling
    /// engine methods directly so new feed types require only a new adapter.
    pub following_feed:     Arc<dyn FeedSource>,
    pub enhanced_feed:      Arc<dyn FeedSource>,
    pub personalized_feed:  Arc<dyn FeedSource>,
    pub trending_feed:      Arc<dyn FeedSource>,
    /// J. H. Laning: readiness flag — false until pre-warm completes.
    /// /readyz returns 503 while false so the load balancer withholds traffic
    /// until the service has warm caches.
    pub ready: Arc<AtomicBool>,
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

const MAX_LIMIT: usize = 200;
const MAX_OFFSET: usize = 10_000;

fn default_limit() -> usize {
    20
}

fn clamp_query(limit: usize, offset: usize) -> (usize, usize) {
    (limit.min(MAX_LIMIT), offset.min(MAX_OFFSET))
}

/// Assemble a `FeedResponse` from a scored item list.
///
/// `has_more_override` — when `Some(v)` the value is used directly; when `None`
/// the field is inferred as `items.len() == limit` (i.e. the page filled up,
/// so there are probably more).
fn build_feed_response(items: Vec<ScoredNft>, limit: usize, has_more_override: Option<bool>) -> FeedResponse {
    let total = items.len();
    let has_more = has_more_override.unwrap_or(total == limit);
    FeedResponse { items, total, has_more }
}

fn is_valid_eth_address(addr: &str) -> bool {
    addr.parse::<EthAddress>().is_ok()
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
#[serde(deny_unknown_fields)]
pub struct InteractionRequest {
    pub user_address: String,
    pub nft_id: String,
    pub interaction_type: String,
    pub view_duration_ms: Option<i64>,
    pub source: Option<String>,
    pub nft_contract_type: Option<String>,
    pub nft_creator_address: Option<String>,
    pub nft_tags: Option<Vec<String>>,
    /// First 120 chars of comment text — used to write comments_on graph edge.
    pub comment_text: Option<String>,
}

/// Response for graph-based user suggestions.
#[derive(Debug, Serialize)]
pub struct UserSuggestionsResponse {
    pub suggestions: Vec<UserSuggestion>,
    pub source: String,
}

#[derive(Debug, Serialize)]
pub struct UserSuggestion {
    pub address: String,
    pub score: f64,
}

/// Request body for cold-start preference seeding (called at onboarding completion).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OnboardingSeedRequest {
    pub user_address: String,
    /// One or more preset IDs from the onboarding picker.
    /// Valid values: "art_lover", "music_fan", "movie_buff", "snap_creator", "collector".
    pub presets: Vec<String>,
}

/// Request body for ML-DSA-65 (FIPS 204) signature verification
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DilithiumVerifyRequest {
    /// Base64-encoded message that was signed (may be ciphertext)
    pub message: String,
    /// Base64-encoded detached signature (3293 raw bytes → 4392 base64)
    pub signature: String,
    /// Base64-encoded public key (1952 raw bytes → 2604 base64)
    pub public_key: String,
}

/// Response for Dilithium3 verification
#[derive(Debug, Serialize)]
pub struct DilithiumVerifyResponse {
    pub valid: bool,
}

// Base64-decoded size limits — reject oversized payloads before decoding
// ML-DSA-65 (FIPS 204): sig=3293 raw → 4392 base64; pk=1952 raw → 2604 base64
const MAX_SIG_B64: usize = 4500;   // 3293 raw → 4392 base64 + buffer
const MAX_PK_B64: usize  = 2800;   // 1952 raw → 2604 base64 + buffer
const MAX_MSG_B64: usize = 8192;   // 4 KB encrypted message + base64 overhead

const MAX_NFT_ID_LEN: usize = 128;
const MAX_SOURCE_LEN: usize = 64;
const MAX_CONTRACT_TYPE_LEN: usize = 32;
const MAX_TAG_LEN: usize = 64;
const MAX_TAGS: usize = 50;
// graph_client.rs truncates comment previews to 120 chars; reject anything
// beyond 500 at the API boundary so the caller knows their text was not saved.
const MAX_COMMENT_TEXT_LEN: usize = 500;

/// Health check response
#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub status: String,
    pub version: String,
    pub checks: HealthChecks,
}

#[derive(Debug, Serialize)]
pub struct HealthChecks {
    pub postgres: bool,
    pub redis: bool,
    pub nebula: bool,
    /// NASA-4/M4: true when the nebula_write_failures DLQ has accumulated fewer
    /// than 10 unreplayed entries in the last 5 minutes. false = elevated write
    /// failure rate; see RUNBOOK.md § 3.
    pub nebula_dlq: bool,
    /// Per-operation failure count in the last hour for the highest-volume write path.
    /// None when the query itself fails (DB unavailable). Operators use this to
    /// distinguish a single blip (low count, dlq=true) from sustained degradation.
    pub nebula_write_failures_last_hour: Option<i64>,
}

/// RS-02: Axum graceful-shutdown signal — resolves on SIGTERM or Ctrl-C.
///
/// When this future resolves, `axum::serve(...).with_graceful_shutdown(...)` stops
/// accepting new connections and waits for open connections to close. This prevents
/// in-flight HTTP requests from receiving a connection-reset on shutdown.
async fn shutdown_signal() {
    // PANIC-004 / PANIC-003: handle signal errors gracefully instead of panicking.
    let ctrl_c = async {
        if let Err(e) = signal::ctrl_c().await {
            warn!("Ctrl-C signal error: {e}");
        }
    };
    #[cfg(unix)]
    let terminate = async {
        match signal::unix::signal(signal::unix::SignalKind::terminate()) {
            Ok(mut s) => { s.recv().await; }
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

/// Start the API server
///
/// RS-02: uses `axum::serve(...).with_graceful_shutdown(shutdown_signal())` so
/// in-flight requests drain before the process exits.
/// RS-03: task_tracker stored in AppState for fire-and-forget graph writes.
pub async fn start_server(
    pool: PgPool,
    port: u16,
    bundler_router: Option<Router>,
    rec_cache: Option<RecCache>,
    cors_origins: Vec<String>,
    graph_client: Option<Arc<dyn GraphTraversal>>,
    task_tracker: Arc<tokio_util::task::TaskTracker>,
) -> Result<()> {
    // Use the shared graph Arc for both AppState.graph and the recommendation engine.
    let graph: Option<Arc<dyn GraphTraversal>> = graph_client;

    // Sherman Ye: start the recommended_to buffer flusher before the engine so
    // the engine can be wired to the sender at construction time.
    let rec_to_tx = graph.as_ref().map(|gt| {
        crate::recommendation::recommended_to_buffer::start_flusher(
            Arc::clone(gt),
            Some(Arc::clone(&task_tracker)),
        )
    });

    let engine = {
        let mut e = RecommendationEngine::new(pool.clone())
            .with_cache(rec_cache.clone())
            // RS-03: engine uses tracked spawns for recommended_to batch writes.
            .with_task_tracker(Arc::clone(&task_tracker));
        if let Some(ref gt) = graph {
            e = e.with_graph_client(Arc::clone(gt));
        }
        if let Some(tx) = rec_to_tx {
            e = e.with_recommended_to_sender(tx);
        }
        e
    };

    let board_cache = Arc::new(BoardCacheState::with_cache(pool.clone(), rec_cache.clone()));

    let engine_arc = Arc::new(engine);
    // Jobs audit: names now match what the adapters actually do.
    // EnhancedFeed  → get_enhanced_feed (personalized, per-user)
    // TrendingFeed  → get_enhanced_feed(0x0) (popularity-based, all users)
    // Previously: TrendingFeed was used for enhanced_feed, CachedFeed for trending
    // (CachedFeed returned empty for the zero address — trending was silently broken).
    let following_feed:    Arc<dyn FeedSource> = Arc::new(FollowingFeed(Arc::clone(&engine_arc)));
    let enhanced_feed:     Arc<dyn FeedSource> = Arc::new(EnhancedFeed(Arc::clone(&engine_arc)));
    let personalized_feed: Arc<dyn FeedSource> = Arc::new(PersonalizedFeed(Arc::clone(&engine_arc)));
    let trending_feed:     Arc<dyn FeedSource> = Arc::new(TrendingFeed(Arc::clone(&engine_arc)));
    // Unwrap Arc<RecommendationEngine> back into the value field.
    // Arc::try_unwrap fails only if there are other Arcs alive — the four adapters
    // hold their own clones, so the original engine_arc still has one reference.
    // Use Arc::clone so the engine_arc itself stays alive (the adapters hold clones).
    let engine = (*engine_arc).clone();

    // Warm board caches on startup
    warm_cache(&board_cache).await;

    // PANIC-001: install Prometheus recorder only once (guard against double-init).
    // install_recorder() returns Err if called again in the same process (e.g. in
    // integration tests that call start_server() twice). OnceLock ensures we only
    // register the global recorder once and reuse the handle on subsequent calls.
    // Note: `get_or_try_init` is behind `once_cell_try` (still unstable). Use
    // `get_or_init` with an explicit expect — equivalent behaviour for startup.
    static METRICS_HANDLE: OnceLock<PrometheusHandle> = OnceLock::new();
    let metrics_handle = METRICS_HANDLE.get_or_init(|| {
        metrics_exporter_prometheus::PrometheusBuilder::new()
            .install_recorder()
            .expect("failed to install Prometheus recorder")
    }).clone();

    let ready = Arc::new(AtomicBool::new(false));

    let state = Arc::new(AppState {
        pool,
        engine,
        board_cache: board_cache.clone(),
        metrics_handle,
        metrics_secret: std::env::var("METRICS_SECRET").ok(),
        internal_api_key: std::env::var("INTERNAL_API_KEY").ok(),
        graph,
        task_tracker: Some(task_tracker),
        following_feed,
        enhanced_feed,
        personalized_feed,
        trending_feed,
        ready: Arc::clone(&ready),
    });

    // J. H. Laning: pre-warm trending feed to populate Redis and PG cache.
    // Mark ready only after the first trending compute so the load balancer
    // sees a warm service. Uses a detached spawn so startup does not block
    // accepting connections (the first /readyz check gates traffic instead).
    {
        let trending = Arc::clone(&state.trending_feed);
        let ready_flag = Arc::clone(&ready);
        tokio::spawn(async move {
            match trending.candidates("0x0", 100, None).await {
                Ok(_) => {
                    ready_flag.store(true, AtomicOrdering::Release);
                    info!("pre-warm complete — service is ready (/readyz → 200)");
                }
                Err(e) => {
                    // Pre-warm failure is non-fatal: mark ready anyway so the
                    // service doesn't stay unroutable indefinitely on a cold DB.
                    ready_flag.store(true, AtomicOrdering::Release);
                    warn!("pre-warm failed ({e:?}), marking ready anyway to avoid perpetual 503");
                }
            }
        });
    }

    let cors = if cors_origins.iter().any(|o| o == "*") {
        CorsLayer::new().allow_origin(Any).allow_methods(Any).allow_headers(Any)
    } else {
        let origins: Vec<HeaderValue> = cors_origins
            .iter()
            .filter_map(|o| o.parse().ok())
            .collect();
        CorsLayer::new().allow_origin(origins).allow_methods(Any).allow_headers(Any)
    };

    let app = Router::new()
        // Liveness: always 200 if the process is up
        .route("/health", get(health_check))
        // Readiness: 503 until pre-warm completes; load balancer gates traffic here
        .route("/readyz", get(readyz_handler))
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
        // Crypto — post-quantum signature verification (stateless, no DB)
        .route("/api/v1/crypto/dilithium/verify", post(dilithium_verify_handler))
        // Interaction tracking
        .route("/api/v1/interactions", post(record_user_interaction))
        // Graph-based user suggestions: who to follow when viewing a creator's profile
        .route("/api/v1/users/suggestions/:viewer_address/:creator_address", get(get_user_suggestions))
        // Prometheus metrics scrape endpoint
        .route("/metrics", get(metrics_handler))
        // User preferences
        .route(
            "/api/v1/preferences/:user_address",
            get(get_user_preferences),
        )
        // Cold-start preference seeding — called once at onboarding completion
        .route("/api/v1/preferences/seed", post(seed_onboarding_preferences))
        // Board cache endpoints
        .merge(board_routes().with_state(board_cache))
        // Middleware: gzip/br/zstd response compression for large JSON payloads
        .layer(CompressionLayer::new())
        // Middleware: 30s timeout prevents hung requests from consuming resources
        .layer(TimeoutLayer::new(Duration::from_secs(30)))
        // INJECTION-01: limit request body to 64 KiB to prevent memory-exhaustion DoS.
        // All POST handlers have field-length guards, but deserialization runs first —
        // this layer rejects oversized bodies before any handler code runs.
        .layer(RequestBodyLimitLayer::new(64 * 1024))
        .layer(cors)
        // Middleware: attach x-request-id (or generate one) as Extension<RequestId>
        // Applied last so it runs first on the way in; all handlers can access it.
        .layer(middleware::from_fn(with_request_id))
        .with_state(state);

    // Nest bundler routes under /bundler/* (if bundler is configured)
    // Done after .with_state() so both routers are Router<()>
    let app = if let Some(br) = bundler_router {
        app.nest("/bundler", br)
    } else {
        app
    };

    let addr = format!("0.0.0.0:{}", port);
    info!("Starting recommendation API server on {}", addr);

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    // RS-02: with_graceful_shutdown drains in-flight connections instead of
    // dropping the TcpListener immediately when the shutdown signal fires.
    // Without this, Axum closes the listener the moment the Future is dropped,
    // sending a connection-reset to any in-flight HTTP request.
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    Ok(())
}

/// Prometheus metrics endpoint — requires METRICS_SECRET env var.
/// Returns 403 if secret is absent, not configured, or wrong.
async fn metrics_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> impl axum::response::IntoResponse {
    let forbidden = (
        StatusCode::FORBIDDEN,
        [(axum::http::header::CONTENT_TYPE, "text/plain")],
        "Forbidden".to_string(),
    );
    // TAG-S28-14: use the secret cached in AppState (read once at startup)
    // rather than calling std::env::var on every request.
    let Some(ref secret) = state.metrics_secret else {
        // Deny by default when secret is not configured — fail closed.
        return forbidden;
    };
    let provided = headers
        .get("x-metrics-secret")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    use subtle::ConstantTimeEq;
    if !bool::from(provided.as_bytes().ct_eq(secret.as_bytes())) {
        return forbidden;
    }
    let body = state.metrics_handle.render();
    (
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "text/plain; version=0.0.4")],
        body,
    )
}

/// Check `x-api-key` header against the expected key stored in `AppState`.
/// Returns `Err(StatusCode::UNAUTHORIZED)` when key missing or wrong.
/// If `INTERNAL_API_KEY` is not configured the call is denied (fail-closed).
fn require_internal_key(headers: &HeaderMap, expected: Option<&str>) -> Result<(), StatusCode> {
    let expected = match expected {
        Some(k) => k,
        None => return Err(StatusCode::UNAUTHORIZED),
    };
    let provided = headers
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    use subtle::ConstantTimeEq;
    if bool::from(provided.as_bytes().ct_eq(expected.as_bytes())) {
        Ok(())
    } else {
        Err(StatusCode::UNAUTHORIZED)
    }
}

/// POST /api/v1/crypto/dilithium/verify
///
/// Stateless Dilithium3 (ML-DSA-65) signature verification.
/// No database access — pure computation, ~0.7 ms per call.
/// Inputs are base64-encoded; size limits enforced before decoding.
/// INJECTION-02: requires internal API key — CPU-bound op, rate-limiting via key.
async fn dilithium_verify_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<DilithiumVerifyRequest>,
) -> Result<Json<DilithiumVerifyResponse>, StatusCode> {
    require_internal_key(&headers, state.internal_api_key.as_deref())?;
    if body.message.len() > MAX_MSG_B64 || body.signature.len() > MAX_SIG_B64 || body.public_key.len() > MAX_PK_B64 {
        return Err(StatusCode::PAYLOAD_TOO_LARGE);
    }

    let message = base64_decode(&body.message).map_err(|_| StatusCode::BAD_REQUEST)?;
    let sig     = base64_decode(&body.signature).map_err(|_| StatusCode::BAD_REQUEST)?;
    let pk      = base64_decode(&body.public_key).map_err(|_| StatusCode::BAD_REQUEST)?;

    let valid = crypto::dilithium::verify(&message, &sig, &pk);
    Ok(Json(DilithiumVerifyResponse { valid }))
}

fn base64_decode(s: &str) -> Result<Vec<u8>, ()> {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    STANDARD.decode(s).map_err(|_| ())
}

/// Health check endpoint
///
/// OB-01: probes all three dependencies so load-balancers detect partial degradation.
/// - postgres: critical (service unavailable if down)
/// - redis: soft dependency (degraded but functional if down)
/// - nebula: soft dependency (circuit open = degraded, not dead)
async fn health_check(
    State(state): State<Arc<AppState>>,
) -> impl axum::response::IntoResponse {
    let db_ok = sqlx::query("SELECT 1")
        .execute(&state.pool)
        .await
        .is_ok();

    let redis_ok = match state.engine.cache() {
        Some(cache) => cache.ping().await.is_ok(),
        None => true, // not configured — not a failure mode
    };

    let nebula_ok = state
        .graph
        .as_ref()
        .map(|g| !g.is_circuit_open())
        .unwrap_or(false);

    // M4: DLQ health — check recent unreplayed failure count.
    // Threshold: ≤ 10 unreplayed entries in the last 5 min = healthy.
    // Query is best-effort; failure returns true (don't degrade health on DB hiccup).
    let nebula_dlq_ok = crate::recommendation::graph_dlq::recent_unreplayed_count(&state.pool)
        .await
        .map(|n| n <= 10)
        .unwrap_or(true);

    // Wire failure_count_last_hour for the highest-volume write path ("upsert_edge").
    // None when the DLQ query itself fails — operators can distinguish "no failures"
    // (Some(0)) from "can't reach DB" (None) in monitoring dashboards.
    let nebula_write_failures_last_hour =
        crate::recommendation::graph_dlq::failure_count_last_hour(&state.pool, "upsert_edge").await;

    let checks = HealthChecks {
        postgres: db_ok,
        redis: redis_ok,
        nebula: nebula_ok,
        nebula_dlq: nebula_dlq_ok,
        nebula_write_failures_last_hour,
    };
    let all_ok = db_ok;
    let status = if all_ok { "healthy" } else { "unhealthy" };
    let code = if all_ok { StatusCode::OK } else { StatusCode::SERVICE_UNAVAILABLE };
    (code, Json(HealthResponse { status: status.to_string(), version: env!("CARGO_PKG_VERSION").to_string(), checks }))
}

/// J. H. Laning: readiness endpoint — 503 until pre-warm completes.
///
/// Load balancers should poll `/readyz` and only route traffic after 200.
/// Unlike `/health` (which checks live dependencies), `/readyz` reflects
/// whether the service has warm caches and is ready to serve production traffic.
async fn readyz_handler(
    State(state): State<Arc<AppState>>,
) -> impl axum::response::IntoResponse {
    if state.ready.load(AtomicOrdering::Acquire) {
        (StatusCode::OK, Json(serde_json::json!({"ready": true})))
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, Json(serde_json::json!({"ready": false, "reason": "pre-warm in progress"})))
    }
}

/// Serve `primary` feed; on empty result or error, serve `fallback` instead.
///
/// Centralises the M3 fallback pattern used by get_following_feed and
/// get_enhanced_feed so the logic lives in one place rather than two copies.
async fn feed_with_fallback(
    primary: &dyn FeedSource,
    fallback: &dyn FeedSource,
    user_address: &str,
    limit: usize,
    contract_type: Option<&str>,
) -> Vec<ScoredNft> {
    match primary.candidates(user_address, limit, contract_type).await {
        Ok(items) if !items.is_empty() => items,
        Ok(_) => {
            warn!("{} empty for {user_address} — returning trending fallback", primary.name());
            // TrendingFeed ignores user_address — passing it through avoids a
            // hardcoded zero-address literal here while keeping the same behaviour.
            fallback
                .candidates(user_address, limit, contract_type)
                .await
                .unwrap_or_default()
        }
        Err(e) => {
            warn!("{} failed for {user_address} ({e:?}) — returning trending fallback", primary.name());
            fallback
                .candidates(user_address, limit, contract_type)
                .await
                .unwrap_or_default()
        }
    }
}

/// Get following feed - NFTs from creators user follows
#[tracing::instrument(skip(state), fields(request_id, address = %user_address))]
async fn get_following_feed(
    State(state): State<Arc<AppState>>,
    axum::Extension(req_id): axum::Extension<RequestId>,
    headers: HeaderMap,
    Path(user_address): Path<String>,
    Query(query): Query<FeedQuery>,
) -> Result<Json<FeedResponse>, StatusCode> {
    // BUG-003: require internal key — these endpoints return per-user personalized data.
    require_internal_key(&headers, state.internal_api_key.as_deref())?;
    tracing::Span::current().record("request_id", req_id.0.as_str());
    if !is_valid_eth_address(&user_address) {
        return Err(StatusCode::BAD_REQUEST);
    }
    let (limit, _offset) = clamp_query(query.limit, query.offset);
    // M3: fallback chain — following → trending. A DB hiccup never returns 500.
    let items = feed_with_fallback(
        state.following_feed.as_ref(),
        state.trending_feed.as_ref(),
        &user_address,
        limit,
        query.contract_type.as_deref(),
    ).await;
    Ok(Json(build_feed_response(items, limit, None)))
}

/// Get enhanced feed - personalized recommendations
#[tracing::instrument(skip(state), fields(request_id, address = %user_address))]
async fn get_enhanced_feed(
    State(state): State<Arc<AppState>>,
    axum::Extension(req_id): axum::Extension<RequestId>,
    headers: HeaderMap,
    Path(user_address): Path<String>,
    Query(query): Query<FeedQuery>,
) -> Result<Json<FeedResponse>, StatusCode> {
    // BUG-003: require internal key — personalized per-user data.
    require_internal_key(&headers, state.internal_api_key.as_deref())?;
    tracing::Span::current().record("request_id", req_id.0.as_str());
    if !is_valid_eth_address(&user_address) {
        return Err(StatusCode::BAD_REQUEST);
    }
    if let Some(ref ct) = query.contract_type {
        if ContentType::from_str(ct).is_none() {
            return Err(StatusCode::BAD_REQUEST);
        }
    }
    // FeedSource is a single-page interface — offset is not threaded through
    // the FeedSource trait. The cache layer handles position-0 warm reads;
    // pagination is a future concern tracked separately.
    let (limit, _) = clamp_query(query.limit, query.offset);

    // M3: fallback chain — enhanced → trending. Score-engine failures never return 500.
    let items = feed_with_fallback(
        state.enhanced_feed.as_ref(),
        state.trending_feed.as_ref(),
        &user_address,
        limit,
        query.contract_type.as_deref(),
    ).await;
    Ok(Json(build_feed_response(items, limit, None)))
}

/// Get personalized recommendations for a user
#[tracing::instrument(skip(state), fields(request_id, address = %user_address))]
async fn get_recommendations(
    State(state): State<Arc<AppState>>,
    axum::Extension(req_id): axum::Extension<RequestId>,
    headers: HeaderMap,
    Path(user_address): Path<String>,
    Query(query): Query<RecommendationsQuery>,
) -> Result<Json<FeedResponse>, StatusCode> {
    // BUG-003: require internal key — personalized per-user data.
    require_internal_key(&headers, state.internal_api_key.as_deref())?;
    tracing::Span::current().record("request_id", req_id.0.as_str());
    if !is_valid_eth_address(&user_address) {
        return Err(StatusCode::BAD_REQUEST);
    }
    if let Some(ref ct) = query.contract_type {
        if ContentType::from_str(ct).is_none() {
            return Err(StatusCode::BAD_REQUEST);
        }
    }
    let limit = query.limit.min(MAX_LIMIT);

    match state
        .engine
        .get_recommendations_coalesced(
            &user_address,
            limit,
            query.contract_type.as_deref(),
            query.exclude_seen,
        )
        .await
    {
        Ok(items) => {
            // WIRE-01: mark every delivered recommendation as served=true so the
            // recommended_to edge feedback loop is closed at serve time.  The engine
            // already wrote these edges with served=false inside get_recommendations;
            // flipping them here confirms the NFTs were actually shown to the user.
            // Fires in a background task — never delays the HTTP response.
            if let Some(ref graph) = state.graph {
                let graph = Arc::clone(graph);
                let addr = user_address.clone();
                let nft_ids: Vec<String> = items.iter().map(|s| s.nft_id.clone()).collect();
                // S24-BATCH: single nGQL round-trip instead of N sequential subprocess spawns.
                let mark_fut = async move {
                    let ids: Vec<&str> = nft_ids.iter().map(String::as_str).collect();
                    graph.mark_recommendations_served_batch(&addr, &ids).await;
                };
                if let Some(ref tracker) = state.task_tracker {
                    tracker.spawn(mark_fut);
                } else {
                    tokio::spawn(mark_fut);
                }
            }
            Ok(Json(build_feed_response(items, limit, Some(false))))
        }
        Err(e) => {
            error!("Failed to get recommendations: {:?}", e);
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

/// Get trending NFTs
///
/// RT-01: Intentionally unauthenticated — returns the same public feed for all callers.
/// No user-specific data is returned. If personalisation is ever added here,
/// require_internal_key must be added before merging.
#[tracing::instrument(skip(state), fields(request_id))]
async fn get_trending(
    State(state): State<Arc<AppState>>,
    axum::Extension(req_id): axum::Extension<RequestId>,
    Query(query): Query<FeedQuery>,
) -> Result<Json<FeedResponse>, StatusCode> {
    tracing::Span::current().record("request_id", req_id.0.as_str());
    if let Some(ref ct) = query.contract_type {
        if ContentType::from_str(ct).is_none() {
            return Err(StatusCode::BAD_REQUEST);
        }
    }
    let (limit, _offset) = clamp_query(query.limit, 0);
    match state.trending_feed.candidates(
        "0x0000000000000000000000000000000000000000",
        limit,
        query.contract_type.as_deref(),
    ).await
    {
        Ok(items) => Ok(Json(build_feed_response(items, limit, None))),
        Err(e) => {
            error!("Failed to get trending: {:?}", e);
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

/// Record a user interaction
async fn record_user_interaction(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<InteractionRequest>,
) -> Result<StatusCode, StatusCode> {
    require_internal_key(&headers, state.internal_api_key.as_deref())?;
    // Validate required address fields
    if !is_valid_eth_address(&req.user_address) {
        return Err(StatusCode::BAD_REQUEST);
    }
    if let Some(ref creator) = req.nft_creator_address {
        if !is_valid_eth_address(creator) {
            return Err(StatusCode::BAD_REQUEST);
        }
    }
    // Reject negative durations and cap at 1 hour. Without the upper bound,
    // a large i64 literal (valid JSON) causes a silent as-u32 wrap after / 1000,
    // writing a fabricated dwell-time of decades onto the Nebula view_event edge
    // and inflating FoF scores for any content the caller views — exploitable by
    // any holder of INTERNAL_API_KEY.
    const MAX_VIEW_DURATION_MS: i64 = 3_600_000;
    if req.view_duration_ms.map(|d| d < 0 || d > MAX_VIEW_DURATION_MS).unwrap_or(false) {
        return Err(StatusCode::BAD_REQUEST);
    }

    // TAG-S27-12: validate UUID format before accepting nft_id to prevent garbage
    // from reaching user_interactions / nft_features lookups.
    uuid::Uuid::parse_str(&req.nft_id).map_err(|_| StatusCode::BAD_REQUEST)?;

    // Clamp string field lengths to prevent oversized DB writes
    if req.nft_id.len() > MAX_NFT_ID_LEN {
        return Err(StatusCode::BAD_REQUEST);
    }
    if req.source.as_deref().map(|s| s.len()).unwrap_or(0) > MAX_SOURCE_LEN {
        return Err(StatusCode::BAD_REQUEST);
    }
    if req.nft_contract_type.as_deref().map(|s| s.len()).unwrap_or(0) > MAX_CONTRACT_TYPE_LEN {
        return Err(StatusCode::BAD_REQUEST);
    }
    if let Some(ref tags) = req.nft_tags {
        if tags.len() > MAX_TAGS || tags.iter().any(|t| t.len() > MAX_TAG_LEN) {
            return Err(StatusCode::BAD_REQUEST);
        }
    }
    if req.comment_text.as_deref().map(|s| s.len()).unwrap_or(0) > MAX_COMMENT_TEXT_LEN {
        return Err(StatusCode::BAD_REQUEST);
    }

    let interaction_type = req.interaction_type.parse::<InteractionType>()
        .map_err(|_| StatusCode::BAD_REQUEST)?;

    let event = InteractionEvent {
        user_address: req.user_address.clone(),
        nft_id: req.nft_id.clone(),
        // RS-13: InteractionType: Copy — no explicit clone needed.
        interaction_type,
        view_duration_ms: req.view_duration_ms,
        source: req.source.clone(),
        nft_contract_type: req.nft_contract_type.clone(),
        nft_creator_address: req.nft_creator_address.clone(),
        nft_tags: req.nft_tags.clone().unwrap_or_default(),
        tag_enrichment: Default::default(),
        // API interactions are not replayed; no dedup key needed here.
        event_id: None,
    };

    // Fire graph edge writes in the background — never blocks the response.
    // RS-03: use task_tracker.spawn() when available so shutdown can drain these
    // writes before the process exits (prevents orphaned Nebula writes).
    if let Some(ref graph) = state.graph {
        let write_fut = dispatch_graph_interaction(
            Arc::clone(graph),
            interaction_type,
            req.user_address.clone(),
            req.nft_id.clone(),
            req.nft_creator_address.clone(),
            req.view_duration_ms,
            req.comment_text.clone().unwrap_or_default(),
        );
        if let Some(ref tracker) = state.task_tracker {
            tracker.spawn(write_fut);
        } else {
            tokio::spawn(write_fut);
        }
    }

    match record_interaction(&state.pool, event, state.engine.cache()).await {
        Ok(_) => Ok(StatusCode::CREATED),
        Err(e) => {
            error!("Failed to record interaction: {:?}", e);
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

/// Dispatch graph edge writes for a single interaction.
///
/// B-01: extracted from record_user_interaction so the mapping from InteractionType
/// to GraphTraversal calls is testable in isolation. Each arm uses a deterministic
/// event_id so retries are idempotent (NEBULA-006, S30-07).
async fn dispatch_graph_interaction(
    graph: Arc<dyn GraphTraversal>,
    itype: InteractionType,
    user: String,
    nft_id: String,
    creator: Option<String>,
    duration_ms: Option<i64>,
    comment_text: String,
) {
    match itype {
        InteractionType::View => {
            // Validated ≤ 3600 s at call site; saturating_as is belt-and-suspenders
            // so a future code-path change never silently wraps to a huge u32.
            let dur_secs = ((duration_ms.unwrap_or(0).max(0) / 1000) as u64)
                .min(u32::MAX as u64) as u32;
            let event_id = format!("view:{}:{}", user, nft_id);
            tokio::join!(
                graph.write_view_event(&user, &nft_id, &event_id, dur_secs),
                async {
                    if let Some(ref creator_addr) = creator {
                        graph.write_creator_affinity(&user, creator_addr, dur_secs).await;
                    }
                },
            );
        }
        InteractionType::Comment => {
            let event_id = format!("cmt:{}:{}", user, nft_id);
            graph.write_comments_on(&user, &nft_id, &event_id, &comment_text).await;
        }
        // WIRE-01 / WIRE-03: write likes edge AND mark recommendation served.
        InteractionType::Like => {
            let event_id = format!("lk:{}:{}", user, nft_id);
            tokio::join!(
                graph.write_likes_edge(&user, &nft_id, &event_id, "like"),
                graph.mark_recommendation_served(&user, &nft_id),
            );
        }
        // WIRE-03: purchase closes the feedback loop AND writes the purchases edge.
        // BUG-NEW-01 fixed: deterministic event_id prevents double-write when both the
        // API path and the Kafka/event-processor path (graph_sync::sync_purchase) fire
        // for the same purchase. UPSERT EDGE is idempotent, but a stable key ensures
        // the ON CONFLICT dedup in user_interactions also fires correctly.
        InteractionType::Purchase => {
            let event_id = format!("pur:{}:{}", user, nft_id);
            tokio::join!(
                graph.write_purchases_edge(&user, &nft_id, &event_id),
                graph.mark_recommendation_served(&user, &nft_id),
            );
        }
        // S30-05: Save/Unsave fell to `_ => {}` before this extraction.
        InteractionType::Save => {
            let event_id = format!("bm:{}:{}", user, nft_id);
            graph.write_bookmark_edge(&user, &nft_id, &event_id).await;
        }
        InteractionType::Unsave => {
            graph.delete_bookmark_edge(&user, &nft_id).await;
        }
        _ => {}
    }
}

/// Get user preferences (internal/admin only)
async fn get_user_preferences(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(user_address): Path<String>,
) -> Result<Json<crate::recommendation::UserPreferences>, StatusCode> {
    require_internal_key(&headers, state.internal_api_key.as_deref())?;
    if !is_valid_eth_address(&user_address) {
        return Err(StatusCode::BAD_REQUEST);
    }
    match crate::recommendation::preferences::get_or_create_preferences(
        &state.pool,
        state.engine.cache(),
        &user_address,
    )
    .await
    {
        Ok(prefs) => Ok(Json(prefs)),
        Err(e) => {
            error!("Failed to get preferences: {:?}", e);
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

/// Graph-walked "Who to Follow" suggestions for the profile page.
///
/// Returns up to 20 users from the creator's audience graph that the viewer
/// isn't already following. Score = number of the creator's viewers who follow
/// the suggested user — a proxy for "well-known in this community."
async fn get_user_suggestions(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((viewer_address, creator_address)): Path<(String, String)>,
) -> Result<Json<UserSuggestionsResponse>, StatusCode> {
    require_internal_key(&headers, state.internal_api_key.as_deref())?;
    if !is_valid_eth_address(&viewer_address) || !is_valid_eth_address(&creator_address) {
        return Err(StatusCode::BAD_REQUEST);
    }

    if let Some(ref graph) = state.graph {
        let suggestions = graph
            .get_viewer_based_user_suggestions(&viewer_address, &creator_address, 20)
            .await;

        if !suggestions.is_empty() {
            return Ok(Json(UserSuggestionsResponse {
                suggestions: suggestions
                    .into_iter()
                    .map(|(addr, score)| UserSuggestion {
                        address: addr.trim_start_matches("user:").to_string(),
                        score,
                    })
                    .collect(),
                source: "graph".to_string(),
            }));
        }
    }

    // Fallback: empty list — callers should degrade to PostgreSQL-based suggestions.
    Ok(Json(UserSuggestionsResponse {
        suggestions: vec![],
        source: "fallback".to_string(),
    }))
}

/// POST /api/v1/preferences/seed
///
/// Seeds initial tag preferences and content-type affinities from onboarding preset
/// selections. Only writes values that are still at the neutral default (≤ 0.5),
/// so interaction data accumulated before onboarding completes is never overwritten.
/// Protected by INTERNAL_API_KEY.
async fn seed_onboarding_preferences(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<OnboardingSeedRequest>,
) -> Result<StatusCode, StatusCode> {
    require_internal_key(&headers, state.internal_api_key.as_deref())?;
    if !is_valid_eth_address(&req.user_address) {
        return Err(StatusCode::BAD_REQUEST);
    }
    if req.presets.is_empty() || req.presets.len() > 10 {
        return Err(StatusCode::BAD_REQUEST);
    }
    // Reject unknown preset names at the API boundary — the vocabulary is fixed and
    // any string outside it would reach seed_from_presets as an unknown key, silently
    // producing no-op writes or surfacing in future log format! calls as untrusted input.
    const VALID_PRESETS: &[&str] = &[
        "art_lover", "music_fan", "movie_buff", "snap_creator", "collector",
    ];
    if req.presets.iter().any(|p| !VALID_PRESETS.contains(&p.as_str())) {
        return Err(StatusCode::BAD_REQUEST);
    }

    // Cache invalidation is handled inside seed_from_presets (FIX seed-prefs-cache-stale).
    crate::recommendation::preferences::seed_from_presets(
        &state.pool,
        state.engine.cache(),
        &req.user_address,
        &req.presets,
    )
    .await
    .map_err(|e| {
        error!("Failed to seed onboarding preferences: {:?}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recommendation::scoring::{RecommendationReason, ScoredNft};

    fn make_scored_nft(id: &str) -> ScoredNft {
        ScoredNft {
            nft_id: id.to_owned(),
            token_id: 1,
            contract_address: "0xdeadbeef".to_owned(),
            score: 0.9,
            reason: RecommendationReason::Discovery,
            contract_type: "ERC721".to_owned(),
            creator_address: "0xcafe".to_owned(),
            tags: vec![],
        }
    }

    // ── build_feed_response ───────────────────────────────────────────────────

    #[test]
    fn empty_items_returns_zero_total_and_no_more() {
        let resp = build_feed_response(vec![], 20, None);
        assert_eq!(resp.total, 0);
        assert!(resp.items.is_empty());
        // 0 items < limit=20, so has_more inferred false
        assert!(!resp.has_more);
    }

    #[test]
    fn non_empty_items_count_matches_and_items_preserved() {
        let items: Vec<ScoredNft> = (0..3).map(|i| make_scored_nft(&format!("nft-{i}"))).collect();
        let resp = build_feed_response(items, 20, None);
        assert_eq!(resp.total, 3);
        assert_eq!(resp.items.len(), 3);
        assert_eq!(resp.items[0].nft_id, "nft-0");
        assert_eq!(resp.items[2].nft_id, "nft-2");
    }

    #[test]
    fn full_page_infers_has_more_true() {
        // When returned count == limit, has_more should be true (page filled up).
        let items: Vec<ScoredNft> = (0..5).map(|i| make_scored_nft(&format!("n{i}"))).collect();
        let resp = build_feed_response(items, 5, None);
        assert_eq!(resp.total, 5);
        assert!(resp.has_more, "full page should infer has_more=true");
    }

    #[test]
    fn partial_page_infers_has_more_false() {
        let items: Vec<ScoredNft> = (0..3).map(|i| make_scored_nft(&format!("n{i}"))).collect();
        let resp = build_feed_response(items, 5, None);
        assert!(!resp.has_more, "partial page should infer has_more=false");
    }

    #[test]
    fn has_more_override_true_overrides_inferred_value() {
        // Even with a partial page, override forces has_more=true.
        let items: Vec<ScoredNft> = (0..2).map(|i| make_scored_nft(&format!("n{i}"))).collect();
        let resp = build_feed_response(items, 20, Some(true));
        assert!(resp.has_more);
    }

    #[test]
    fn has_more_override_false_overrides_inferred_value() {
        // Full page but override forces has_more=false (used by following feed).
        let items: Vec<ScoredNft> = (0..5).map(|i| make_scored_nft(&format!("n{i}"))).collect();
        let resp = build_feed_response(items, 5, Some(false));
        assert!(!resp.has_more);
    }

    #[test]
    fn response_total_reflects_actual_item_count_not_limit() {
        // total is items.len(), not the limit parameter.
        let items: Vec<ScoredNft> = (0..7).map(|i| make_scored_nft(&format!("n{i}"))).collect();
        let resp = build_feed_response(items, 20, None);
        assert_eq!(resp.total, 7);
    }
}
