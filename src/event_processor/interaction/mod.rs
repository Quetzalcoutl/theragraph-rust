// Interaction handlers — events that carry recommendation signal.
// Each handler resolves a UUID, enriches with metadata, then records an interaction.

use crate::error::{Error, Result};
use crate::kafka::BlockchainEvent;
use crate::recommendation::preferences::{record_interaction, InteractionEvent, InteractionType};
use tokio::sync::mpsc::error::TrySendError;
use tracing::{info, warn};
use uuid::Uuid;

use super::{EnrichmentTask, EventDispatcher, NftMetadata};

/// Resolve an NFT UUID + metadata from the Elixir DB; return early when not found.
macro_rules! lookup_nft_or_skip {
    ($pool:expr, $contract:expr, $token_id:expr, $ctx:literal) => {
        match super::elixir_db::lookup_nft_with_metadata($pool, $contract, $token_id).await? {
            Some(pair) => pair,
            None => {
                warn!("NFT not found for {}: contract={}, token={}", $ctx, $contract, $token_id);
                return Ok(());
            }
        }
    };
}

/// Convert a uint256 wei amount (decoded as a decimal string by the ABI
/// parser) into human-scale TREEZ units for Nebula's `double` price property.
///
/// Lazy-mint economy (2026-09-13): the Nebula graph is a recommendation/
/// scoring signal store, not the financial ledger — Postgres (`nfts.price`,
/// `DECIMAL(78,0)`, via theragraph-indexer) stays the wei-precise source of
/// truth. This lossy f64 approximation is only ever read back for scoring/
/// display in the graph, matching the same wei/1e18 conversion
/// `friendsz_indexer.ex` already does when it needs a human-scale TREEZ
/// amount instead of a raw wei string.
fn wei_str_to_treez(s: &str) -> f64 {
    s.parse::<f64>().unwrap_or(0.0) / 1e18
}

