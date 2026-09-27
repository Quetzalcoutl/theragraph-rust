//! Real-time Event Processor
//!
//! Kafka consumer loop + event dispatcher.
//! Handler logic lives in sub-modules:
//!   • `elixir_db`    — all cross-DB queries (Candidate 5 seam)
//!   • `interaction`  — events with recommendation signal (like, purchase, comment, etc.)
//!
//! The unified TheraFriendz contract's `UserFollowed`/`UserUnfollowed` events were
//! retired in the lazy-mint rewrite (follow is off-chain now) — the `social`
//! sub-module (follow/unfollow Kafka handlers) was removed along with them. The
//! legacy per-contract `Followed`/`Unfollowed` events were never wired into this
//! Kafka dispatch table to begin with (only into `DirectHandlers`), so nothing
//! else depended on it.

pub mod direct;
pub(crate) mod elixir_db;
pub mod graph_sync;
mod interaction;
pub mod purchase_signals;
pub mod reconciliation;

pub use direct::DirectHandlers;

use crate::config::Config;
use crate::error::{Error, Result};
use crate::kafka::BlockchainEvent;
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
///
/// `contract_address` is `Box<str>` not `String`: frozen at construction (moved
/// once through the channel, never mutated), and this queue can hold up to
/// `CHANNEL_CAPACITY` (1024) live tasks at once if the enrichment worker falls
/// behind — unlike most of this module's per-event scratch state, this one
/// genuinely accumulates. Currently the sole indexed contract means every
/// queued task likely repeats the same ~42-byte address; `Box<str>` alone
/// still saves the 8-byte capacity field per queued task regardless.
pub(super) struct EnrichmentTask {
    pub nft_uuid: Uuid,
    pub contract_address: Box<str>,
    pub token_id: i64,
}

/// Shared NFT metadata threaded through interaction handlers.
#[derive(Clone, Debug)]
pub(crate) struct NftMetadata {
    pub(crate) contract_type: String,
    pub(crate) creator_address: String,
    pub(crate) tags: Vec<String>,
}

// ─── EventDispatcher — testable without Kafka ─────────────────────────────────
//
// Contains only domain dependencies (pools, graph, cache).  No Kafka consumer,
// no shutdown channel.  `dispatch()` is the pure routing entry point; it can
// be constructed and tested with real or test pools without standing up Kafka.

pub(crate) struct EventDispatcher {
    pub(super) pool:           PgPool,
    pub(super) elixir_pool:   PgPool,
    pub(super) graph_sync:    graph_sync::GraphSync<crate::recommendation::graph_client::DynGraphTransport>,
    pub(super) cache:         Option<crate::recommendation::cache::RecCache>,
    pub(super) enrichment_tx: mpsc::Sender<EnrichmentTask>,
}

impl EventDispatcher {
    /// Map raw event type string directly to a handler. Unknown types are silently ignored.
    pub(super) async fn dispatch(&self, event: &BlockchainEvent) -> Result<()> {
        match event.event_type.as_str() {
            // ── Content ────────────────────────────────────────────────────
            "ContentMinted"     => self.handle_content_minted(event).await,
            "ContentCopyMinted" => self.handle_content_purchase(event).await,
            // Lazy-mint economy (2026-09-13): no longer observability-only —
            // persists price/max_copies onto the Nebula post vertex.
            "ListingUpdated"    => self.handle_listing_updated(event).await,

            // ── Interactions ───────────────────────────────────────────────
            "ContentBookmarked"  => self.handle_bookmark(event).await,
            "ContentShared"      => self.handle_share(event).await,
            "RoyaltyDistributed" => self.handle_royalty_distributed(event).await,

            // ── Observability-only (no recommendation signal) ──────────────
            "UsernameRegistered"         => self.log_username_registration(event),
            "ProfileUpdated"             => self.log_profile_update(event),
            "ProfileUpdatedExtended"     => self.log_profile_update_extended(event),
            "UserVerified"               => self.log_user_verified(event),
            "UserBlocked"                => self.log_user_blocked(event),
            "TipSent"                    => self.log_tip(event),
            "BadgeAwarded"               => self.log_badge(event),
            "EarningsWithdrawn"          => self.log_earnings_withdrawn(event),
            "ContentBurned"              => self.log_content_burned(event),
            "BurnedContentRevenue"       => self.log_burned_content_revenue(event),
            "TreasuryUpdated"            => self.log_treasury_updated(event),
            "PlatformFeeUpdated"         => self.log_platform_fee_updated(event),
            "TokensRecovered"            => self.log_tokens_recovered(event),

            // ── Legacy aliases ─────────────────────────────────────────────
            "SnapMinted" | "ArtMinted" | "MusicMinted" | "FlixMinted"
                => self.handle_legacy_mint(event).await,
            "SnapLiked" | "ArtLiked" | "MusicLiked" | "FlixLiked"
                => self.handle_legacy_like(event).await,
            "SnapCommented" | "ArtCommented" | "MusicCommented" | "FlixCommented"
                => self.handle_legacy_comment(event).await,
            "SnapBoughtAndMinted" | "ArtBoughtAndMinted" | "MusicBoughtAndMinted" | "FlixBoughtAndMinted"
                => self.handle_legacy_purchase(event).await,

            _ => {
                debug!("Ignoring unknown event type: {}", event.event_type);
                Ok(())
            }
        }
    }
}

