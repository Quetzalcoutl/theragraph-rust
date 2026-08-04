//! Real-time Event Processor
//!
//! Kafka consumer loop + event dispatcher.
//! Handler logic lives in sub-modules:
//!   • `elixir_db`    — all cross-DB queries (Candidate 5 seam)
//!   • `interaction`  — events with recommendation signal (like, purchase, etc.)
//!   • `social`       — follow/unfollow graph writes

pub mod direct;
mod elixir_db;
pub mod graph_sync;
mod interaction;
pub mod reconciliation;
mod social;

pub use direct::DirectHandlers;

use crate::config::Config;
use crate::error::{Error, Result};
use crate::events::EventType;
use crate::kafka::BlockchainEvent;
use crate::recommendation::preferences::InteractionType;
use crate::AppState;
use rdkafka::config::ClientConfig;
use rdkafka::consumer::{CommitMode, Consumer, StreamConsumer};
use rdkafka::message::Message;
use sqlx::PgPool;
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc};
use tracing::{debug, error, info, instrument, warn};
use uuid::Uuid;

/// Task sent over the enrichment channel.
pub(super) struct EnrichmentTask {
    pub nft_uuid: Uuid,
    pub contract_address: String,
    pub token_id: i64,
}

/// Shared NFT metadata threaded through interaction handlers.
#[derive(Clone, Debug)]
pub(crate) struct NftMetadata {
    pub(crate) contract_type: String,
    pub(crate) creator_address: String,
    pub(crate) tags: Vec<String>,
}

/// Database row for NFT metadata lookups (Elixir DB).
#[derive(sqlx::FromRow)]
pub(crate) struct NftMetadataRow {
    pub(crate) contract_type: String,
    pub(crate) creator_address: String,
    pub(crate) tags: Vec<String>,
}

/// Consumes Kafka events and updates recommendation data in real time.
pub struct EventProcessor {
    consumer: StreamConsumer,
    pool: PgPool,           // rec DB — preferences, nft_features, interactions
    elixir_pool: PgPool,    // Elixir DB — nfts, social_users
    shutdown: broadcast::Receiver<()>,
    graph_sync: graph_sync::GraphSync<crate::recommendation::graph_client::DynGraphTransport>,
    cache: Option<crate::recommendation::cache::RecCache>,
    enrichment_tx: mpsc::Sender<EnrichmentTask>,
    enrichment_rx: Option<mpsc::Receiver<EnrichmentTask>>,
}

impl EventProcessor {
    pub fn new(
        config: &Config,
        pool: PgPool,
        elixir_pool: PgPool,
        shutdown: broadcast::Receiver<()>,
        graph_client: Arc<dyn crate::recommendation::graph_client::GraphTraversal>,
        cache: Option<crate::recommendation::cache::RecCache>,
    ) -> Result<Self> {
        let consumer: StreamConsumer = ClientConfig::new()
            .set("group.id", &config.kafka.group_id)
            .set("bootstrap.servers", &config.kafka.brokers)
            .set("enable.partition.eof", "false")
            .set("session.timeout.ms", "30000")
            .set("heartbeat.interval.ms", "10000")
            .set("request.timeout.ms", "60000")
            .set("socket.timeout.ms", "60000")
            .set("enable.auto.commit", "false")
            .set("auto.offset.reset", "earliest")
            .create()
            .map_err(|e| Error::kafka(format!("Failed to create consumer: {}", e)))?;

        consumer
            .subscribe(&["user.actions", "blockchain.events"])
            .map_err(|e| Error::kafka(format!("Failed to subscribe: {}", e)))?;

        let (enrichment_tx, enrichment_rx) = mpsc::channel(1024);

        Ok(Self {
            consumer,
            pool,
            elixir_pool,
            shutdown,
            graph_sync: graph_sync::GraphSync::new(
                crate::recommendation::graph_client::GraphClient::from_dyn_traversal(graph_client)
            ),
            cache,
            enrichment_tx,
            enrichment_rx: Some(enrichment_rx),
        })
    }

