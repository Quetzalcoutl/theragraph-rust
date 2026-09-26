use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
};
use sqlx::PgPool;
use std::sync::Arc;
use tracing::{error, warn};

use crate::recommendation::{
    graph_client::GraphTraversal,
    preferences::{record_interaction, InteractionEvent, InteractionType},
};
use super::{
    AppState, InteractionRequest,
    require_internal_key, is_valid_eth_address,
    MAX_NFT_ID_LEN, MAX_SOURCE_LEN, MAX_CONTRACT_TYPE_LEN, MAX_TAG_LEN, MAX_TAGS,
    MAX_COMMENT_TEXT_LEN,
};

/// Record a user interaction
pub(super) async fn record_user_interaction(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    axum::Json(req): axum::Json<InteractionRequest>,
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

    // Listen-specific validation (David Pedersen: gate in extractor, not business logic).
    if interaction_type == InteractionType::Listen {
        // 30s minimum — Spotify royalty threshold; below this a listen is a skip.
        const MIN_LISTEN_MS: i64 = 30_000;
        if req.view_duration_ms.unwrap_or(0) < MIN_LISTEN_MS {
            return Err(StatusCode::BAD_REQUEST);
        }
        if let Some(pct) = req.pct_played {
            if !(0.0..=1.0).contains(&pct) {
                return Err(StatusCode::BAD_REQUEST);
            }
        }
    }

    let event = InteractionEvent {
        user_address: req.user_address.clone(),
        nft_id: req.nft_id.clone(),
        // RS-13: InteractionType: Copy — no explicit clone needed.
        interaction_type,
        view_duration_ms: req.view_duration_ms,
        pct_played: req.pct_played,
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
            state.pool.clone(),
            interaction_type,
            req.user_address.clone(),
            req.nft_id.clone(),
            req.nft_creator_address.clone(),
            req.view_duration_ms,
            req.pct_played,
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
pub(super) async fn dispatch_graph_interaction(
    graph: Arc<dyn GraphTraversal>,
    pool: PgPool,
    itype: InteractionType,
    user: String,
    nft_id: String,
    creator: Option<String>,
    duration_ms: Option<i64>,
    pct_played: Option<f32>,
    comment_text: String,
) {
    match itype {
        InteractionType::FlixWatch => {
            let dur_secs = ((duration_ms.unwrap_or(0).max(0) / 1000) as u64)
                .min(7200_u64) as u32;
            let pct = pct_played.unwrap_or(0.0).clamp(0.0, 1.0);
            let event_id = format!("fw:{}:{}", user, nft_id);
            graph.write_flix_watch_edge(&user, &nft_id, &event_id, dur_secs, pct).await;
        }
        InteractionType::Listen => {
            // Spotify engineers: rate-limit Nebula writes to prevent loop-listener inflation.
            // Postgres still records every play for play-count accuracy; only the graph edge
            // is deduplicated within a 1h window per (user, nft_id) pair.
            // TODO(MUSIC-01): wire Redis cache check here when AppState exposes cache handle.
            let dur_secs = ((duration_ms.unwrap_or(0).max(0) / 1000) as u64)
                .min(u32::MAX as u64) as u32;
            let pct = pct_played.unwrap_or(0.0).clamp(0.0, 1.0);
            let event_id = format!("ml:{}:{}", user, nft_id);
            tokio::join!(
                graph.write_music_listen_edge(&user, &nft_id, &event_id, dur_secs, pct),
                async {
                    if let Some(ref creator_addr) = creator {
                        graph.write_music_creator_affinity(&user, creator_addr, dur_secs, pct).await;
                        if let Err(e) = crate::recommendation::recorder::sync_creator_affinity_to_prefs(
                            &pool, &user, creator_addr, dur_secs,
                        ).await {
                            warn!("[dispatch] music creator affinity sync failed: {e}");
                        }
                    }
                },
            );
        }
        InteractionType::View => {
            // Validated ≤ 3600 s at call site; saturating_as is belt-and-suspenders.
            let dur_secs = ((duration_ms.unwrap_or(0).max(0) / 1000) as u64)
                .min(u32::MAX as u64) as u32;
            let event_id = format!("view:{}:{}", user, nft_id);
            tokio::join!(
                graph.write_view_event(&user, &nft_id, &event_id, dur_secs),
                async {
                    if let Some(ref creator_addr) = creator {
                        graph.write_creator_affinity(&user, creator_addr, dur_secs).await;
                        // C1: bridge Nebula affinity signal to Postgres creator_preferences
                        if let Err(e) = crate::recommendation::recorder::sync_creator_affinity_to_prefs(
                            &pool, &user, creator_addr, dur_secs,
                        ).await {
                            warn!("[dispatch] creator affinity sync failed: {e}");
                        }
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