impl EventDispatcher {
    pub(super) async fn handle_content_minted(&self, event: &BlockchainEvent) -> Result<()> {
        if let Some(data) = &event.data {
            let token_id_str = data.get("tokenId").and_then(|v| v.as_str()).unwrap_or("");
            let creator = data.get("creator").and_then(|v| v.as_str()).unwrap_or("");

            let token_id: i64 = match data
                .get("tokenId")
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse::<i64>().ok())
            {
                Some(id) => id,
                None => {
                    warn!("handle_content_minted: invalid or missing tokenId in event {:?}, skipping",
                        event.transaction_hash);
                    return Ok(());
                }
            };

            let nft_uuid = Self::generate_nft_uuid(&event.contract_address, token_id_str);

            // token_id is already i64 (parsed at the get("tokenId") block above); no conversion needed.
            let token_id_i64 = token_id;

            sqlx::query(
                // TAG-S27-01: DO NOT overwrite tags on conflict — the enrichment worker
                // may have already populated them asynchronously. EXCLUDED.tags is always []
                // here (mint event carries no tags); SET tags = EXCLUDED.tags silently wipes
                // enriched tags on every Kafka redelivery.
                "INSERT INTO nft_features \
                 (nft_id, contract_address, token_id, tags, inserted_at, updated_at) \
                 VALUES ($1, $2, $3, $4, NOW(), NOW()) \
                 ON CONFLICT (nft_id) DO UPDATE SET updated_at = NOW()",
            )
            .bind(nft_uuid)
            .bind(&event.contract_address)
            .bind(token_id_i64)
            .bind(Vec::<String>::new())
            .execute(&self.pool)
            .await
            .map_err(|e| Error::Database {
                message: "Failed to update NFT features".into(),
                source: Some(e),
            })?;

            // Queue tag backfill — try_send never blocks the Kafka consumer loop.
            // A full queue means the enrichment worker is behind; skip this backfill
            // rather than stalling offset commits. The score updater retries missed
            // enrichments on its next cycle via reconcile_placeholder_nfts.
            match self.enrichment_tx.try_send(EnrichmentTask {
                nft_uuid,
                contract_address: event.contract_address.as_str().into(),
                token_id: token_id_i64,
            }) {
                Ok(()) => {},
                Err(TrySendError::Full(_)) => {
                    // Enrichment task dropped — nft_features row for this NFT stays with
                    // Degraded tag enrichment until a future interaction re-enqueues it.
                    // TODO: add a reconcile_degraded_enrichments pass to the score updater
                    // that re-sends EnrichmentTask for all nft_features rows with status=Degraded.
                    warn!("enrichment queue full, skipping backfill for {}", nft_uuid);
                }
                Err(TrySendError::Closed(_)) => {
                    warn!("enrichment worker disconnected, skipping backfill for {}", nft_uuid);
                }
            }

            // Lazy-mint economy (2026-09-13): persist the voucher's real listing
            // price onto the Nebula post vertex. VID-FIX: the vertex must be keyed
            // by the Postgres UUID (nfts.id), same as every other graph_sync call
            // in this module, or this lands on a "post:<token_id>" vertex
            // disconnected from the "post:<uuid>" one likes/comments/purchases use.
            //
            // Separate lookup (not `lookup_nft_or_skip!`) because the rest of this
            // handler's work (nft_features + enrichment queue above) must run even
            // when the Elixir consumer hasn't inserted the `nfts` row yet — a real
            // race, since both consumers read the same Kafka topic independently.
            // Best-effort: if the row isn't there yet, skip silently — the paired
            // ListingUpdated event (same `redeemVoucher` tx) or a later interaction
            // will re-establish the vertex.
            match super::elixir_db::lookup_nft_with_metadata(&self.elixir_pool, &event.contract_address, token_id_str).await {
                Ok(Some((uuid, _meta))) => {
                    let price = data
                        .get("price")
                        .and_then(|v| v.as_str())
                        .map(wei_str_to_treez)
                        .unwrap_or(0.0);
                    if let Err(e) = self.graph_sync.sync_content_minted(&uuid.to_string(), price).await {
                        warn!("handle_content_minted: Nebula sync_content_minted failed: {e}");
                    }
                }
                Ok(None) => {
                    warn!(
                        "handle_content_minted: NFT row not found yet for Nebula price sync (contract={}, token={}) — will be set by the paired ListingUpdated or a later interaction",
                        event.contract_address, token_id_str
                    );
                }
                Err(e) => {
                    warn!("handle_content_minted: elixir_db lookup failed for Nebula price sync: {e}");
                }
            }

            info!("📝 Processed content mint: {} by {}", nft_uuid, creator);
        }
        Ok(())
    }

    /// `ListingUpdated` — fires on a voucher's first redemption (same tx as its
    /// paired `ContentMinted`) and on every later `updateListing` call. Persists
    /// price/max_copies onto the Nebula post vertex (lazy-mint economy,
    /// 2026-09-13 — see migration 26 in theragraph-nebula/init/).
    ///
    /// No Postgres write here: theragraph-indexer (Elixir) owns the canonical
    /// `nfts.price`/`max_copies` row for this event. This handler is Nebula-only,
    /// same division of responsibility as every other graph_sync-only handler.
    pub(super) async fn handle_listing_updated(&self, event: &BlockchainEvent) -> Result<()> {
        let data = match &event.data {
            Some(d) => d,
            None => {
                warn!(
                    "handle_listing_updated: event has no data payload tx={:?} — skipping (producer bug?)",
                    event.transaction_hash
                );
                return Ok(());
            }
        };

        let original_token_id = data.get("original_token_id").and_then(|v| v.as_str()).unwrap_or("");
        if original_token_id.is_empty() {
            warn!(
                "handle_listing_updated: missing original_token_id in event tx={:?}, skipping",
                event.transaction_hash
            );
            return Ok(());
        }

        let price = data
            .get("price")
            .and_then(|v| v.as_str())
            .map(wei_str_to_treez)
            .unwrap_or(0.0);
        let max_copies = data
            .get("max_copies")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or(0);

        info!(
            "🏷️ Listing updated: original_token_id={} price={} max_copies={}",
            original_token_id, price, max_copies
        );

        // VID-FIX + race tolerance: same pattern as handle_content_minted above.
        // ContentMinted and this event's FIRST occurrence fire in the same
        // `redeemVoucher` tx, so the Elixir consumer may not have inserted the
        // `nfts` row yet — best-effort, skip silently rather than block. Every
        // later `updateListing`-triggered occurrence is unaffected: real time has
        // passed since mint, so the row is reliably present by then.
        match super::elixir_db::lookup_nft_with_metadata(&self.elixir_pool, &event.contract_address, original_token_id).await {
            Ok(Some((uuid, _meta))) => {
                if let Err(e) = self.graph_sync.sync_listing_updated(&uuid.to_string(), price, max_copies).await {
                    warn!("handle_listing_updated: Nebula sync_listing_updated failed: {e}");
                }
            }
            Ok(None) => {
                warn!(
                    "handle_listing_updated: NFT row not found yet for original_token_id={} (contract={}) — Nebula price/max_copies not synced this event",
                    original_token_id, event.contract_address
                );
            }
            Err(e) => {
                warn!("handle_listing_updated: elixir_db lookup failed: {e}");
            }
        }

        Ok(())
    }

    pub(super) async fn handle_content_purchase(&self, event: &BlockchainEvent) -> Result<()> {
        // SILENT-NULL-DATA-DROP: data=None on a purchase event is a producer-side bug —
        // warn so operators can detect malformed events from the marketplace contract.
        let data = match &event.data {
            Some(d) => d,
            None => {
                warn!("handle_content_purchase: event has no data payload tx={:?} — skipping (producer bug?)",
                    event.transaction_hash);
                return Ok(());
            }
        };

        let buyer = data.get("buyer").and_then(|v| v.as_str()).unwrap_or("");
        // EMPTY-USER-ADDRESS: reject events where buyer is absent or empty to avoid
        // writing garbage user_preferences rows keyed on "".
        if buyer.is_empty() {
            warn!("handle_content_purchase: missing buyer in event tx={:?}, skipping",
                event.transaction_hash);
            return Ok(());
        }

        let original_id = data.get("originalId").and_then(|v| v.as_str()).unwrap_or("");
        let new_token_id = data.get("newTokenId").and_then(|v| v.as_str()).unwrap_or("");

        let (nft_uuid, meta) = match super::elixir_db::lookup_nft_with_metadata(
            &self.elixir_pool, &event.contract_address, original_id,
        ).await? {
            Some(pair) => pair,
            None => match super::elixir_db::lookup_nft_with_metadata(
                &self.elixir_pool, &event.contract_address, new_token_id,
            ).await? {
                Some(pair) => pair,
                None => {
                    warn!(
                        "NFT not found for purchase: contract={}, original={}, new={}",
                        event.contract_address, original_id, new_token_id
                    );
                    return Ok(());
                }
            },
        };

        enrich_and_record(self, &nft_uuid, Some(meta), buyer, InteractionType::Purchase, "marketplace", event).await?;

        // Materialise once — sync_purchase takes &str, not &Uuid.
        let nft_uuid_str = nft_uuid.to_string();

        // WIRE-05: write purchase edge to Nebula so the FoF traversal picks
        // up purchase signals when building recommendations.
        // Best-effort — Nebula failure does not roll back the Postgres write.
        if let Err(e) = self
            .graph_sync
            .sync_purchase(
                &event.contract_address,
                // VID-FIX: nft_uuid is the Postgres UUID of the original NFT (looked up
                // by original_id above). Using it ensures the purchase edge lands on the
                // same "post:{uuid}" vertex that the API write path uses.
                // Integer original_id → "post:42" VID would be disconnected from API edges.
                &nft_uuid_str,
                buyer,
                &event.transaction_hash,
            )
            .await
        {
            warn!("handle_content_purchase: Nebula sync_purchase failed: {e}");
        }

        info!(
            "Processed content purchase: {} bought copy of {} (uuid={})",
            buyer, original_id, nft_uuid
        );
        Ok(())
    }

    pub(super) async fn handle_like(
        &self,
        event: &BlockchainEvent,
        interaction_type: InteractionType,
    ) -> Result<()> {
        if let Some(data) = &event.data {
            let liker = data.get("liker").and_then(|v| v.as_str()).unwrap_or("");
            // EMPTY-USER-ADDRESS: reject empty liker to prevent garbage DB rows.
            if liker.is_empty() {
                warn!("handle_like: missing liker in event tx={:?}, skipping", event.transaction_hash);
                return Ok(());
            }

            let token_id = data.get("tokenId").and_then(|v| v.as_str()).unwrap_or("");

            let (nft_uuid, meta) = lookup_nft_or_skip!(&self.elixir_pool, &event.contract_address, token_id, "like");

            // OUT-OF-ORDER-LIKE-UNLIKE: check if we already processed a more recent
            // block for this (user, nft) like/unlike pair. Kafka rebalances can deliver
            // Unlike (block N+1) before Like (block N); without this guard the final
            // Nebula state would be wrong.
            if self.is_stale_like_event(liker, &nft_uuid, event.block_number).await {
                warn!("handle_like: out-of-order event block={} for user={} nft={}, skipping",
                    event.block_number, liker.chars().take(10).collect::<String>(), nft_uuid);
                return Ok(());
            }

            enrich_and_record(self, &nft_uuid, Some(meta), liker, interaction_type, "feed", event).await?;

            // Materialise once — sync_like/sync_unlike take &str, not &Uuid.
            let nft_uuid_str = nft_uuid.to_string();

            // WIRE-03: write the likes / unlike edge to Nebula so the FoF scoring
            // traversal (get_fof_recommendations) can walk the social graph.
            // Best-effort — Nebula failure is logged but does not roll back the
            // Postgres write above.
            match interaction_type {
                InteractionType::Like => {
                    if let Err(e) = self
                        .graph_sync
                        .sync_like(
                            &event.contract_address,
                            // VID-FIX: use Postgres UUID (already fetched above) so the Kafka
                            // write path produces "post:{uuid}" VIDs, matching the API path's
                            // write_likes_edge which also uses the UUID. Integer token_id VIDs
                            // ("post:42") and UUID VIDs ("post:550e...") are disconnected vertices
                            // in Nebula — FoF traversals starting from one style never find edges
                            // written by the other, causing recommendation blind spots.
                            &nft_uuid_str,
                            liker,
                            &event.transaction_hash,
                        )
                        .await
                    {
                        warn!("handle_like: Nebula sync_like failed: {e}");
                    }
                }
                InteractionType::Unlike => {
                    if let Err(e) = self
                        .graph_sync
                        .sync_unlike(&event.contract_address, &nft_uuid_str, liker)
                        .await
                    {
                        warn!("handle_like: Nebula sync_unlike failed: {e}");
                    }
                }
                _ => {}
            }

            info!(
                "👍 Processed {}: {} on {} (uuid={})",
                event.event_type, liker, token_id, nft_uuid
            );
        } else {
            warn!("handle_like: event has no data payload tx={:?} — skipping (producer bug?)", event.transaction_hash);
        }
        Ok(())
    }

    pub(super) async fn handle_comment(&self, event: &BlockchainEvent) -> Result<()> {
        if let Some(data) = &event.data {
            let commenter = data.get("commenter").and_then(|v| v.as_str()).unwrap_or("");
            // EMPTY-USER-ADDRESS: reject empty commenter.
            if commenter.is_empty() {
                warn!("handle_comment: missing commenter in event tx={:?}, skipping", event.transaction_hash);
                return Ok(());
            }

            let token_id = data.get("tokenId").and_then(|v| v.as_str()).unwrap_or("");
            // Extract comment text when present; empty string is safe (filtered in sync_comment).
            let comment_text = data
                .get("commentText")
                .or_else(|| data.get("comment"))
                .and_then(|v| v.as_str())
                .unwrap_or("");

            let (nft_uuid, meta) = lookup_nft_or_skip!(&self.elixir_pool, &event.contract_address, token_id, "comment");

            enrich_and_record(self, &nft_uuid, Some(meta), commenter, InteractionType::Comment, "feed", event).await?;

            // WIRE-03: write comments_on edge to Nebula so comment-FoF traversal
            // (get_comment_fof_recommendations) can walk the social graph.
            // Best-effort — Nebula failure does not roll back the Postgres write above.
            //
            // COMMENT-RANDOM-EDGE-ID: use transaction_hash as edge ID (matches the pattern
            // used by sync_like and sync_purchase) so comment edges are idempotent across
            // Kafka redeliveries. A fresh new_v4() on every call created duplicate edges.
            let event_id = event.transaction_hash.clone();
            // Materialise once — sync_comment takes &str, not &Uuid.
            let nft_uuid_str = nft_uuid.to_string();
            if let Err(e) = self
                .graph_sync
                // VID-FIX: use Postgres UUID so comment edges land on the same "post:{uuid}"
                // vertex that the API write path and like edges use.
                .sync_comment(&nft_uuid_str, commenter, &event_id, comment_text)
                .await
            {
                warn!("handle_comment: Nebula sync_comment failed: {e}");
            }

            info!("💬 Processed comment: {} on {} (uuid={})", commenter, token_id, nft_uuid);
        } else {
            warn!("handle_comment: event has no data payload tx={:?} — skipping (producer bug?)", event.transaction_hash);
        }
        Ok(())
    }

    pub(super) async fn handle_bookmark(&self, event: &BlockchainEvent) -> Result<()> {
        if let Some(data) = &event.data {
            let user = data.get("user").and_then(|v| v.as_str()).unwrap_or("");
            // EMPTY-USER-ADDRESS: reject empty user.
            if user.is_empty() {
                warn!("handle_bookmark: missing user in event tx={:?}, skipping", event.transaction_hash);
                return Ok(());
            }

            let token_id = data.get("tokenId").and_then(|v| v.as_str()).unwrap_or("");
            let bookmarked = data.get("bookmarked").and_then(|v| v.as_bool()).unwrap_or(false);

            let (nft_uuid, meta) = lookup_nft_or_skip!(&self.elixir_pool, &event.contract_address, token_id, "bookmark");

            let itype = if bookmarked { InteractionType::Save } else { InteractionType::Unsave };
            enrich_and_record(self, &nft_uuid, Some(meta), user, itype, "feed", event).await?;

            // TAG-S29-04: bookmark/unbookmark Nebula edge — highest-intent signal.
            let nft_uuid_str = nft_uuid.to_string();
            let tx_hash_str = event.transaction_hash.as_str();
            if bookmarked {
                if let Err(e) = self.graph_sync.sync_bookmark(&nft_uuid_str, user, tx_hash_str).await {
                    warn!("handle_bookmark: Nebula sync_bookmark failed: {e}");
                }
            } else if let Err(e) = self.graph_sync.sync_unbookmark(&nft_uuid_str, user).await {
                warn!("handle_bookmark: Nebula sync_unbookmark failed: {e}");
            }

            info!(
                "🔖 Processed bookmark: {} {} {} (uuid={})",
                user,
                if bookmarked { "saved" } else { "unsaved" },
                token_id,
                nft_uuid
            );
        } else {
            warn!("handle_bookmark: event has no data payload tx={:?} — skipping (producer bug?)", event.transaction_hash);
        }
        Ok(())
    }

    pub(super) async fn handle_share(&self, event: &BlockchainEvent) -> Result<()> {
        if let Some(data) = &event.data {
            let sharer = data.get("sharer").and_then(|v| v.as_str()).unwrap_or("");
            // EMPTY-USER-ADDRESS: reject empty sharer.
            if sharer.is_empty() {
                warn!("handle_share: missing sharer in event tx={:?}, skipping", event.transaction_hash);
                return Ok(());
            }

            let token_id = data.get("tokenId").and_then(|v| v.as_str()).unwrap_or("");

            let (nft_uuid, meta) = lookup_nft_or_skip!(&self.elixir_pool, &event.contract_address, token_id, "share");

            enrich_and_record(self, &nft_uuid, Some(meta), sharer, InteractionType::Share, "feed", event).await?;

            // TAG-S29-04: share Nebula edge — highest-intent signal.
            let nft_uuid_str = nft_uuid.to_string();
            let tx_hash_str = event.transaction_hash.as_str();
            if let Err(e) = self.graph_sync.sync_share(&nft_uuid_str, sharer, tx_hash_str).await {
                warn!("handle_share: Nebula sync_share failed: {e}");
            }

            info!("📤 Processed share: {} shared {} (uuid={})", sharer, token_id, nft_uuid);
        } else {
            warn!("handle_share: event has no data payload tx={:?} — skipping (producer bug?)", event.transaction_hash);
        }
        Ok(())
    }

    pub(super) async fn handle_royalty_distributed(&self, event: &BlockchainEvent) -> Result<()> {
        // SILENT-NULL-DATA-DROP: data=None on a royalty event is a producer-side bug.
        let data = match &event.data {
            Some(d) => d,
            None => {
                warn!("handle_royalty_distributed: event has no data payload tx={:?} — skipping",
                    event.transaction_hash);
                return Ok(());
            }
        };

        let token_id = data.get("tokenId").and_then(|v| v.as_str()).unwrap_or("");
        let recipient = data.get("recipient").and_then(|v| v.as_str()).unwrap_or("");
        let amount = data.get("amount").and_then(|v| v.as_str()).unwrap_or("");

        // EMPTY-USER-ADDRESS: reject empty recipient.
        if recipient.is_empty() {
            warn!("handle_royalty_distributed: missing recipient in event tx={:?}, skipping",
                event.transaction_hash);
            return Ok(());
        }

        // ROYALTY-AS-PURCHASE: do NOT record InteractionType::Purchase here. A royalty
        // distribution means the original creator received payment — it carries no intent
        // signal (the creator did not choose to buy). Recording it as Purchase would
        // accumulate phantom purchase interactions, biasing recs to keep recommending the
        // creator their own work and inflating total_purchases in user_preferences.
        // The royalty event is logged for audit; recommendation signals come from the
        // actual buyer's ContentPurchase event via handle_content_purchase.

        info!(
            "💰 Processed royalty distribution: {} received {} for token {} (no rec signal recorded)",
            recipient, amount, token_id
        );
        Ok(())
    }

    // ── Legacy event aliases ────────────────────────────────────────────────

    pub(super) async fn handle_legacy_mint(&self, event: &BlockchainEvent) -> Result<()> {
        self.handle_content_minted(event).await
    }

    pub(super) async fn handle_legacy_like(&self, event: &BlockchainEvent) -> Result<()> {
        self.handle_like(event, InteractionType::Like).await
    }

    pub(super) async fn handle_legacy_comment(&self, event: &BlockchainEvent) -> Result<()> {
        self.handle_comment(event).await
    }

    pub(super) async fn handle_legacy_purchase(&self, event: &BlockchainEvent) -> Result<()> {
        self.handle_content_purchase(event).await
    }

    // ── Private helpers ─────────────────────────────────────────────────────

    /// OUT-OF-ORDER-LIKE-UNLIKE guard: returns true when a more recent (higher block_number)
    /// like/unlike interaction for this (user, nft) pair is already recorded.
    ///
    /// Kafka rebalances can deliver Unlike (block N+1) before Like (block N). Without this
    /// guard the final Nebula edge state can diverge from on-chain truth.
    ///
    /// REQUIRES: ALTER TABLE user_interactions ADD COLUMN block_number BIGINT;
    /// and recorder.rs INSERT must bind event.block_number into the block_number column.
    /// Until the migration is applied, the query will fail and unwrap_or(0) returns false
    /// (same behavior as the stub), degrading gracefully with no data loss.
    async fn is_stale_like_event(&self, user_address: &str, nft_id: &Uuid, block_number: u64) -> bool {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM user_interactions \
             WHERE user_address = $1 AND nft_id = $2 \
             AND interaction_type IN ('like','unlike') AND block_number > $3"
        )
        .bind(user_address)
        .bind(nft_id)
        // i64::MAX as fallback: if block_number somehow overflows, guard returns false
        // (not stale) rather than silently treating every event as stale.
        .bind(i64::try_from(block_number).unwrap_or(i64::MAX))
        .fetch_one(&self.pool)
        .await
        .unwrap_or(0);
        count > 0
    }
}

