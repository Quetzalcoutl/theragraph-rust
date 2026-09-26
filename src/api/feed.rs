use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::Json,
};
use std::sync::Arc;
use tracing::{error, warn};

use crate::recommendation::{FeedSource, ScoredNft, types::ContentType};
use super::{
    AppState, FeedQuery, FeedResponse, RecommendationsQuery, RequestId,
    require_internal_key, is_valid_eth_address, build_feed_response, clamp_query, MAX_LIMIT,
    parse_genre_slugs,
};

/// Serve `primary` feed; on empty result or error, serve `fallback` instead.
///
/// Centralises the M3 fallback pattern used by get_following_feed and
/// get_enhanced_feed so the logic lives in one place rather than two copies.
pub(super) async fn feed_with_fallback(
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
pub(super) async fn get_following_feed(
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
pub(super) async fn get_enhanced_feed(
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
pub(super) async fn get_recommendations(
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

    // GENRE-02: comma-separated slug list, capped at MAX_GENRE_SLUGS (reject
    // overflow, not silent truncation — see `parse_genre_slugs`). Non-empty
    // genre_slugs still gets full cache-aside + stampede-lock coverage via
    // `get_recommendations_coalesced` (GENRE-02 follow-up) — keyed by a hash
    // of the slug set rather than by user_address alone, so it neither reads
    // a stale unfiltered result nor collides with a different genre
    // combination's cached result. See `RecommendationEngine::
    // get_recommendations_coalesced`'s doc comment for the key scheme.
    let genre_slugs: Vec<String> = match query.genre_slugs.as_deref() {
        Some(raw) => parse_genre_slugs(raw).map_err(|_| StatusCode::BAD_REQUEST)?,
        None => Vec::new(),
    };

    let result = state
        .engine
        .get_recommendations_coalesced(
            &user_address,
            limit,
            query.contract_type.as_deref(),
            query.exclude_seen,
            &genre_slugs,
        )
        .await;

    match result {
        Ok(items) => {
            // WIRE-01: mark every delivered recommendation as served=true so the
            // recommended_to edge feedback loop is closed at serve time.
            if let Some(ref graph) = state.graph {
                let graph = Arc::clone(graph);
                let addr = user_address.clone();
                let nft_ids: Vec<String> = items.iter().map(|s| s.nft_id.to_string()).collect();
                // S24-BATCH: single nGQL round-trip instead of N sequential subprocess spawns.
                let mark_fut = async move {
                    graph.mark_recommendations_served_batch(&addr, &nft_ids).await;
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
pub(super) async fn get_trending(
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
