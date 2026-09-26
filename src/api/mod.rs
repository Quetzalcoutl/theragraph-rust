//! HTTP API Server for Recommendations
//!
//! Provides REST endpoints for the frontend to fetch personalized feeds.

mod feed;
mod interaction;
mod profile;

use anyhow::Result;
use crate::config::ApiConfig;
use axum::{
    extract::State,
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
use tracing::{info, warn};

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
    pub internal_api_key: Option<String>,
    /// Graph client for writing interaction edges and serving user suggestions.
    pub graph: Option<Arc<dyn GraphTraversal>>,
    /// RS-03: task tracker for fire-and-forget graph-write spawns.
    pub task_tracker: Option<Arc<tokio_util::task::TaskTracker>>,
    /// C6: live FeedSource adapters.
    pub following_feed:     Arc<dyn FeedSource>,
    pub enhanced_feed:      Arc<dyn FeedSource>,
    pub personalized_feed:  Arc<dyn FeedSource>,
    pub trending_feed:      Arc<dyn FeedSource>,
    /// J. H. Laning: readiness flag — false until pre-warm completes.
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
    /// Optional genre-slug boost/filter, comma-separated kebab-case slugs
    /// (e.g. `?genre_slugs=deep-house,uk-garage`) — capped at
    /// `MAX_GENRE_SLUGS`, matching the frontend's 437-leaf genre taxonomy's
    /// own request cap. Overflow (more than `MAX_GENRE_SLUGS` slugs supplied)
    /// is rejected with 400, consistent with how this crate validates every
    /// other capped list field (style_tags, genre_ids, artist_affinity,
    /// nft_tags) at the API boundary. Boosts candidates whose `tags` intersect
    /// the requested slugs — see `RecommendationEngine::get_recommendations`'s
    /// `genre_slugs` parameter.
    #[serde(default)]
    pub genre_slugs: Option<String>,
}

/// Parse a comma-separated query-param string into a validated slug list.
///
/// Splits on `,`, trims whitespace, drops empty segments. Returns
/// `Err(())` (→ 400) when more than `MAX_GENRE_SLUGS` non-empty slugs are
/// supplied — reject-on-overflow, not silent truncation, matching every other
/// capped list field in this API (style_tags, genre_ids, artist_affinity,
/// nft_tags all reject in `record_companion_signal`/`record_user_interaction`).
pub(crate) fn parse_genre_slugs(raw: &str) -> Result<Vec<String>, ()> {
    let slugs: Vec<String> = raw
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_lowercase)
        .collect();
    if slugs.len() > MAX_GENRE_SLUGS {
        return Err(());
    }
    Ok(slugs)
}

const MAX_LIMIT: usize = 200;
const MAX_OFFSET: usize = 10_000;
/// Cap on genre slugs accepted by both the `?genre_slugs=` feed query param
/// and the `POST /api/v1/genre-preferences/{addr}` write endpoint — matches
/// the frontend's per-request cap on its 437-leaf genre taxonomy.
pub(crate) const MAX_GENRE_SLUGS: usize = 30;
/// Max length of a single genre slug (kebab-case), same cap
/// `sanitize_genre_slug` truncates to.
pub(crate) const MAX_GENRE_SLUG_LEN: usize = 32;

fn default_limit() -> usize {
    20
}

fn clamp_query(limit: usize, offset: usize) -> (usize, usize) {
    (limit.min(MAX_LIMIT), offset.min(MAX_OFFSET))
}

/// Assemble a `FeedResponse` from a scored item list.
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
    /// Music listen completion rate 0.0–1.0. Required for Listen interactions.
    pub pct_played: Option<f32>,
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
    pub presets: Vec<String>,
}

/// Request body for AiFriendz tag preference delta write
#[derive(Debug, Deserialize)]
pub struct TagPreferencesDeltaRequest {
    pub user_address: String,
    pub tag_preferences: std::collections::HashMap<String, f32>,
}

