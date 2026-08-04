//! DirectHandlers — preference-signal dispatch when Kafka is disabled.
//!
//! When KAFKA_ENABLED=false the indexer's Kafka send is a no-op, which means
//! follow/like/purchase events never reach the EventProcessor and
//! user_preferences stays empty forever. This module closes that gap: it
//! accepts a ParsedEvent-shaped BlockchainEvent, enriches from the Elixir DB,
//! and writes the same preference signals the Kafka EventProcessor would have
//! written — including Nebula social-graph edges.

use crate::kafka::BlockchainEvent;
use crate::recommendation::cache::RecCache;
use crate::recommendation::graph_client::{GraphClient, GraphTraversal};
use crate::recommendation::preferences::InteractionType;
use anyhow::Result;
use sqlx::PgPool;
use std::sync::Arc;
use tracing::{info, warn};

use super::elixir_db;
use super::graph_sync::GraphSync;
use super::interaction::enrich_and_record_pools;
use crate::recommendation::graph_client::DynGraphTransport;

/// Processes blockchain events directly into the recommendation DB and Nebula.
/// Constructed once at startup when Kafka is disabled; cloned into each indexer.
#[derive(Clone)]
pub struct DirectHandlers {
    pool: PgPool,
    elixir_pool: PgPool,
    cache: Option<RecCache>,
    graph_sync: GraphSync<DynGraphTransport>,
}

impl DirectHandlers {
    pub fn new(
        pool: PgPool,
        elixir_pool: PgPool,
        cache: Option<RecCache>,
        graph_client: Arc<dyn GraphTraversal>,
    ) -> Self {
        let gc = GraphClient::from_dyn_traversal(graph_client);
        Self { pool, elixir_pool, cache, graph_sync: GraphSync::new(gc) }
    }

    /// Route a parsed blockchain event to the appropriate handler.
    pub async fn dispatch(&self, event: &BlockchainEvent) -> Result<()> {
        match event.event_type.as_str() {
            "UserFollowed" | "Followed" => self.handle_follow(event).await,
            "UserUnfollowed" | "Unfollowed" => self.handle_unfollow(event).await,
            "ContentLiked"
            | "SnapLiked"
            | "ArtLiked"
            | "MusicLiked"
            | "FlixLiked" => self.handle_like(event, InteractionType::Like).await,
            "ContentUnliked"
            | "SnapUnliked"
            | "ArtUnliked"
            | "MusicUnliked"
            | "FlixUnliked" => self.handle_like(event, InteractionType::Unlike).await,
            "ContentCopyMinted"
            | "SnapBoughtAndMinted"
            | "ArtBoughtAndMinted"
            | "MusicBoughtAndMinted"
            | "FlixBoughtAndMinted" => self.handle_purchase(event).await,
            "ContentCommented"
            | "SnapCommented"
            | "ArtCommented"
            | "MusicCommented"
            | "FlixCommented" => self.handle_generic_interaction(event, InteractionType::Comment, "feed").await,
            "ContentBookmarked" => self.handle_generic_interaction(event, InteractionType::Save, "feed").await,
            "ContentShared" => self.handle_generic_interaction(event, InteractionType::Share, "feed").await,
            _ => Ok(()),
        }
    }