/// Thin wrapper — delegates to the pub(crate) free-function form.
async fn enrich_and_record(
    ep: &EventDispatcher,
    nft_uuid: &Uuid,
    prefetched_meta: Option<NftMetadata>,
    user_address: &str,
    interaction_type: InteractionType,
    source: &str,
    event: &BlockchainEvent,
) -> crate::error::Result<()> {
    enrich_and_record_pools(
        &ep.pool,
        &ep.elixir_pool,
        ep.cache.as_ref(),
        nft_uuid,
        prefetched_meta,
        user_address,
        interaction_type,
        source,
        &event.contract_type,
        event,
    ).await
}

/// Free-function form — usable by DirectHandlers and any future caller outside EventProcessor.
///
/// Enriches from the Elixir DB (creator, tags, contract_type) then writes to the rec DB.
/// Tag enrichment failure is non-fatal: records with Degraded status for later repair.
pub(crate) async fn enrich_and_record_pools(
    rec_pool: &sqlx::PgPool,
    elixir_pool: &sqlx::PgPool,
    cache: Option<&crate::recommendation::cache::RecCache>,
    nft_uuid: &Uuid,
    prefetched_meta: Option<NftMetadata>,
    user_address: &str,
    interaction_type: InteractionType,
    source: &str,
    fallback_contract_type: &str,
    event: &BlockchainEvent,
) -> crate::error::Result<()> {
    use crate::recommendation::preferences::TagEnrichmentStatus;

    // POOL-005: callers that used lookup_nft_with_metadata already have the metadata
    // in hand — skip the second DB round-trip.  Callers without it (None) fall through
    // to the existing nft_metadata fetch so the behaviour is unchanged for them.
    let (contract_type, creator_addr, tags, enrichment) = if let Some(m) = prefetched_meta {
        (m.contract_type, m.creator_address, m.tags, TagEnrichmentStatus::Complete)
    } else {
        match super::elixir_db::nft_metadata(elixir_pool, nft_uuid).await {
            Ok(Some(m)) => (m.contract_type, m.creator_address, m.tags, TagEnrichmentStatus::Complete),
            _ => (
                fallback_contract_type.to_string(),
                String::new(),
                vec![],
                TagEnrichmentStatus::Degraded { reason: "NFT not yet indexed in Elixir DB".into() },
            ),
        }
    };

    record_interaction(
        rec_pool,
        InteractionEvent {
            user_address: user_address.to_string(),
            nft_id: nft_uuid.to_string(),
            interaction_type,
            view_duration_ms: None,
            source: Some(source.to_string()),
            nft_contract_type: Some(contract_type),
            nft_creator_address: if creator_addr.is_empty() { None } else { Some(creator_addr) },
            nft_tags: tags,
            tag_enrichment: enrichment,
            // NON-ATOMIC-RECORD-INTERACTION: pass a stable event_id so ON CONFLICT
            // (event_id) DO NOTHING in insert_interaction deduplicates Kafka redeliveries.
            // Combine tx_hash + event_type to stay unique when multiple event types share
            // a transaction (e.g. batch mints) — avoids false ON CONFLICT collisions.
            event_id: Some(format!("{}:{}", event.transaction_hash, event.event_type)),
            pct_played: None,
        },
        cache,
    )
    .await?;
    Ok(())
}