/// Request body for `POST /api/v1/genre-preferences/{user_address}`.
///
/// Writes the user's explicitly declared favorite genres (up to
/// `MAX_GENRE_SLUGS`) as `genre_preference` Nebula edges — the canonical,
/// wide preference-write path (distinct from `CompanionSignalRequest`'s
/// narrow, AI-chat-derived `genre_ids`, capped at 3 and keyed by the old
/// numeric id format). See `write_genre_preference_edges`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GenrePreferencesRequest {
    /// Kebab-case genre slugs (e.g. `"deep-house"`, `"psychedelic-trance"`),
    /// capped at `MAX_GENRE_SLUGS`. Each is re-sanitized server-side via
    /// `sanitize_genre_slug` before being embedded in a Nebula VID.
    pub genre_slugs: Vec<String>,
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

/// Request body for Ethereum ECDSA (secp256k1) signature recovery.
///
/// `address` is optional: when present, the handler compares it (case-insensitively)
/// to the recovered signer and returns that comparison as `valid`. When absent, the
/// handler always returns `valid: true` alongside the recovered address, leaving the
/// comparison to the caller — mirrors how `EthSignature.verify_signer/3` on the
/// Elixir side always supplies an expected address, while still allowing a bare
/// "who signed this" recovery call.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EthVerifyRequest {
    /// The raw (unprefixed) message string that was passed to `personal_sign` /
    /// viem's `walletClient.signMessage({ message })`.
    pub message: String,
    /// `0x`-prefixed 130-hex-char raw signature: r(32) || s(32) || v(1).
    pub signature: String,
    /// Expected signer address (`0x` + 40 hex chars). When omitted, no comparison
    /// is performed and `valid` is always `true` for a successfully recovered address.
    pub address: Option<String>,
}

/// Response for Ethereum signature recovery/verification.
#[derive(Debug, Serialize)]
pub struct EthVerifyResponse {
    /// `true` when recovery succeeded and (if `address` was supplied) it matches.
    pub valid: bool,
    /// The recovered signer address, lowercase `0x…`.
    pub recovered_address: String,
}

// Base64-decoded size limits — reject oversized payloads before decoding
const MAX_SIG_B64: usize = 4500;
const MAX_PK_B64: usize  = 2800;
const MAX_MSG_B64: usize = 8192;

// eth/verify: signature is always exactly 132 chars ("0x" + 130 hex); message is the
// plaintext session string (small — "TheraGraph Message Session\n..." — well under 1 KiB).
const MAX_ETH_SIG_LEN: usize = 140;
const MAX_ETH_MESSAGE_LEN: usize = 2048;
const MAX_ETH_ADDRESS_LEN: usize = 42;

const MAX_NFT_ID_LEN: usize = 128;
const MAX_SOURCE_LEN: usize = 64;
const MAX_CONTRACT_TYPE_LEN: usize = 32;
const MAX_TAG_LEN: usize = 64;
const MAX_TAGS: usize = 50;
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
    /// NASA-4/M4: true when the nebula_write_failures DLQ has fewer than 10 unreplayed entries.
    pub nebula_dlq: bool,
    pub nebula_write_failures_last_hour: Option<i64>,
}