// ─── DLQ writer — isolated from Kafka message lifetime ───────────────────────

/// Persist a failed event to the dead-letter queue.
///
/// Extracted from the consumer loop so it can be tested independently and
/// so the schema knowledge lives in one place.
async fn write_failed_event(
    pool:       &PgPool,
    topic:      &str,
    partition:  i32,
    offset:     i64,
    payload:    Option<&str>,
    error_type: &str,
    error_msg:  &str,
) {
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        sqlx::query(
            "INSERT INTO failed_events \
             (topic, partition, \"offset\", payload, \
              error_type, error_message) \
             VALUES ($1, $2, $3, $4, $5, $6)"
        )
        .bind(topic)
        .bind(partition)
        .bind(offset)
        .bind(payload)
        .bind(error_type)
        .bind(error_msg)
        .execute(pool),
    )
    .await;

    match result {
        Ok(Err(de)) => error!("Failed to write to DLQ: {:?}", de),
        Err(_)      => error!("DLQ write timed out — event lost"),
        Ok(Ok(_))   => {}
    }
}

// ─── EventProcessor — thin Kafka wiring ──────────────────────────────────────

/// Consumes Kafka events and dispatches to EventDispatcher.
/// Only infra concerns live here; all domain logic is in EventDispatcher.
pub struct EventProcessor {
    consumer:       StreamConsumer,
    shutdown:       broadcast::Receiver<()>,
    enrichment_rx:  Option<mpsc::Receiver<EnrichmentTask>>,
    dispatcher:     EventDispatcher,
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
            shutdown,
            enrichment_rx: Some(enrichment_rx),
            dispatcher: EventDispatcher {
                pool,
                elixir_pool,
                graph_sync: graph_sync::GraphSync::new(
                    crate::recommendation::graph_client::GraphClient::from_dyn_traversal(graph_client)
                ),
                cache,
                enrichment_tx,
            },
        })
    }

    /// Main event loop — reads Kafka, dispatches, commits on success.
    #[instrument(skip(self))]
    pub async fn run(mut self) -> Result<()> {
        info!("🎯 Starting real-time event processor");

        let rx = match self.enrichment_rx.take() {
            Some(r) => r,
            None => {
                error!("EventProcessor::run() called more than once — aborting");
                return Err(Error::kafka("EventProcessor already running".to_string()));
            }
        };
        let pool        = self.dispatcher.pool.clone();
        let elixir_pool = self.dispatcher.elixir_pool.clone();
        let cache       = self.dispatcher.cache.clone();
        let mut enrichment_handle = tokio::spawn(async move {
            let mut rx = rx;
            while let Some(task) = rx.recv().await {
                elixir_db::process_enrichment(task, &pool, &elixir_pool, cache.as_ref()).await;
            }
            info!("Enrichment worker stopped (channel closed)");
        });
        let mut enrichment_alive = true;

        loop {
            tokio::select! {
                message = self.consumer.recv() => {
                    match message {
                        Ok(msg) => {
                            let payload_str = msg
                                .payload()
                                .and_then(|b| std::str::from_utf8(b).ok())
                                .map(str::to_owned);

                            match self.process_message(&msg).await {
                                Ok(()) => {
                                    if let Err(e) = self.consumer.commit_message(&msg, CommitMode::Async) {
                                        error!("Failed to commit offset: {:?}", e);
                                    }
                                }
                                Err(ref e) => {
                                    error!("Failed to process message: {:?}", e);
                                    let is_permanent = matches!(e,
                                        Error::Json(_) | Error::InvalidFormat { .. }
                                    );
                                    if is_permanent {
                                        if let Err(ce) = self.consumer.commit_message(&msg, CommitMode::Async) {
                                            error!("Failed to commit offset for poison-pill: {:?}", ce);
                                        }
                                        let error_type = match e {
                                            Error::Json(_)              => "json_parse",
                                            Error::InvalidFormat { .. } => "invalid_format",
                                            _                           => "permanent_unknown",
                                        };
                                        write_failed_event(
                                            &self.dispatcher.pool,
                                            msg.topic(),
                                            msg.partition(),
                                            msg.offset(),
                                            payload_str.as_deref(),
                                            error_type,
                                            &format!("{e}"),
                                        )
                                        .await;
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
        self.dispatcher.dispatch(&event).await
    }
}

mod observability;


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