// ── Pure helpers (no I/O) — extracted for testability ─────────────────────────

/// Map the `bookmarked` boolean carried by a `ContentBookmarked` event to the
/// appropriate `InteractionType`.  Mirrors the inline logic in `handle_bookmark`.
#[allow(dead_code)]
pub(crate) fn bookmark_to_interaction(bookmarked: bool) -> InteractionType {
    if bookmarked {
        InteractionType::Save
    } else {
        InteractionType::Unsave
    }
}

/// Resolve the user field from a generic interaction data payload.
/// Falls back through "user" → "commenter" → "sharer" → "" and lowercases.
/// Returns `None` when all keys are absent or the result is empty.
#[allow(dead_code)]
pub(crate) fn extract_generic_user(data: &serde_json::Value) -> Option<String> {
    let raw = data
        .get("user")
        .or_else(|| data.get("commenter"))
        .or_else(|| data.get("sharer"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let lower = raw.to_lowercase();
    if lower.trim().is_empty() {
        None
    } else {
        Some(lower)
    }
}

/// Returns `true` when the interaction is a positive like (not an unlike).
/// Mirrors the `is_like` guard used before Nebula edge writes.
#[allow(dead_code)]
pub(crate) fn is_positive_like(t: &InteractionType) -> bool {
    matches!(t, InteractionType::Like)
}

// ── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests;