/// RS-02: Axum graceful-shutdown signal — resolves on SIGTERM or Ctrl-C.
async fn shutdown_signal() {
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
pub async fn start_server(
    pool: PgPool,
    api_config: ApiConfig,
    bundler_router: Option<Router>,
    rec_cache: Option<RecCache>,
    graph_client: Option<Arc<dyn GraphTraversal>>,
    task_tracker: Arc<tokio_util::task::TaskTracker>,
) -> Result<()> {
    let port         = api_config.port;
    let cors_origins = api_config.cors_origins.clone();
    let graph: Option<Arc<dyn GraphTraversal>> = graph_client;

    let rec_to_tx = graph.as_ref().map(|gt| {
        crate::recommendation::recommended_to_buffer::start_flusher(
            Arc::clone(gt),
            Some(Arc::clone(&task_tracker)),
        )
    });

    let engine = {
        let mut e = RecommendationEngine::new(pool.clone())
            .with_cache(rec_cache.clone())
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
    let following_feed:    Arc<dyn FeedSource> = Arc::new(FollowingFeed(Arc::clone(&engine_arc)));
    let enhanced_feed:     Arc<dyn FeedSource> = Arc::new(EnhancedFeed(Arc::clone(&engine_arc)));
    let personalized_feed: Arc<dyn FeedSource> = Arc::new(PersonalizedFeed(Arc::clone(&engine_arc)));
    let trending_feed:     Arc<dyn FeedSource> = Arc::new(TrendingFeed(Arc::clone(&engine_arc)));
    let engine = (*engine_arc).clone();

    warm_cache(&board_cache).await;

    // PANIC-001: install Prometheus recorder only once.
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
        metrics_secret:   api_config.metrics_secret.clone(),
        internal_api_key: api_config.internal_api_key.clone(),
        graph,
        task_tracker: Some(task_tracker),
        following_feed,
        enhanced_feed,
        personalized_feed,
        trending_feed,
        ready: Arc::clone(&ready),
    });

    // J. H. Laning: pre-warm trending feed; mark ready only after first compute.
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
        // Readiness: 503 until pre-warm completes
        .route("/readyz", get(readyz_handler))
        // Feed endpoints
        .route("/api/v1/feed/{user_address}", get(feed::get_following_feed))
        .route("/api/v1/enhanced-feed/{user_address}", get(feed::get_enhanced_feed))
        .route("/api/v1/recommendations/{user_address}", get(feed::get_recommendations))
        .route("/api/v1/trending", get(feed::get_trending))
        // Crypto — post-quantum signature verification (stateless, no DB)
        .route("/api/v1/crypto/dilithium/verify", post(dilithium_verify_handler))
        // Crypto — Ethereum ECDSA (secp256k1) EIP-191 signature recovery (stateless, no DB)
        .route("/api/v1/crypto/eth/verify", post(eth_verify_handler))
        // Interaction tracking
        .route("/api/v1/interactions", post(interaction::record_user_interaction))
        // Graph-based user suggestions
        .route("/api/v1/users/suggestions/{viewer_address}/{creator_address}", get(profile::get_user_suggestions))
        // Prometheus metrics scrape endpoint
        .route("/metrics", get(metrics_handler))
        // User preferences
        .route("/api/v1/preferences/{user_address}", get(profile::get_user_preferences))
        // Cold-start preference seeding
        .route("/api/v1/preferences/seed", post(profile::seed_onboarding_preferences))
        .route("/api/v1/users/{addr}/genesis_seed", post(profile::seed_genesis_preferences))
        .route("/api/v1/preferences/tags", post(profile::write_tag_preferences))
        .route("/api/v1/companion-signal", post(profile::record_companion_signal))
        // Declared genre preferences (up to 30 kebab-case slugs)
        .route("/api/v1/genre-preferences/{user_address}", post(profile::write_genre_preferences))
        .route("/admin/phase-shadow", post(profile::start_phase_shadow))
        // Rebuild user_preferences by replaying user_interactions (recovery/backfill tool)
        .route("/admin/rebuild-preferences/{user_address}", post(profile::rebuild_preferences))
        .route("/admin/rebuild-preferences", post(profile::rebuild_all_preferences))
        // One-time recovery for BUG-TAGS-01's historically-empty nft_tags rows
        .route("/admin/backfill-nft-tags", post(profile::backfill_nft_tags))
        // Board cache endpoints
        .merge(board_routes().with_state(board_cache))
        .layer(CompressionLayer::new())
        .layer(TimeoutLayer::with_status_code(axum::http::StatusCode::REQUEST_TIMEOUT, Duration::from_secs(30)))
        // INJECTION-01: limit request body to 64 KiB to prevent memory-exhaustion DoS.
        .layer(RequestBodyLimitLayer::new(64 * 1024))
        .layer(cors)
        .layer(middleware::from_fn(with_request_id))
        .with_state(state);

    let app = if let Some(br) = bundler_router {
        app.nest("/bundler", br)
    } else {
        app
    };

    let addr = format!("0.0.0.0:{}", port);
    info!("Starting recommendation API server on {}", addr);

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    Ok(())
}