    /// Main event loop — reads Kafka, dispatches, commits on success.
    #[instrument(skip(self))]
    pub async fn run(mut self) -> Result<()> {
        info!("🎯 Starting real-time event processor");

        // DLQ table is created by migrations/006_create_failed_events.sql (DLQ-S23-01).

        // Spawn enrichment worker — drains the channel so mint handlers never block.
        let rx = match self.enrichment_rx.take() {
            Some(r) => r,
            None => {
                error!("EventProcessor::run() called more than once — aborting");
                return Err(Error::kafka("EventProcessor already running".to_string()));
            }
        };
        let pool = self.pool.clone();
        let elixir_pool = self.elixir_pool.clone();
        let cache = self.cache.clone();
        // BUG-001: save the JoinHandle so the main loop can detect an unexpected
        // exit (panic or task cancellation) and log it.  Dropping the handle
        // silently detaches the task — deaths were previously invisible.
        let mut enrichment_handle = tokio::spawn(async move {
            let mut rx = rx;
            while let Some(task) = rx.recv().await {
                elixir_db::process_enrichment(task, &pool, &elixir_pool, cache.as_ref()).await;
            }
            info!("Enrichment worker stopped (channel closed)");
        });
        // BUG-001: disable the enrichment-handle arm in select! after it fires
        // once so a completed JoinHandle doesn't spin in a tight loop.
        let mut enrichment_alive = true;

        loop {
            tokio::select! {
                message = self.consumer.recv() => {
                    match message {
                        Ok(msg) => {
                            match self.process_message(&msg).await {
                                Ok(()) => {
                                    if let Err(e) = self.consumer.commit_message(&msg, CommitMode::Async) {
                                        error!("Failed to commit offset: {:?}", e);
                                    }
                                }
                                Err(ref e) => {
                                    error!("Failed to process message: {:?}", e);
                                    // POOL-001: permanent errors (malformed JSON or invalid field
                                    // format) will never succeed on retry.  Commit-and-skip them
                                    // so a single poison-pill message doesn't stall the consumer
                                    // forever.  Transient errors (DB down, Nebula timeout) are NOT
                                    // committed — Kafka redelivers them on the next session.
                                    let is_permanent = matches!(e,
                                        Error::Json(_) | Error::InvalidFormat { .. }
                                    );
                                    if is_permanent {
                                        if let Err(ce) = self.consumer.commit_message(&msg, CommitMode::Async) {
                                            error!("Failed to commit offset for poison-pill: {:?}", ce);
                                        }
                                        // DLQ: extract fields while msg is in scope, write async
                                        // so the consumer loop never blocks on the INSERT.
                                        let dlq_pool = self.pool.clone();
                                        let dlq_topic = msg.topic().to_owned();
                                        let dlq_partition = msg.partition();
                                        let dlq_offset = msg.offset();
                                        let dlq_payload = msg
                                            .payload()
                                            .and_then(|b| std::str::from_utf8(b).ok())
                                            .map(str::to_owned);
                                        let dlq_error_type = match e {
                                            Error::Json(_) => "json_parse",
                                            Error::InvalidFormat { .. } => "invalid_format",
                                            _ => "permanent_unknown",
                                        }.to_owned();
                                        let dlq_error_msg = format!("{e}");
                                        // Inline with timeout — avoids a dropped JoinHandle
                                        // where a panic inside the spawned task would be silently swallowed.
                                        // 5s is generous for a simple INSERT on a pool that's already open.
                                        let dlq_result = tokio::time::timeout(
                                            std::time::Duration::from_secs(5),
                                            sqlx::query(
                                                "INSERT INTO failed_events \
                                                 (topic, partition, \"offset\", payload, \
                                                  error_type, error_message) \
                                                 VALUES ($1, $2, $3, $4, $5, $6)"
                                            )
                                            .bind(&dlq_topic)
                                            .bind(dlq_partition)
                                            .bind(dlq_offset)
                                            .bind(dlq_payload.as_deref())
                                            .bind(&dlq_error_type)
                                            .bind(&dlq_error_msg)
                                            .execute(&dlq_pool),
                                        ).await;
                                        match dlq_result {
                                            Ok(Err(de)) => error!("Failed to write to DLQ: {:?}", de),
                                            Err(_) => error!("DLQ write timed out — event lost"),
                                            Ok(Ok(_)) => {}
                                        }
                                    }
                                }
                            }
                        }
                        Err(e) => error!("Kafka consumer error: {:?}", e),
                    }
                }
                result = self.shutdown.recv() => {
                    match result {
                        Ok(_) | Err(broadcast::error::RecvError::Closed) => {
                            info!("Event processor shutting down");
                            break;
                        }
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            warn!("Shutdown channel lagged by {n} messages — continuing");
                        }
                    }
                }
                // BUG-001: monitor enrichment worker — log exit and disable
                // this select arm so the main loop continues for other events.
                result = &mut enrichment_handle, if enrichment_alive => {
                    enrichment_alive = false;
                    match result {
                        Ok(()) => {
                            error!(
                                "Enrichment worker exited unexpectedly — \
                                 NFT tag back-fill disabled until restart"
                            );
                        }
                        Err(ref e) if e.is_panic() => {
                            error!(
                                "Enrichment worker panicked: {e:?} — \
                                 NFT tag back-fill disabled until restart"
                            );
                        }
                        Err(e) => {
                            error!(
                                "Enrichment worker task error: {e:?} — \
                                 NFT tag back-fill disabled until restart"
                            );
                        }
                    }
                    // The Kafka consumer continues — enrichment failure is non-fatal
                    // for other event types (follow, like, purchase, etc.).
                }
            }
        }