    async fn handle_follow(&self, event: &BlockchainEvent) -> Result<()> {
        let data = match &event.data { Some(d) => d, None => return Ok(()) };
        let follower = data.get("follower").and_then(|v| v.as_str()).unwrap_or("").to_lowercase();
        let target = data.get("target").and_then(|v| v.as_str()).unwrap_or("").to_lowercase();
        if follower.is_empty() || target.is_empty() { return Ok(()); }

        sqlx::query(
            "INSERT INTO user_preferences \
             (id, user_address, creator_preferences, inserted_at, updated_at) \
             VALUES (gen_random_uuid(), $1, jsonb_build_object($2, 0.8::float8), NOW(), NOW()) \
             ON CONFLICT (user_address) DO UPDATE SET \
                 creator_preferences = user_preferences.creator_preferences || \
                     jsonb_build_object($2, LEAST( \
                         COALESCE((user_preferences.creator_preferences->$2)::float8, 0.3) + 0.15, \
                         0.95 \
                     )), \
                 updated_at = NOW()",
        )
        .bind(&follower)
        .bind(&target)
        .execute(&self.pool)
        .await?;

        self.graph_sync
            .sync_follow(&follower, &target, &event.transaction_hash)
            .await
            .map_err(|e| anyhow::anyhow!("Direct: Nebula sync_follow failed: {e}"))?;

        info!("Direct: follow {} → {} (prefs + Nebula)", follower, target);
        Ok(())
    }

    async fn handle_unfollow(&self, event: &BlockchainEvent) -> Result<()> {
        let data = match &event.data { Some(d) => d, None => return Ok(()) };
        let follower = data.get("follower").and_then(|v| v.as_str()).unwrap_or("").to_lowercase();
        let target = data.get("target").and_then(|v| v.as_str()).unwrap_or("").to_lowercase();
        if follower.is_empty() || target.is_empty() { return Ok(()); }

        sqlx::query(
            "UPDATE user_preferences SET \
                 creator_preferences = creator_preferences || \
                     jsonb_build_object($2, GREATEST( \
                         COALESCE((creator_preferences->$2)::float8, 0.3) - 0.2, \
                         0.2 \
                     )), \
                 updated_at = NOW() \
             WHERE user_address = $1",
        )
        .bind(&follower)
        .bind(&target)
        .execute(&self.pool)
        .await?;

        self.graph_sync
            .sync_unfollow(&follower, &target)
            .await
            .map_err(|e| anyhow::anyhow!("Direct: Nebula sync_unfollow failed: {e}"))?;

        Ok(())
    }

    async fn handle_like(&self, event: &BlockchainEvent, interaction_type: InteractionType) -> Result<()> {
        let data = match &event.data { Some(d) => d, None => return Ok(()) };
        let liker = data.get("liker").and_then(|v| v.as_str()).unwrap_or("").to_lowercase();
        let token_id = data.get("tokenId").and_then(|v| v.as_str()).unwrap_or("");
        if liker.is_empty() { return Ok(()); }

        let (nft_uuid, meta) = match elixir_db::lookup_nft_with_metadata(
            &self.elixir_pool, &event.contract_address, token_id,
        ).await? {
            Some(pair) => pair,
            None => {
                warn!(
                    "Direct: NFT not found for like: contract={}, token={}",
                    event.contract_address, token_id
                );
                self.save_pending(event, &liker, token_id).await;
                return Ok(());
            }
        };

        let is_like = matches!(interaction_type, InteractionType::Like);
        enrich_and_record_pools(
            &self.pool,
            &self.elixir_pool,
            self.cache.as_ref(),
            &nft_uuid,
            Some(meta),
            &liker,
            interaction_type,
            "blockchain",
            &event.contract_type,
            event,
        )
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;

        // BUG-004: propagate Nebula sync_like error instead of swallowing it.
        // The caller (dispatch) can decide to log-and-continue; but swallowing
        // here meant graph writes were silently lost without any visibility.
        if is_like {
            self.graph_sync
                .sync_like(
                    &event.contract_address,
                    &nft_uuid.to_string(),
                    &liker,
                    &event.transaction_hash,
                )
                .await
                .map_err(|e| anyhow::anyhow!("Direct: Nebula sync_like failed: {e}"))?;
        }

        Ok(())
    }