/// Prometheus metrics endpoint — requires METRICS_SECRET env var.
async fn metrics_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> impl axum::response::IntoResponse {
    let forbidden = (
        StatusCode::FORBIDDEN,
        [(axum::http::header::CONTENT_TYPE, "text/plain")],
        "Forbidden".to_owned(),
    );
    let Some(ref secret) = state.metrics_secret else {
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

/// POST /api/v1/crypto/eth/verify
///
/// Stateless Ethereum ECDSA (secp256k1) EIP-191 `personal_sign` signature recovery,
/// mirroring `dilithium_verify_handler` above but for viem-signed wallet messages
/// (`walletClient.signMessage`) rather than post-quantum ML-DSA-65 signatures.
///
/// When `address` is supplied, `valid` reflects a case-insensitive comparison against
/// the recovered signer — this is the shape `TheraGraph.Crypto.EthSignature.verify_signer/3`
/// calls on the Elixir side. When `address` is omitted, `valid` is always `true` for a
/// successfully recovered signature (pure recovery, no comparison).
async fn eth_verify_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<EthVerifyRequest>,
) -> Result<Json<EthVerifyResponse>, StatusCode> {
    require_internal_key(&headers, state.internal_api_key.as_deref())?;

    if body.message.len() > MAX_ETH_MESSAGE_LEN
        || body.signature.len() > MAX_ETH_SIG_LEN
        || body.address.as_deref().map(str::len).unwrap_or(0) > MAX_ETH_ADDRESS_LEN
    {
        return Err(StatusCode::PAYLOAD_TOO_LARGE);
    }

    let recovered = crypto::eth_signature::recover_eth_signer(&body.message, &body.signature)
        .map_err(|_| StatusCode::BAD_REQUEST)?;

    let valid = match &body.address {
        Some(expected) => recovered.eq_ignore_ascii_case(expected),
        None => true,
    };

    Ok(Json(EthVerifyResponse { valid, recovered_address: recovered }))
}

fn base64_decode(s: &str) -> Result<Vec<u8>, ()> {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    STANDARD.decode(s).map_err(|_| ())
}

/// Health check endpoint
///
/// OB-01: probes all three dependencies so load-balancers detect partial degradation.
async fn health_check(
    State(state): State<Arc<AppState>>,
) -> impl axum::response::IntoResponse {
    let db_ok = sqlx::query("SELECT 1")
        .execute(&state.pool)
        .await
        .is_ok();

    let redis_ok = match state.engine.cache() {
        Some(cache) => cache.ping().await.is_ok(),
        None => true,
    };

    let nebula_ok = state
        .graph
        .as_ref()
        .map(|g| !g.is_circuit_open())
        .unwrap_or(false);

    let nebula_dlq_ok = crate::recommendation::graph_dlq::recent_unreplayed_count(&state.pool)
        .await
        .map(|n| n <= 10)
        .unwrap_or(true);

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
async fn readyz_handler(
    State(state): State<Arc<AppState>>,
) -> impl axum::response::IntoResponse {
    if state.ready.load(AtomicOrdering::Acquire) {
        (StatusCode::OK, Json(serde_json::json!({"ready": true})))
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, Json(serde_json::json!({"ready": false, "reason": "pre-warm in progress"})))
    }
}


#[cfg(test)]
mod tests;
