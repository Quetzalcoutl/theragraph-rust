use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::Json,
};
use serde::Deserialize;
use std::sync::Arc;
use tracing::{error, info, warn};

use crate::recommendation::{
    model::EurekaPayload,
    preferences::{seed_from_genesis_eureka, write_tag_preferences_delta},
    schema_consts::{vid_user, ensure_user_vertex_nql, SPACE_THERAGRAPH},
};
use super::{
    AppState, UserSuggestionsResponse, UserSuggestion, OnboardingSeedRequest,
    TagPreferencesDeltaRequest, GenrePreferencesRequest, require_internal_key,
    is_valid_eth_address, MAX_GENRE_SLUGS, MAX_GENRE_SLUG_LEN,
};

/// Companion signal payload from AiFriendz Puter AI extraction.
///
/// AiFriendz fires this at session end when it detects musical preference language.
/// Puter AI does the extraction — Rust receives structured tags, never raw text.
/// Signed with COMPANION_SIGNAL_SECRET (checked via require_internal_key).
#[derive(Debug, Deserialize)]
pub(super) struct CompanionSignalRequest {
    user_address: String,
    companion_id: String,
    /// Always "music_preference" for now; extensible for future signal types.
    signal_type: String,
    /// Style texture tags: ["melancholic", "late-night", "energetic"].
    #[serde(default)]
    style_tags: Vec<String>,
    /// Atomic genre IDs from the genre taxonomy.
    #[serde(default)]
    genre_ids: Vec<i32>,
    /// Creator addresses the companion detected affinity toward.
    #[serde(default)]
    artist_affinity: Vec<String>,
    /// 0.0–1.0. Explicit mention = 0.8–1.0, inferred mood = 0.4–0.6.
    confidence: f32,
    /// Optional: "morning", "afternoon", "night".
    #[serde(default)]
    time_context: String,
    /// Sanitized paraphrase for For You attribution copy. NOT raw conversation text.
    #[serde(default)]
    #[allow(dead_code)]
    source_phrase: String,
}

/// Get user preferences (internal/admin only)
pub(super) async fn get_user_preferences(
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
pub(super) async fn get_user_suggestions(
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
                        address: addr.trim_start_matches("user:").to_owned(),
                        score,
                    })
                    .collect(),
                source: "graph".to_owned(),
            }));
        }
    }

    // Fallback: empty list — callers should degrade to PostgreSQL-based suggestions.
    Ok(Json(UserSuggestionsResponse {
        suggestions: vec![],
        source: "fallback".to_owned(),
    }))
}

/// POST /api/v1/preferences/seed
///
/// Seeds initial tag preferences and content-type affinities from onboarding preset
/// selections. Only writes values that are still at the neutral default (≤ 0.5),
/// so interaction data accumulated before onboarding completes is never overwritten.
/// Protected by INTERNAL_API_KEY.
pub(super) async fn seed_onboarding_preferences(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    axum::Json(req): axum::Json<OnboardingSeedRequest>,
) -> Result<StatusCode, StatusCode> {
    require_internal_key(&headers, state.internal_api_key.as_deref())?;
    if !is_valid_eth_address(&req.user_address) {
        return Err(StatusCode::BAD_REQUEST);
    }
    if req.presets.is_empty() || req.presets.len() > 10 {
        return Err(StatusCode::BAD_REQUEST);
    }
    // Reject unknown preset names at the API boundary — the vocabulary is fixed and
    // any string outside it would reach seed_from_presets as an unknown key.
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

    // Ensure the user vertex exists in Nebula so future edge writes have a valid src VID.
    if let Some(ref graph) = state.graph {
        let graph = Arc::clone(graph);
        let addr = req.user_address.clone();
        tokio::spawn(async move {
            let vid = vid_user(&addr);
            let query = format!("USE {space}; {upsert}",
                space = SPACE_THERAGRAPH,
                upsert = ensure_user_vertex_nql(&vid, &addr),
            );
            if let Err(e) = graph.raw_write(&query).await {
                warn!("[seed_onboarding] user vertex upsert failed (CB open?): {e}");
            }
        });
    }

    Ok(StatusCode::NO_CONTENT)
}

