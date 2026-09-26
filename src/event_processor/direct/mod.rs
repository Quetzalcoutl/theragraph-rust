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
use std::time::Duration;
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
    /// Elixir internal notify URL and API key. Set via ELIXIR_NOTIFY_URL + INTERNAL_API_KEY.
    /// When present, DirectHandlers fire-and-forget POSTs notification triggers to Elixir
    /// after successful like/purchase writes, closing the notification gap that exists
    /// when KAFKA_ENABLED=false (Kafka path never fires → Elixir EventHandlers never run).
    // Arc<str> not String: frozen at construction, never mutated, and re-cloned
    // on every notify_elixir call PLUS every DirectHandlers::clone() (one per
    // indexer instance) — Arc::clone is a refcount bump, String::clone is a
    // fresh allocation + copy of the same bytes every time.
    elixir_notify_url: Option<Arc<str>>,
    elixir_api_key: Arc<str>,
    http: reqwest::Client,
}

impl DirectHandlers {
    pub fn new(
        pool: PgPool,
        elixir_pool: PgPool,
        cache: Option<RecCache>,
        graph_client: Arc<dyn GraphTraversal>,
    ) -> Self {
        let elixir_notify_url: Option<Arc<str>> = std::env::var("ELIXIR_NOTIFY_URL").ok()
            .or_else(|| {
                // Derive from ELIXIR_API_URL if present: http://host:4000 → http://host:4000/internal/notify
                std::env::var("ELIXIR_API_URL").ok()
                    .map(|base| format!("{}/internal/notify", base.trim_end_matches('/')))
            })
            .map(Arc::from);
        let elixir_api_key: Arc<str> = std::env::var("INTERNAL_API_KEY").unwrap_or_default().into();
        let gc = GraphClient::from_dyn_traversal(graph_client);
        Self {
            pool,
            elixir_pool,
            cache,
            graph_sync: GraphSync::new(gc),
            elixir_notify_url,
            elixir_api_key,
            http: reqwest::Client::builder()
                .timeout(Duration::from_millis(800))
                .build()
                .unwrap_or_default(),
        }
    }

    /// Fire-and-forget: POST notification trigger to Elixir's internal endpoint.
    /// Returns immediately; the HTTP call runs in a detached tokio task.
    /// On failure (Elixir down, timeout, wrong key) the like/purchase is still recorded
    /// in the rec DB and Nebula — only the notification is delayed until reconciliation.
    fn notify_elixir(&self, event_type: &'static str, payload: serde_json::Value) {
        let Some(url) = self.elixir_notify_url.clone() else { return };
        let key = self.elixir_api_key.clone();
        let http = self.http.clone();
        let ev = event_type;
        tokio::spawn(async move {
            let body = serde_json::json!({ "event_type": ev, "data": payload });
            match http.post(url.as_ref())
                .header("x-api-key", key.as_ref())
                .header("content-type", "application/json")
                .json(&body)
                .send()
                .await
            {
                Ok(resp) if resp.status().is_success() => {
                    info!("DirectHandlers: notify_elixir {} → 2xx", ev);
                }
                Ok(resp) => {
                    warn!("DirectHandlers: notify_elixir {} → HTTP {}", ev, resp.status());
                }
                Err(e) => {
                    warn!("DirectHandlers: notify_elixir {} failed: {}", ev, e);
                }
            }
        });
    }

    /// Route a parsed blockchain event to the appropriate handler.
    pub async fn dispatch(&self, event: &BlockchainEvent) -> Result<()> {
        match event.event_type.as_str() {
            "Followed" => self.handle_follow(event).await,
            "Unfollowed" => self.handle_unfollow(event).await,
            "SnapLiked"
            | "ArtLiked"
            | "MusicLiked"
            | "FlixLiked" => self.handle_like(event, InteractionType::Like).await,
            "SnapUnliked"
            | "ArtUnliked"
            | "MusicUnliked"
            | "FlixUnliked" => self.handle_like(event, InteractionType::Unlike).await,
            "ContentCopyMinted"
            | "SnapBoughtAndMinted"
            | "ArtBoughtAndMinted"
            | "MusicBoughtAndMinted"
            | "FlixBoughtAndMinted" => self.handle_purchase(event).await,
            "SnapCommented"
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
        // Extract before meta is moved into enrich_and_record_pools.
        let creator_address = meta.creator_address.clone();
        let content_type = meta.contract_type.clone();
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

            // Notify Elixir so content_liked notifications reach the creator even when
            // Kafka is disabled. Fire-and-forget — does not block the indexer loop.
            // Elixir's EventHandlerDecorator idempotency gate deduplicates if Kafka
            // is later re-enabled and the event also arrives via Kafka.
            self.notify_elixir("ContentLiked", serde_json::json!({
                "liker":             liker,
                "token_id":          token_id,
                "contract_address":  event.contract_address,
                "creator_address":   creator_address,
                "content_type":      content_type,
                "transaction_hash":  event.transaction_hash,
                "block_number":      event.block_number,
            }));
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

        let creator_address = meta.creator_address.clone();
        let content_type = meta.contract_type.clone();
        let new_token_id = data.get("newTokenId")
            .or_else(|| data.get("tokenId"))
            .and_then(|v| v.as_str())
            .unwrap_or(original_id);

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
        .map_err(|e| anyhow::anyhow!("{e}"))?;

        // Notify Elixir so copy_purchased notifications reach the original creator
        // even when Kafka is disabled.
        self.notify_elixir("ContentCopyMinted", serde_json::json!({
            "buyer":             buyer,
            "new_token_id":      new_token_id,
            "original_id":       original_id,
            "contract_address":  event.contract_address,
            "creator_address":   creator_address,
            "content_type":      content_type,
            "transaction_hash":  event.transaction_hash,
            "block_number":      event.block_number,
        }));

        Ok(())
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
        "Followed" => Some("follow"),
        "Unfollowed" => Some("unfollow"),
        "SnapLiked" | "ArtLiked" | "MusicLiked" | "FlixLiked" => {
            Some("like")
        }
        "SnapUnliked" | "ArtUnliked" | "MusicUnliked" | "FlixUnliked" => {
            Some("unlike")
        }
        "ContentCopyMinted"
        | "SnapBoughtAndMinted"
        | "ArtBoughtAndMinted"
        | "MusicBoughtAndMinted"
        | "FlixBoughtAndMinted" => Some("purchase"),
        "SnapCommented"
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
mod tests;