    async fn handle_purchase(&self, event: &BlockchainEvent) -> Result<()> {
        let data = match &event.data { Some(d) => d, None => return Ok(()) };
        let buyer = data.get("buyer").and_then(|v| v.as_str()).unwrap_or("").to_lowercase();
        let original_id = data.get("originalId").and_then(|v| v.as_str()).unwrap_or("");
        if buyer.is_empty() { return Ok(()); }

        let (nft_uuid, meta) = match elixir_db::lookup_nft_with_metadata(
            &self.elixir_pool, &event.contract_address, original_id,
        ).await? {
            Some(pair) => pair,
            None => {
                self.save_pending(event, &buyer, original_id).await;
                return Ok(());
            }
        };

        enrich_and_record_pools(
            &self.pool,
            &self.elixir_pool,
            self.cache.as_ref(),
            &nft_uuid,
            Some(meta),
            &buyer,
            InteractionType::Purchase,
            "marketplace",
            &event.contract_type,
            event,
        )
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))
    }

    async fn handle_generic_interaction(
        &self,
        event: &BlockchainEvent,
        interaction_type: InteractionType,
        source: &str,
    ) -> Result<()> {
        let data = match &event.data { Some(d) => d, None => return Ok(()) };
        let user = data.get("user")
            .or_else(|| data.get("commenter"))
            .or_else(|| data.get("sharer"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_lowercase();
        let token_id = data.get("tokenId").and_then(|v| v.as_str()).unwrap_or("");
        if user.is_empty() { return Ok(()); }

        let (nft_uuid, meta) = match elixir_db::lookup_nft_with_metadata(
            &self.elixir_pool, &event.contract_address, token_id,
        ).await? {
            Some(pair) => pair,
            None => {
                self.save_pending(event, &user, token_id).await;
                return Ok(());
            }
        };

        enrich_and_record_pools(
            &self.pool,
            &self.elixir_pool,
            self.cache.as_ref(),
            &nft_uuid,
            Some(meta),
            &user,
            interaction_type,
            source,
            &event.contract_type,
            event,
        )
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))
    }

    /// C3: persist failed-lookup events for repair once NFT is indexed.
    async fn save_pending(&self, event: &BlockchainEvent, user_address: &str, token_id: &str) {
        let result = sqlx::query(
            "INSERT INTO pending_interactions \
             (id, user_address, contract_address, contract_type, token_id, event_type, \
              transaction_hash, block_number, created_at) \
             VALUES (gen_random_uuid(), $1, $2, $3, $4, $5, $6, $7, NOW()) \
             ON CONFLICT (user_address, contract_address, token_id, event_type) DO NOTHING",
        )
        .bind(user_address)
        .bind(&event.contract_address)
        .bind(&event.contract_type)
        .bind(token_id)
        .bind(&event.event_type)
        .bind(&event.transaction_hash)
        .bind(event.block_number as i64)
        .execute(&self.pool)
        .await;

        if let Err(e) = result {
            warn!("Direct: failed to save pending interaction: {e}");
        }
    }
}

// ── Pure helpers (no I/O) — extracted for testability ─────────────────────────

/// Categorise an event_type string into the handler group dispatch() would use.
/// Returns `None` for unknown / ignored event types.
#[allow(dead_code)]
pub(crate) fn classify_event_type(event_type: &str) -> Option<&'static str> {
    match event_type {
        "UserFollowed" | "Followed" => Some("follow"),
        "UserUnfollowed" | "Unfollowed" => Some("unfollow"),
        "ContentLiked" | "SnapLiked" | "ArtLiked" | "MusicLiked" | "FlixLiked" => {
            Some("like")
        }
        "ContentUnliked" | "SnapUnliked" | "ArtUnliked" | "MusicUnliked" | "FlixUnliked" => {
            Some("unlike")
        }
        "ContentCopyMinted"
        | "SnapBoughtAndMinted"
        | "ArtBoughtAndMinted"
        | "MusicBoughtAndMinted"
        | "FlixBoughtAndMinted" => Some("purchase"),
        "ContentCommented"
        | "SnapCommented"
        | "ArtCommented"
        | "MusicCommented"
        | "FlixCommented" => Some("comment"),
        "ContentBookmarked" => Some("save"),
        "ContentShared" => Some("share"),
        _ => None,
    }
}