/// Full-fidelity genesis seeding from DarkChamber eureka payload.
/// Auth: internal API key (called from Elixir GenesisSeedController).
pub(super) async fn seed_genesis_preferences(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(addr): Path<String>,
    axum::Json(payload): axum::Json<EurekaPayload>,
) -> Result<impl axum::response::IntoResponse, StatusCode> {
    require_internal_key(&headers, state.internal_api_key.as_deref())?;
    if !is_valid_eth_address(&addr) {
        return Err(StatusCode::BAD_REQUEST);
    }

    let result = seed_from_genesis_eureka(
        &state.pool,
        state.engine.cache(),
        &addr,
        payload,
    )
    .await
    .map_err(|e| {
        error!("Genesis seeding failed for {addr}: {:?}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let alignment_level = if result.alignment_watch { "watch" } else { "good" };

    Ok(Json(serde_json::json!({
        "seeded": true,
        "tags_seeded": result.tags_seeded,
        "creators_seeded": result.creators_seeded,
        "alignment_level": alignment_level,
        "cold_start_complete": true,
    })))
}

/// Write AiFriendz tag preference deltas to PG.
/// Auth: internal API key (called from Elixir AiSignals context).
pub(super) async fn write_tag_preferences(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    axum::Json(req): axum::Json<TagPreferencesDeltaRequest>,
) -> Result<StatusCode, StatusCode> {
    require_internal_key(&headers, state.internal_api_key.as_deref())?;
    if !is_valid_eth_address(&req.user_address) {
        return Err(StatusCode::BAD_REQUEST);
    }

    write_tag_preferences_delta(
        &state.pool,
        state.engine.cache(),
        &req.user_address,
        req.tag_preferences,
    )
    .await
    .map_err(|e| {
        warn!("write_tag_preferences_delta failed for {}: {:?}", req.user_address, e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    Ok(StatusCode::NO_CONTENT)
}

/// POST /api/v1/companion-signal
///
/// Writes style/genre preference edges to Nebula for companion-extracted music taste.
/// Auth: internal API key (Puter AI service signs with COMPANION_SIGNAL_SECRET).
pub(super) async fn record_companion_signal(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    axum::Json(req): axum::Json<CompanionSignalRequest>,
) -> Result<StatusCode, StatusCode> {
    require_internal_key(&headers, state.internal_api_key.as_deref())?;

    if !is_valid_eth_address(&req.user_address) {
        return Err(StatusCode::BAD_REQUEST);
    }
    if req.signal_type != "music_preference" {
        return Err(StatusCode::BAD_REQUEST);
    }
    if req.companion_id.is_empty() || req.companion_id.len() > 64 {
        return Err(StatusCode::BAD_REQUEST);
    }
    if !(0.0..=1.0).contains(&req.confidence) {
        return Err(StatusCode::BAD_REQUEST);
    }
    // style_tags: max 5, each max 32 chars, alphanumeric + hyphens only
    if req.style_tags.len() > 5
        || req.style_tags.iter().any(|t| t.len() > 32 || !t.chars().all(|c| c.is_alphanumeric() || c == '-'))
    {
        return Err(StatusCode::BAD_REQUEST);
    }
    if req.genre_ids.len() > 3 {
        return Err(StatusCode::BAD_REQUEST);
    }
    if req.artist_affinity.len() > 5 || req.artist_affinity.iter().any(|a| !is_valid_eth_address(a)) {
        return Err(StatusCode::BAD_REQUEST);
    }

    if let Some(ref graph) = state.graph {
        let graph = Arc::clone(graph);
        let user = req.user_address.clone();
        let style_tags = req.style_tags.clone();
        let genre_ids = req.genre_ids.clone();
        let confidence = req.confidence;
        let time_context = req.time_context.clone();
        tokio::spawn(async move {
            graph.write_companion_preference_edges(
                &user,
                &style_tags,
                &genre_ids,
                confidence,
                &time_context,
            ).await;
        });
    }

    Ok(StatusCode::CREATED)
}

/// POST /api/v1/genre-preferences/{user_address}
///
/// Writes a user's explicitly declared favorite genres (up to
/// `MAX_GENRE_SLUGS` kebab-case slugs) as `genre_preference` Nebula edges —
/// the canonical, wide preference-write path (see `GenrePreferencesRequest`
/// doc comment for how this differs from `CompanionSignalRequest.genre_ids`).
/// Auth: internal API key.
pub(super) async fn write_genre_preferences(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(user_address): Path<String>,
    axum::Json(req): axum::Json<GenrePreferencesRequest>,
) -> Result<StatusCode, StatusCode> {
    require_internal_key(&headers, state.internal_api_key.as_deref())?;
    if !is_valid_eth_address(&user_address) {
        return Err(StatusCode::BAD_REQUEST);
    }
    // Reject-on-overflow (not silent truncation) and strict kebab-case
    // validation, matching CompanionSignalRequest.style_tags' validation
    // style just above — the frontend's taxonomy already emits lowercase
    // kebab-case slugs, so anything else indicates a contract mismatch worth
    // surfacing as 400 rather than silently dropping.
    if req.genre_slugs.len() > MAX_GENRE_SLUGS
        || req.genre_slugs.iter().any(|s| {
            s.is_empty()
                || s.len() > MAX_GENRE_SLUG_LEN
                || !s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        })
    {
        return Err(StatusCode::BAD_REQUEST);
    }

    if let Some(ref graph) = state.graph {
        // GENRE-02 follow-up: capture the OLD declared genre set before it's
        // overwritten below, so the genre-filtered *recommendations* cache
        // scoped to that old set can be invalidated too — otherwise a user
        // who just edited their favorites could keep seeing results scored
        // against their previous genre list until
        // GENRE_ONDEMAND_TTL_MINS/GENRE_PREWARM_TTL_MINS lapses (see
        // `RecommendationEngine::invalidate_genre_feed`). This is on top of,
        // not instead of, the existing genre-preference read-cache
        // invalidation below.
        let old_prefs = graph.get_genre_preferences(&user_address).await;
        if !old_prefs.is_empty() {
            let old_slugs: Vec<String> = old_prefs.keys().map(|s| s.to_string()).collect();
            state.engine.invalidate_genre_feed(&user_address, &old_slugs).await;
        }

        let graph = Arc::clone(graph);
        let addr = user_address.clone();
        let genre_slugs = req.genre_slugs.clone();
        tokio::spawn(async move {
            graph.write_genre_preference_edges(&addr, &genre_slugs).await;
        });
    }

    // Invalidate the request-path genre-preference read cache so the next
    // feed request picks up the new declaration within this call's lifetime
    // rather than waiting out GENRE_PREFS_TTL.
    if let Some(cache) = state.engine.cache() {
        cache.delete_genre_preferences(&user_address).await;
    }

    Ok(StatusCode::CREATED)
}

/// Phase shadow mode trigger — Elixir PhasePromoterWorker calls this when
/// a user count threshold is crossed.
pub(super) async fn start_phase_shadow(
    State(_state): State<Arc<AppState>>,
    headers: HeaderMap,
    axum::Json(body): axum::Json<serde_json::Value>,
) -> Result<StatusCode, StatusCode> {
    // Admin endpoint — internal API key required
    let key = headers
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if key.is_empty() {
        return Err(StatusCode::UNAUTHORIZED);
    }

    let next_phase = body
        .get("next_phase")
        .and_then(|v| v.as_u64())
        .ok_or(StatusCode::BAD_REQUEST)?;

    info!("🔭 Phase shadow mode requested for phase {}", next_phase);
    // Shadow mode spawning is handled by the engine on the next scoring cycle.

    Ok(StatusCode::ACCEPTED)
}

/// Rebuild one user's `user_preferences` row from scratch by replaying their
/// full `user_interactions` history — recovery path for BUG-TAGS-01 (the
/// nfts.tags-that-never-existed bug that left tag_preferences learning zero
/// real signal for every interaction until it was fixed) and the general
/// "user_preferences is a derived, always-recomputable view over the
/// permanent user_interactions log" insurance policy.
pub(super) async fn rebuild_preferences(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(user_address): Path<String>,
) -> Result<Json<crate::recommendation::UserPreferences>, StatusCode> {
    require_internal_key(&headers, state.internal_api_key.as_deref())?;
    if !is_valid_eth_address(&user_address) {
        return Err(StatusCode::BAD_REQUEST);
    }
    match crate::recommendation::preferences::rebuild_user_preferences(
        &state.pool,
        state.engine.cache(),
        &user_address,
    )
    .await
    {
        Ok(prefs) => Ok(Json(prefs)),
        Err(e) => {
            error!("rebuild_preferences failed for {user_address}: {e:?}");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

/// Rebuild every user with at least one `user_interactions` row, up to 16
/// concurrently. Intended for a one-time recovery pass (e.g. immediately
/// after backfilling historically-empty `nft_tags` values) — not meant to
/// be called routinely.
#[derive(serde::Serialize)]
pub(super) struct RebuildAllResponse {
    rebuilt: u64,
    failed: u64,
}

pub(super) async fn rebuild_all_preferences(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<RebuildAllResponse>, StatusCode> {
    require_internal_key(&headers, state.internal_api_key.as_deref())?;
    match crate::recommendation::preferences::rebuild_all_user_preferences(
        &state.pool,
        state.engine.cache(),
    )
    .await
    {
        Ok((rebuilt, failed)) => Ok(Json(RebuildAllResponse { rebuilt, failed })),
        Err(e) => {
            error!("rebuild_all_preferences failed: {e:?}");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

/// One-time backfill for `user_interactions` rows BUG-TAGS-01 left with
/// permanently empty `nft_tags` — looks up each referenced NFT's current
/// genre/genres and fills them in. `state.pool` doubles as the Elixir pool
/// here: `spawn_api_server` (main.rs) already wires this same pool in as
/// `elixir_db.pool()`, which is why `get_user_preferences` et al. can read
/// `nfts` rows through it despite `AppState` having no separate elixir-pool
/// field — this handler relies on that same existing assumption rather than
/// introducing a new one.
///
/// Run `rebuild_all_preferences` afterward so the newly-populated history
/// actually reaches `tag_preferences`/`creator_preferences`.
#[derive(serde::Serialize)]
pub(super) struct BackfillNftTagsResponse {
    nfts_considered: u64,
    nfts_with_genres: u64,
    rows_updated: u64,
}

pub(super) async fn backfill_nft_tags(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<BackfillNftTagsResponse>, StatusCode> {
    require_internal_key(&headers, state.internal_api_key.as_deref())?;
    match crate::event_processor::elixir_db::backfill_historical_nft_tags(&state.pool, &state.pool).await {
        Ok(stats) => Ok(Json(BackfillNftTagsResponse {
            nfts_considered: stats.nfts_considered,
            nfts_with_genres: stats.nfts_with_genres,
            rows_updated: stats.rows_updated,
        })),
        Err(e) => {
            error!("backfill_nft_tags failed: {e:?}");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}