        Ok(())
    }

    async fn process_message(
        &self,
        message: &rdkafka::message::BorrowedMessage<'_>,
    ) -> Result<()> {
        let payload = message
            .payload()
            .ok_or_else(|| Error::kafka("Empty message payload"))?;

        let event: BlockchainEvent = serde_json::from_slice(payload).map_err(Error::Json)?;
        self.process_event(&event).await
    }

    /// Map raw event type string to a handler. Unknown types are silently ignored.
    async fn process_event(&self, event: &BlockchainEvent) -> Result<()> {
        let event_type = match event.event_type.as_str() {
            "ContentMinted"              => EventType::ContentMinted,
            "ContentCopyMinted"          => EventType::ContentCopyMinted,
            "ContentLiked"               => EventType::ContentLiked,
            "ContentUnliked"             => EventType::ContentUnliked,
            "ContentCommented"           => EventType::ContentCommented,
            "ContentBookmarked"          => EventType::ContentBookmarked,
            "ContentShared"              => EventType::ContentShared,
            "UserFollowed"               => EventType::UserFollowed,
            "UserUnfollowed"             => EventType::UserUnfollowed,
            "UsernameRegistered"         => EventType::UsernameRegistered,
            "ProfileUpdated"             => EventType::ProfileUpdated,
            "ProfileUpdatedExtended"     => EventType::ProfileUpdatedExtended,
            "UserVerified"               => EventType::UserVerified,
            "UserBlocked"                => EventType::UserBlocked,
            "RoyaltyDistributed"         => EventType::RoyaltyDistributed,
            "EarningsWithdrawn"          => EventType::EarningsWithdrawn,
            "ContentRequirementsUpdated" => EventType::ContentRequirementsUpdated,
            "ContentBurned"              => EventType::ContentBurned,
            "BurnedContentRevenue"       => EventType::BurnedContentRevenue,
            "TreasuryUpdated"            => EventType::TreasuryUpdated,
            "DailyLimitsUpdated"         => EventType::DailyLimitsUpdated,
            "PricesUpdated"              => EventType::PricesUpdated,
            "TokensRecovered"            => EventType::TokensRecovered,
            "TipSent"                    => EventType::TipSent,
            "BadgeAwarded"               => EventType::BadgeAwarded,
            "SnapMinted"                 => EventType::SnapMinted,
            "ArtMinted"                  => EventType::ArtMinted,
            "MusicMinted"                => EventType::MusicMinted,
            "FlixMinted"                 => EventType::FlixMinted,
            "SnapLiked"                  => EventType::SnapLiked,
            "ArtLiked"                   => EventType::ArtLiked,
            "MusicLiked"                 => EventType::MusicLiked,
            "FlixLiked"                  => EventType::FlixLiked,
            "SnapCommented"              => EventType::SnapCommented,
            "ArtCommented"               => EventType::ArtCommented,
            "MusicCommented"             => EventType::MusicCommented,
            "FlixCommented"              => EventType::FlixCommented,
            "SnapBoughtAndMinted"        => EventType::SnapBoughtAndMinted,
            "ArtBoughtAndMinted"         => EventType::ArtBoughtAndMinted,
            "MusicBoughtAndMinted"       => EventType::MusicBoughtAndMinted,
            "FlixBoughtAndMinted"        => EventType::FlixBoughtAndMinted,
            _ => {
                debug!("Ignoring unknown event type: {}", event.event_type);
                return Ok(());
            }
        };

        match event_type {
            // ── Content ────────────────────────────────────────────────────
            EventType::ContentMinted     => self.handle_content_minted(event).await,
            EventType::ContentCopyMinted => self.handle_content_purchase(event).await,

            // ── Interactions ───────────────────────────────────────────────
            EventType::ContentLiked       => self.handle_like(event, InteractionType::Like).await,
            EventType::ContentUnliked     => self.handle_like(event, InteractionType::Unlike).await,
            EventType::ContentCommented   => self.handle_comment(event).await,
            EventType::ContentBookmarked  => self.handle_bookmark(event).await,
            EventType::ContentShared      => self.handle_share(event).await,
            EventType::RoyaltyDistributed => self.handle_royalty_distributed(event).await,

            // ── Social graph ───────────────────────────────────────────────
            EventType::UserFollowed   => self.handle_follow(event).await,
            EventType::UserUnfollowed => self.handle_unfollow(event).await,

            // ── Observability-only (no recommendation signal) ──────────────
            EventType::UsernameRegistered         => self.log_username_registration(event),
            EventType::ProfileUpdated             => self.log_profile_update(event),
            EventType::ProfileUpdatedExtended     => self.log_profile_update_extended(event),
            EventType::UserVerified               => self.log_user_verified(event),
            EventType::UserBlocked                => self.log_user_blocked(event),
            EventType::TipSent                    => self.log_tip(event),
            EventType::BadgeAwarded               => self.log_badge(event),
            EventType::EarningsWithdrawn          => self.log_earnings_withdrawn(event),
            EventType::ContentRequirementsUpdated => self.log_content_requirements_updated(event),
            EventType::ContentBurned              => self.log_content_burned(event),
            EventType::BurnedContentRevenue       => self.log_burned_content_revenue(event),
            EventType::TreasuryUpdated            => self.log_treasury_updated(event),
            EventType::DailyLimitsUpdated         => self.log_daily_limits_updated(event),
            EventType::PricesUpdated              => self.log_prices_updated(event),
            EventType::TokensRecovered            => self.log_tokens_recovered(event),

            // ── Legacy aliases ─────────────────────────────────────────────
            EventType::SnapMinted | EventType::ArtMinted
            | EventType::MusicMinted | EventType::FlixMinted
                => self.handle_legacy_mint(event).await,

            EventType::SnapLiked | EventType::ArtLiked
            | EventType::MusicLiked | EventType::FlixLiked
                => self.handle_legacy_like(event).await,

            EventType::SnapCommented | EventType::ArtCommented
            | EventType::MusicCommented | EventType::FlixCommented
                => self.handle_legacy_comment(event).await,

            EventType::SnapBoughtAndMinted | EventType::ArtBoughtAndMinted
            | EventType::MusicBoughtAndMinted | EventType::FlixBoughtAndMinted
                => self.handle_legacy_purchase(event).await,

            _ => Ok(()),
        }
    }

    // ── Observability-only handlers (log and return Ok) ────────────────────

    fn log_username_registration(&self, event: &BlockchainEvent) -> Result<()> {
        if let Some(d) = &event.data {
            info!("👤 Username registered: {} → {}",
                d.get("user").and_then(|v| v.as_str()).unwrap_or(""),
                d.get("username").and_then(|v| v.as_str()).unwrap_or(""));
        }
        Ok(())
    }

    fn log_profile_update(&self, event: &BlockchainEvent) -> Result<()> {
        if let Some(d) = &event.data {
            info!("📝 Profile update: {}", d.get("user").and_then(|v| v.as_str()).unwrap_or(""));
        }
        Ok(())
    }

    fn log_profile_update_extended(&self, event: &BlockchainEvent) -> Result<()> {
        if let Some(d) = &event.data {
            info!("📝 ProfileUpdatedExtended: {} (username={}, hash={}, bio={}, website={})",
                d.get("user").and_then(|v| v.as_str()).unwrap_or(""),
                d.get("username").and_then(|v| v.as_str()).unwrap_or(""),
                d.get("profile_hash").and_then(|v| v.as_str()).unwrap_or(""),
                d.get("bio").and_then(|v| v.as_str()).unwrap_or(""),
                d.get("website").and_then(|v| v.as_str()).unwrap_or(""));
        }
        Ok(())
    }

    fn log_tip(&self, event: &BlockchainEvent) -> Result<()> {
        if let Some(d) = &event.data {
            info!("💸 Tip: {} sent {} to {}",
                d.get("from").and_then(|v| v.as_str()).unwrap_or(""),
                d.get("amount").and_then(|v| v.as_u64()).unwrap_or(0),
                d.get("to").and_then(|v| v.as_str()).unwrap_or(""));
        }
        Ok(())
    }

    fn log_badge(&self, event: &BlockchainEvent) -> Result<()> {
        if let Some(d) = &event.data {
            info!("🏆 Badge: {} earned {}",
                d.get("user").and_then(|v| v.as_str()).unwrap_or(""),
                d.get("badgeType").and_then(|v| v.as_str()).unwrap_or(""));
        }
        Ok(())
    }

    fn log_user_verified(&self, event: &BlockchainEvent) -> Result<()> {
        if let Some(d) = &event.data {
            info!("✅ User verified: {}", d.get("user").and_then(|v| v.as_str()).unwrap_or(""));
        }
        Ok(())
    }

    fn log_user_blocked(&self, event: &BlockchainEvent) -> Result<()> {
        if let Some(d) = &event.data {
            info!("🚫 Block: {} blocked {}",
                d.get("blockedBy").and_then(|v| v.as_str()).unwrap_or(""),
                d.get("user").and_then(|v| v.as_str()).unwrap_or(""));
        }
        Ok(())
    }

    fn log_earnings_withdrawn(&self, event: &BlockchainEvent) -> Result<()> {
        if let Some(d) = &event.data {
            info!("💸 Earnings withdrawn: {} withdrew {}",
                d.get("user").and_then(|v| v.as_str()).unwrap_or(""),
                d.get("amount").and_then(|v| v.as_str()).unwrap_or(""));
        }
        Ok(())
    }

    fn log_content_requirements_updated(&self, event: &BlockchainEvent) -> Result<()> {
        if let Some(d) = &event.data {
            info!("📋 Content requirements updated by: {}",
                d.get("updater").and_then(|v| v.as_str()).unwrap_or(""));
        }
        Ok(())
    }

    fn log_content_burned(&self, event: &BlockchainEvent) -> Result<()> {
        if let Some(d) = &event.data {
            info!("🔥 Content burned: {} burned token {}",
                d.get("burner").and_then(|v| v.as_str()).unwrap_or(""),
                d.get("tokenId").and_then(|v| v.as_str()).unwrap_or(""));
        }
        Ok(())
    }

    fn log_burned_content_revenue(&self, event: &BlockchainEvent) -> Result<()> {
        if let Some(d) = &event.data {
            info!("💰 Burned content revenue: {} received {}",
                d.get("recipient").and_then(|v| v.as_str()).unwrap_or(""),
                d.get("amount").and_then(|v| v.as_str()).unwrap_or(""));
        }
        Ok(())
    }

    fn log_treasury_updated(&self, event: &BlockchainEvent) -> Result<()> {
        if let Some(d) = &event.data {
            info!("🏦 Treasury update: {} performed {}",
                d.get("updater").and_then(|v| v.as_str()).unwrap_or(""),
                d.get("action").and_then(|v| v.as_str()).unwrap_or(""));
        }
        Ok(())
    }

    fn log_daily_limits_updated(&self, event: &BlockchainEvent) -> Result<()> {
        if let Some(d) = &event.data {
            info!("📊 Daily limits updated by: {}",
                d.get("updater").and_then(|v| v.as_str()).unwrap_or(""));
        }
        Ok(())
    }

    fn log_prices_updated(&self, event: &BlockchainEvent) -> Result<()> {
        if let Some(d) = &event.data {
            info!("💰 Prices updated by: {}",
                d.get("updater").and_then(|v| v.as_str()).unwrap_or(""));
        }
        Ok(())
    }

    fn log_tokens_recovered(&self, event: &BlockchainEvent) -> Result<()> {
        if let Some(d) = &event.data {
            info!("🔄 Tokens recovered: {} recovered {}",
                d.get("recoverer").and_then(|v| v.as_str()).unwrap_or(""),
                d.get("amount").and_then(|v| v.as_str()).unwrap_or(""));
        }
        Ok(())
    }
}

/// Spawn the event processor task. Parks until shutdown if Kafka is disabled.
pub fn spawn_event_processor(state: Arc<AppState>) -> tokio::task::JoinHandle<()> {
    let shutdown_rx = state.shutdown.subscribe();

    tokio::spawn(async move {
        if !state.config.kafka.enabled {
            info!("Kafka disabled (KAFKA_ENABLED=false) — event processor not started");
            let mut rx = shutdown_rx;
            let _ = rx.recv().await;
            return;
        }

        let processor = match EventProcessor::new(
            &state.config,
            state.db.pool().clone(),
            state.elixir_db.pool().clone(),
            shutdown_rx,
            Arc::clone(&state.graph_client),
            state.rec_cache.clone(),
        ) {
            Ok(p) => p,
            Err(e) => {
                error!("Failed to create event processor: {:?}", e);
                return;
            }
        };

        if let Err(e) = processor.run().await {
            error!("Event processor failed: {:?}", e);
        }
    })
}