/// Parse a token-id string to `i64`.  Returns `None` for non-numeric or u256 input,
/// mirroring the early-return in `lookup_nft_with_metadata`.
#[allow(dead_code)]
pub(crate) fn parse_token_id(s: &str) -> Option<i64> {
    s.parse::<i64>().ok()
}

/// Normalise a blockchain address for storage: lowercase, empty-string guard.
/// Returns `None` when the input is blank (or only whitespace after trim).
#[allow(dead_code)]
pub(crate) fn normalise_address(raw: &str) -> Option<String> {
    let lower = raw.to_lowercase();
    if lower.trim().is_empty() {
        None
    } else {
        Some(lower)
    }
}

/// Extract the follow-event actor addresses from a JSON data payload.
/// Returns `(follower, target)`, both normalised, or `None` if either is absent.
#[allow(dead_code)]
pub(crate) fn extract_follow_addrs(
    data: &serde_json::Value,
) -> Option<(String, String)> {
    let follower = normalise_address(data.get("follower")?.as_str()?)?;
    let target = normalise_address(data.get("target")?.as_str()?)?;
    Some((follower, target))
}

// ── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── classify_event_type ─────────────────────────────────────────────────

    #[test]
    fn classify_follow_variants() {
        assert_eq!(classify_event_type("UserFollowed"), Some("follow"));
        assert_eq!(classify_event_type("Followed"), Some("follow"));
    }

    #[test]
    fn classify_unfollow_variants() {
        assert_eq!(classify_event_type("UserUnfollowed"), Some("unfollow"));
        assert_eq!(classify_event_type("Unfollowed"), Some("unfollow"));
    }

    #[test]
    fn classify_like_variants() {
        for ev in &["ContentLiked", "SnapLiked", "ArtLiked", "MusicLiked", "FlixLiked"] {
            assert_eq!(classify_event_type(ev), Some("like"), "failed for {ev}");
        }
    }

    #[test]
    fn classify_unlike_variants() {
        for ev in &[
            "ContentUnliked",
            "SnapUnliked",
            "ArtUnliked",
            "MusicUnliked",
            "FlixUnliked",
        ] {
            assert_eq!(classify_event_type(ev), Some("unlike"), "failed for {ev}");
        }
    }

    #[test]
    fn classify_purchase_variants() {
        for ev in &[
            "ContentCopyMinted",
            "SnapBoughtAndMinted",
            "ArtBoughtAndMinted",
            "MusicBoughtAndMinted",
            "FlixBoughtAndMinted",
        ] {
            assert_eq!(classify_event_type(ev), Some("purchase"), "failed for {ev}");
        }
    }

    #[test]
    fn classify_comment_variants() {
        for ev in &[
            "ContentCommented",
            "SnapCommented",
            "ArtCommented",
            "MusicCommented",
            "FlixCommented",
        ] {
            assert_eq!(classify_event_type(ev), Some("comment"), "failed for {ev}");
        }
    }

    #[test]
    fn classify_bookmark_share() {
        assert_eq!(classify_event_type("ContentBookmarked"), Some("save"));
        assert_eq!(classify_event_type("ContentShared"), Some("share"));
    }

    #[test]
    fn classify_unknown_returns_none() {
        assert_eq!(classify_event_type("ContentMinted"), None);
        assert_eq!(classify_event_type("UserFollowed_typo"), None);
        assert_eq!(classify_event_type(""), None);
        assert_eq!(classify_event_type("CONTENTLIKED"), None); // case-sensitive
    }

    // ── parse_token_id ──────────────────────────────────────────────────────

    #[test]
    fn parse_token_id_valid_numbers() {
        assert_eq!(parse_token_id("0"), Some(0));
        assert_eq!(parse_token_id("1"), Some(1));
        assert_eq!(parse_token_id("42"), Some(42));
        assert_eq!(parse_token_id("9223372036854775807"), Some(i64::MAX));
    }

    #[test]
    fn parse_token_id_negative() {
        // Negative token IDs are technically valid i64 parses
        assert_eq!(parse_token_id("-1"), Some(-1));
    }

    #[test]
    fn parse_token_id_non_numeric_returns_none() {
        assert_eq!(parse_token_id(""), None);
        assert_eq!(parse_token_id("abc"), None);
        assert_eq!(parse_token_id("1.5"), None);
        assert_eq!(parse_token_id("0x1a"), None); // hex not supported
    }

    #[test]
    fn parse_token_id_overflow_returns_none() {
        // u128::MAX does not fit in i64
        assert_eq!(parse_token_id("99999999999999999999999"), None);
    }

    // ── normalise_address ───────────────────────────────────────────────────

    #[test]
    fn normalise_address_lowercases() {
        assert_eq!(
            normalise_address("0xABCDEF"),
            Some("0xabcdef".to_string())
        );
    }

    #[test]
    fn normalise_address_already_lowercase_unchanged() {
        assert_eq!(
            normalise_address("0xdeadbeef"),
            Some("0xdeadbeef".to_string())
        );
    }

    #[test]
    fn normalise_address_empty_returns_none() {
        assert_eq!(normalise_address(""), None);
        assert_eq!(normalise_address("   "), None);
    }

    // ── extract_follow_addrs ────────────────────────────────────────────────

    #[test]
    fn extract_follow_addrs_happy_path() {
        let data = serde_json::json!({
            "follower": "0xALICE",
            "target":   "0xBOB"
        });
        let (f, t) = extract_follow_addrs(&data).unwrap();
        assert_eq!(f, "0xalice");
        assert_eq!(t, "0xbob");
    }

    #[test]
    fn extract_follow_addrs_missing_follower_returns_none() {
        let data = serde_json::json!({ "target": "0xBOB" });
        assert!(extract_follow_addrs(&data).is_none());
    }

    #[test]
    fn extract_follow_addrs_missing_target_returns_none() {
        let data = serde_json::json!({ "follower": "0xALICE" });
        assert!(extract_follow_addrs(&data).is_none());
    }

    #[test]
    fn extract_follow_addrs_empty_strings_return_none() {
        let data = serde_json::json!({ "follower": "", "target": "0xBOB" });
        assert!(extract_follow_addrs(&data).is_none());
    }

    // ── follow / unfollow score delta arithmetic ────────────────────────────
    // The SQL uses LEAST/GREATEST to clamp.  Mirror the arithmetic here so
    // the test documents the intended semantics regardless of DB.

    fn follow_score(current: f32) -> f32 {
        (current + 0.15_f32).min(0.95)
    }

    fn unfollow_score(current: f32) -> f32 {
        (current - 0.20_f32).max(0.20)
    }

    #[test]
    fn follow_score_increases_and_caps_at_095() {
        assert!((follow_score(0.3) - 0.45).abs() < 1e-6);
        assert!((follow_score(0.85) - 0.95).abs() < 1e-6); // capped
        assert!((follow_score(0.95) - 0.95).abs() < 1e-6); // already at cap
    }

    #[test]
    fn follow_score_never_exceeds_095() {
        // Even starting at 1.0 the cap holds
        assert!(follow_score(1.0) <= 0.95);
    }

    #[test]
    fn unfollow_score_decreases_and_floors_at_020() {
        assert!((unfollow_score(0.5) - 0.30).abs() < 1e-6);
        assert!((unfollow_score(0.3) - 0.20).abs() < 1e-6); // floored
        assert!((unfollow_score(0.1) - 0.20).abs() < 1e-6); // already below floor
    }

    #[test]
    fn unfollow_score_never_goes_below_020() {
        assert!(unfollow_score(0.0) >= 0.20);
    }
}
