//! Real-time Event Processor
//!
//! Consumes blockchain events from Kafka and updates recommendation data:
//! - User preferences based on interactions
//! - NFT features and engagement scores
//! - Trending calculations
//!
//! This ensures the recommendation engine has fresh data for personalization.

use crate::config::Config;
use crate::error::{Error, Result};
use crate::events::EventType;
use crate::kafka::BlockchainEvent;
use crate::recommendation::preferences::{record_interaction, InteractionEvent, InteractionType};
use crate::AppState;
use rdkafka::config::ClientConfig;
use rdkafka::consumer::{Consumer, StreamConsumer};
use rdkafka::message::Message;
use sqlx::PgPool;
use std::sync::Arc;
use tokio::sync::broadcast;
use tracing::{debug, error, info, instrument};
use uuid::Uuid;

/// Event processor that consumes Kafka events and updates recommendations
pub struct EventProcessor {
    consumer: StreamConsumer,
    pool: PgPool,
    _elixir_pool: PgPool,
    shutdown: broadcast::Receiver<()>,
}

impl EventProcessor {
    /// Create a new event processor
    pub fn new(
        config: &Config,
        pool: PgPool,
        elixir_pool: PgPool,
        shutdown: broadcast::Receiver<()>,
    ) -> Result<Self> {
        let consumer: StreamConsumer = ClientConfig::new()
            .set("group.id", &config.kafka.group_id)
            .set("bootstrap.servers", &config.kafka.brokers)
            .set("enable.partition.eof", "false")
            .set("session.timeout.ms", "6000")
            .set("enable.auto.commit", "true")
            .set("auto.offset.reset", "latest")
            .create()
            .map_err(|e| Error::kafka(format!("Failed to create consumer: {}", e)))?;

        // Subscribe to relevant topics
        consumer
            .subscribe(&["user.actions", "blockchain.events"])
            .map_err(|e| Error::kafka(format!("Failed to subscribe: {}", e)))?;

        Ok(Self {
            consumer,
            pool,
            _elixir_pool: elixir_pool,
            shutdown,
        })
    }

    /// Generate a consistent UUID for an NFT based on contract address and token ID
    fn generate_nft_uuid(contract_address: &str, token_id: &str) -> Uuid {
        // Create a deterministic UUID from contract_address + token_id
        // This ensures the same NFT always gets the same UUID
        let combined = format!("{}:{}", contract_address.to_lowercase(), token_id);
        Uuid::new_v5(&Uuid::NAMESPACE_OID, combined.as_bytes())
    }

    /// Run the event processor
    #[instrument(skip(self))]
    pub async fn run(mut self) -> Result<()> {
        info!("🎯 Starting real-time event processor");

        loop {
            tokio::select! {
                message = self.consumer.recv() => {
                    match message {
                        Ok(msg) => {
                            if let Err(e) = self.process_message(&msg).await {
                                error!("Failed to process message: {:?}", e);
                            }
                        }
                        Err(e) => {
                            error!("Kafka consumer error: {:?}", e);
                        }
                    }
                }
                _ = self.shutdown.recv() => {
                    info!("Event processor shutting down");
                    break;
                }
            }
        }

        Ok(())
    }

    /// Process a single Kafka message
    async fn process_message(&self, message: &rdkafka::message::BorrowedMessage<'_>) -> Result<()> {
        let payload = message
            .payload()
            .ok_or_else(|| Error::kafka("Empty message payload"))?;

        let event: BlockchainEvent = serde_json::from_slice(payload).map_err(Error::Json)?;

        self.process_event(&event).await
    }

    /// Process a blockchain event and update recommendation data
    async fn process_event(&self, event: &BlockchainEvent) -> Result<()> {
        // Parse event type from string
        let event_type = match event.event_type.as_str() {
            "ContentMinted" => EventType::ContentMinted,
            "ContentCopyMinted" => EventType::ContentCopyMinted,
            "ContentLiked" => EventType::ContentLiked,
            "ContentUnliked" => EventType::ContentUnliked,
            "ContentCommented" => EventType::ContentCommented,
            "ContentBookmarked" => EventType::ContentBookmarked,
            "ContentShared" => EventType::ContentShared,
            "UserFollowed" => EventType::UserFollowed,
            "UserUnfollowed" => EventType::UserUnfollowed,
            "UsernameRegistered" => EventType::UsernameRegistered,
            "ProfileUpdated" => EventType::ProfileUpdated,
            "ProfileUpdatedExtended" => EventType::ProfileUpdatedExtended,
            "UserVerified" => EventType::UserVerified,
            "UserBlocked" => EventType::UserBlocked,
            "RoyaltyDistributed" => EventType::RoyaltyDistributed,
            "EarningsWithdrawn" => EventType::EarningsWithdrawn,
            "ContentRequirementsUpdated" => EventType::ContentRequirementsUpdated,
            "ContentBurned" => EventType::ContentBurned,
            "TreasuryUpdated" => EventType::TreasuryUpdated,
            "DailyLimitsUpdated" => EventType::DailyLimitsUpdated,
            "PricesUpdated" => EventType::PricesUpdated,
            "TokensRecovered" => EventType::TokensRecovered,
            "TipSent" => EventType::TipSent,
            "BadgeAwarded" => EventType::BadgeAwarded,
            "SnapMinted" => EventType::SnapMinted,
            "ArtMinted" => EventType::ArtMinted,
            "MusicMinted" => EventType::MusicMinted,
            "FlixMinted" => EventType::FlixMinted,
            "SnapLiked" => EventType::SnapLiked,
            "ArtLiked" => EventType::ArtLiked,
            "MusicLiked" => EventType::MusicLiked,
            "FlixLiked" => EventType::FlixLiked,
            "SnapCommented" => EventType::SnapCommented,
            "ArtCommented" => EventType::ArtCommented,
            "MusicCommented" => EventType::MusicCommented,
            "FlixCommented" => EventType::FlixCommented,
            "SnapBoughtAndMinted" => EventType::SnapBoughtAndMinted,
            "ArtBoughtAndMinted" => EventType::ArtBoughtAndMinted,
            "MusicBoughtAndMinted" => EventType::MusicBoughtAndMinted,
            "FlixBoughtAndMinted" => EventType::FlixBoughtAndMinted,
            _ => {
                debug!("Ignoring unknown event type: {}", event.event_type);
                return Ok(());
            }
        };

        match event_type {
            // Content creation events
            EventType::ContentMinted => self.handle_content_minted(event).await,
            EventType::ContentCopyMinted => self.handle_content_purchase(event).await,

            // Social interaction events
            EventType::ContentLiked => self.handle_like(event, InteractionType::Like).await,
            EventType::ContentUnliked => self.handle_like(event, InteractionType::Unlike).await,
            EventType::ContentCommented => self.handle_comment(event).await,
            EventType::ContentBookmarked => self.handle_bookmark(event).await,
            EventType::ContentShared => self.handle_share(event).await,

            // Social relationship events
            EventType::UserFollowed => self.handle_follow(event).await,
            EventType::UserUnfollowed => self.handle_unfollow(event).await,

            // User profile events
            EventType::UsernameRegistered => self.handle_username_registration(event).await,
            EventType::ProfileUpdated => self.handle_profile_update(event).await,
            EventType::ProfileUpdatedExtended => self.handle_profile_update_extended(event).await,
            EventType::UserVerified => self.handle_user_verified(event).await,
            EventType::UserBlocked => self.handle_user_blocked(event).await,

            // Financial events
            EventType::RoyaltyDistributed => self.handle_royalty_distributed(event).await,
            EventType::EarningsWithdrawn => self.handle_earnings_withdrawn(event).await,
            EventType::TipSent => self.handle_tip(event).await,

            // Administrative events
            EventType::ContentRequirementsUpdated => {
                self.handle_content_requirements_updated(event).await
            }
            EventType::ContentBurned => self.handle_content_burned(event).await,
            EventType::BurnedContentRevenue => self.handle_burned_content_revenue(event).await,
            EventType::TreasuryUpdated => self.handle_treasury_updated(event).await,
            EventType::DailyLimitsUpdated => self.handle_daily_limits_updated(event).await,
            EventType::PricesUpdated => self.handle_prices_updated(event).await,
            EventType::TokensRecovered => self.handle_tokens_recovered(event).await,

            // Other social events
            EventType::BadgeAwarded => self.handle_badge(event).await,

            // Legacy events (for backward compatibility)
            EventType::SnapMinted
            | EventType::ArtMinted
            | EventType::MusicMinted
            | EventType::FlixMinted => self.handle_legacy_mint(event).await,
            EventType::SnapLiked
            | EventType::ArtLiked
            | EventType::MusicLiked
            | EventType::FlixLiked => self.handle_legacy_like(event).await,
            EventType::SnapCommented
            | EventType::ArtCommented
            | EventType::MusicCommented
            | EventType::FlixCommented => self.handle_legacy_comment(event).await,
            EventType::SnapBoughtAndMinted
            | EventType::ArtBoughtAndMinted
            | EventType::MusicBoughtAndMinted
            | EventType::FlixBoughtAndMinted => self.handle_legacy_purchase(event).await,

            // Events we don't process for recommendations
            _ => Ok(()),
        }
    }

    async fn handle_content_minted(&self, event: &BlockchainEvent) -> Result<()> {
        // Extract NFT metadata and create/update NFT features
        if let Some(data) = &event.data {
            let token_id_str = data.get("tokenId").and_then(|v| v.as_str()).unwrap_or("");
            let creator = data.get("creator").and_then(|v| v.as_str()).unwrap_or("");
            let _content_type = data
                .get("contentType")
                .and_then(|v| v.as_u64())
                .map(|ct| match ct {
                    0 => "art",
                    1 => "flix",
                    2 => "music",
                    3 => "snap",
                    _ => "unknown",
                })
                .unwrap_or("unknown");

            let token_id = data
                .get("tokenId")
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse::<i64>().ok())
                .unwrap_or(0);

            // Generate consistent UUID for this NFT
            let nft_uuid = Self::generate_nft_uuid(&event.contract_address, token_id_str);

            // Insert or update NFT features
            sqlx::query(
                r#"
                INSERT INTO nft_features (nft_id, contract_address, token_id, tags, inserted_at, updated_at)
                VALUES ($1, $2, $3, $4, NOW(), NOW())
                ON CONFLICT (nft_id) DO UPDATE SET
                    tags = EXCLUDED.tags,
                    updated_at = NOW()
                "#
            )
            .bind(nft_uuid)
            .bind(&event.contract_address)
            .bind(token_id)
            .bind(Vec::<String>::new())
            .execute(&self.pool)
            .await
            .map_err(|e| Error::Database {
                message: "Failed to update NFT features".into(),
                source: Some(e),
            })?;

            info!("📝 Processed content mint: {} by {}", nft_uuid, creator);
        }
        Ok(())
    }

    async fn handle_content_purchase(&self, event: &BlockchainEvent) -> Result<()> {
        if let Some(data) = &event.data {
            let buyer = data.get("buyer").and_then(|v| v.as_str()).unwrap_or("");
            let original_id = data
                .get("originalId")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let new_token_id = data
                .get("newTokenId")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            // Generate UUID for the purchased NFT
            let nft_uuid = Self::generate_nft_uuid(&event.contract_address, new_token_id);

            // Record purchase interaction
            let interaction = InteractionEvent {
                user_address: buyer.to_string(),
                nft_id: nft_uuid.to_string(),
                interaction_type: InteractionType::Purchase,
                view_duration_ms: None,
                source: Some("marketplace".to_string()),
                nft_contract_type: Some(event.contract_type.clone()),
                nft_creator_address: None, // TODO: Look up from NFT
                nft_tags: vec![],
            };

            record_interaction(&self.pool, interaction).await?;

            info!(
                "💰 Processed content purchase: {} bought copy of {}",
                buyer, original_id
            );
        }
        Ok(())
    }

    async fn handle_like(
        &self,
        event: &BlockchainEvent,
        interaction_type: InteractionType,
    ) -> Result<()> {
        if let Some(data) = &event.data {
            let liker = data.get("liker").and_then(|v| v.as_str()).unwrap_or("");
            let token_id = data.get("tokenId").and_then(|v| v.as_str()).unwrap_or("");
            let creator = data.get("creator").and_then(|v| v.as_str()).unwrap_or("");

            // Generate UUID for the NFT
            let nft_uuid = Self::generate_nft_uuid(&event.contract_address, token_id);

            // Determine if this is a like (increment) or unlike (decrement)
            let _is_like = interaction_type == InteractionType::Like;

            let interaction = InteractionEvent {
                user_address: liker.to_string(),
                nft_id: nft_uuid.to_string(),
                interaction_type,
                view_duration_ms: None,
                source: Some("feed".to_string()),
                nft_contract_type: Some(event.contract_type.clone()),
                nft_creator_address: Some(creator.to_string()),
                nft_tags: vec![], // TODO: Look up from NFT features
            };

            record_interaction(&self.pool, interaction).await?;

            // self.update_nft_likes_count(&event.contract_address, token_id, is_like).await?;

            info!(
                "👍 Processed {}: {} on {}",
                event.event_type, liker, token_id
            );
        }
        Ok(())
    }

    async fn handle_comment(&self, event: &BlockchainEvent) -> Result<()> {
        if let Some(data) = &event.data {
            let commenter = data.get("commenter").and_then(|v| v.as_str()).unwrap_or("");
            let token_id = data.get("tokenId").and_then(|v| v.as_str()).unwrap_or("");
            let _comment = data.get("comment").and_then(|v| v.as_str()).unwrap_or("");

            // Generate UUID for the NFT
            let nft_uuid = Self::generate_nft_uuid(&event.contract_address, token_id);

            let interaction = InteractionEvent {
                user_address: commenter.to_string(),
                nft_id: nft_uuid.to_string(),
                interaction_type: InteractionType::Comment,
                view_duration_ms: None,
                source: Some("feed".to_string()),
                nft_contract_type: Some(event.contract_type.clone()),
                nft_creator_address: None,
                nft_tags: vec![],
            };

            record_interaction(&self.pool, interaction).await?;

            // self.update_nft_comments_count(&event.contract_address, token_id, true).await?;

            info!("💬 Processed comment: {} on {}", commenter, token_id);
        }
        Ok(())
    }

    async fn handle_bookmark(&self, event: &BlockchainEvent) -> Result<()> {
        if let Some(data) = &event.data {
            let user = data.get("user").and_then(|v| v.as_str()).unwrap_or("");
            let token_id = data.get("tokenId").and_then(|v| v.as_str()).unwrap_or("");
            let bookmarked = data
                .get("bookmarked")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            // Generate UUID for the NFT
            let nft_uuid = Self::generate_nft_uuid(&event.contract_address, token_id);

            let interaction_type = if bookmarked {
                InteractionType::Save
            } else {
                InteractionType::Unsave
            };

            let interaction = InteractionEvent {
                user_address: user.to_string(),
                nft_id: nft_uuid.to_string(),
                interaction_type,
                view_duration_ms: None,
                source: Some("feed".to_string()),
                nft_contract_type: Some(event.contract_type.clone()),
                nft_creator_address: None,
                nft_tags: vec![],
            };

            record_interaction(&self.pool, interaction).await?;

            info!(
                "🔖 Processed bookmark: {} {} {}",
                user,
                if bookmarked { "saved" } else { "unsaved" },
                token_id
            );
        }
        Ok(())
    }

    async fn handle_share(&self, event: &BlockchainEvent) -> Result<()> {
        if let Some(data) = &event.data {
            let sharer = data.get("sharer").and_then(|v| v.as_str()).unwrap_or("");
            let token_id = data.get("tokenId").and_then(|v| v.as_str()).unwrap_or("");

            // Generate UUID for the NFT
            let nft_uuid = Self::generate_nft_uuid(&event.contract_address, token_id);

            let interaction = InteractionEvent {
                user_address: sharer.to_string(),
                nft_id: nft_uuid.to_string(),
                interaction_type: InteractionType::Share,
                view_duration_ms: None,
                source: Some("feed".to_string()),
                nft_contract_type: Some(event.contract_type.clone()),
                nft_creator_address: None,
                nft_tags: vec![],
            };

            record_interaction(&self.pool, interaction).await?;

            info!("📤 Processed share: {} shared {}", sharer, token_id);
        }
        Ok(())
    }

    async fn handle_follow(&self, event: &BlockchainEvent) -> Result<()> {
        if let Some(data) = &event.data {
            let follower = data.get("follower").and_then(|v| v.as_str()).unwrap_or("");
            let target = data.get("target").and_then(|v| v.as_str()).unwrap_or("");

            // Following relationships help with creator affinity scoring
            // We could store this in a separate table or use it for preference updates
            info!("👥 Processed follow: {} follows {}", follower, target);
        }
        Ok(())
    }

    async fn handle_unfollow(&self, event: &BlockchainEvent) -> Result<()> {
        if let Some(data) = &event.data {
            let follower = data.get("follower").and_then(|v| v.as_str()).unwrap_or("");
            let target = data.get("target").and_then(|v| v.as_str()).unwrap_or("");

            info!("👋 Processed unfollow: {} unfollows {}", follower, target);
        }
        Ok(())
    }

    async fn handle_username_registration(&self, event: &BlockchainEvent) -> Result<()> {
        if let Some(data) = &event.data {
            let user = data.get("user").and_then(|v| v.as_str()).unwrap_or("");
            let username = data.get("username").and_then(|v| v.as_str()).unwrap_or("");

            info!(
                "👤 Processed username registration: {} -> {}",
                user, username
            );
        }
        Ok(())
    }

    async fn handle_profile_update(&self, event: &BlockchainEvent) -> Result<()> {
        if let Some(data) = &event.data {
            let user = data.get("user").and_then(|v| v.as_str()).unwrap_or("");

            info!("📝 Processed profile update: {}", user);
        }
        Ok(())
    }

    async fn handle_profile_update_extended(&self, event: &BlockchainEvent) -> Result<()> {
        if let Some(data) = &event.data {
            let user = data.get("user").and_then(|v| v.as_str()).unwrap_or("");
            let username = data.get("username").and_then(|v| v.as_str()).unwrap_or("");
            let profile_hash = data.get("profile_hash").and_then(|v| v.as_str()).unwrap_or("");
            let bio = data.get("bio").and_then(|v| v.as_str()).unwrap_or("");
            let website = data.get("website").and_then(|v| v.as_str()).unwrap_or("");

            info!("📝 Processed profile updated extended: {} (username={}, profile_hash={}, bio={}, website={})", user, username, profile_hash, bio, website);
        }
        Ok(())
    }

    async fn handle_tip(&self, event: &BlockchainEvent) -> Result<()> {
        if let Some(data) = &event.data {
            let from = data.get("from").and_then(|v| v.as_str()).unwrap_or("");
            let to = data.get("to").and_then(|v| v.as_str()).unwrap_or("");
            let amount = data.get("amount").and_then(|v| v.as_u64()).unwrap_or(0);

            info!("💸 Processed tip: {} sent {} to {}", from, amount, to);
        }
        Ok(())
    }

    async fn handle_badge(&self, event: &BlockchainEvent) -> Result<()> {
        if let Some(data) = &event.data {
            let user = data.get("user").and_then(|v| v.as_str()).unwrap_or("");
            let badge_type = data.get("badgeType").and_then(|v| v.as_str()).unwrap_or("");

            info!("🏆 Processed badge: {} earned {}", user, badge_type);
        }
        Ok(())
    }

    async fn handle_user_verified(&self, event: &BlockchainEvent) -> Result<()> {
        if let Some(data) = &event.data {
            let user = data.get("user").and_then(|v| v.as_str()).unwrap_or("");

            info!("✅ Processed user verification: {}", user);
        }
        Ok(())
    }

    async fn handle_user_blocked(&self, event: &BlockchainEvent) -> Result<()> {
        if let Some(data) = &event.data {
            let user = data.get("user").and_then(|v| v.as_str()).unwrap_or("");
            let blocked_by = data.get("blockedBy").and_then(|v| v.as_str()).unwrap_or("");

            info!("🚫 Processed user block: {} blocked {}", blocked_by, user);
        }
        Ok(())
    }

    async fn handle_royalty_distributed(&self, event: &BlockchainEvent) -> Result<()> {
        if let Some(data) = &event.data {
            let token_id = data.get("tokenId").and_then(|v| v.as_str()).unwrap_or("");
            let recipient = data.get("recipient").and_then(|v| v.as_str()).unwrap_or("");
            let amount = data.get("amount").and_then(|v| v.as_str()).unwrap_or("");

            // Generate UUID for the NFT
            let nft_uuid = Self::generate_nft_uuid(&event.contract_address, token_id);

            // For recommendations, a royalty distribution indicates a purchase/sale
            // This is valuable signal for content popularity and creator success
            let interaction = InteractionEvent {
                user_address: recipient.to_string(),
                nft_id: nft_uuid.to_string(),
                interaction_type: InteractionType::Purchase, // Royalty distribution = successful sale
                view_duration_ms: None,
                source: Some("royalty".to_string()),
                nft_contract_type: Some(event.contract_type.clone()),
                nft_creator_address: None, // The recipient is getting royalties, so they might be the creator
                nft_tags: vec![],
            };

            record_interaction(&self.pool, interaction).await?;

            // self.update_nft_buys_count(&event.contract_address, token_id, true).await?;

            info!(
                "💰 Processed royalty distribution: {} received {} for token {}",
                recipient, amount, token_id
            );
        }
        Ok(())
    }

    async fn handle_earnings_withdrawn(&self, event: &BlockchainEvent) -> Result<()> {
        if let Some(data) = &event.data {
            let user = data.get("user").and_then(|v| v.as_str()).unwrap_or("");
            let amount = data.get("amount").and_then(|v| v.as_str()).unwrap_or("");

            info!(
                "💸 Processed earnings withdrawal: {} withdrew {}",
                user, amount
            );
        }
        Ok(())
    }

    async fn handle_content_requirements_updated(&self, event: &BlockchainEvent) -> Result<()> {
        if let Some(data) = &event.data {
            let updater = data.get("updater").and_then(|v| v.as_str()).unwrap_or("");

            info!("📋 Processed content requirements update by: {}", updater);
        }
        Ok(())
    }

    async fn handle_content_burned(&self, event: &BlockchainEvent) -> Result<()> {
        if let Some(data) = &event.data {
            let token_id = data.get("tokenId").and_then(|v| v.as_str()).unwrap_or("");
            let burner = data.get("burner").and_then(|v| v.as_str()).unwrap_or("");

            info!(
                "🔥 Processed content burn: {} burned token {}",
                burner, token_id
            );
        }
        Ok(())
    }

    async fn handle_burned_content_revenue(&self, event: &BlockchainEvent) -> Result<()> {
        if let Some(data) = &event.data {
            let recipient = data.get("recipient").and_then(|v| v.as_str()).unwrap_or("");
            let amount = data.get("amount").and_then(|v| v.as_str()).unwrap_or("");

            info!(
                "💰 Processed burned content revenue: {} received {}",
                recipient, amount
            );
        }
        Ok(())
    }

    async fn handle_treasury_updated(&self, event: &BlockchainEvent) -> Result<()> {
        if let Some(data) = &event.data {
            let updater = data.get("updater").and_then(|v| v.as_str()).unwrap_or("");
            let action = data.get("action").and_then(|v| v.as_str()).unwrap_or("");

            info!(
                "🏦 Processed treasury update: {} performed {}",
                updater, action
            );
        }
        Ok(())
    }

    async fn handle_daily_limits_updated(&self, event: &BlockchainEvent) -> Result<()> {
        if let Some(data) = &event.data {
            let updater = data.get("updater").and_then(|v| v.as_str()).unwrap_or("");

            info!("📊 Processed daily limits update by: {}", updater);
        }
        Ok(())
    }

    async fn handle_prices_updated(&self, event: &BlockchainEvent) -> Result<()> {
        if let Some(data) = &event.data {
            let updater = data.get("updater").and_then(|v| v.as_str()).unwrap_or("");

            info!("💰 Processed prices update by: {}", updater);
        }
        Ok(())
    }

    async fn handle_tokens_recovered(&self, event: &BlockchainEvent) -> Result<()> {
        if let Some(data) = &event.data {
            let recoverer = data.get("recoverer").and_then(|v| v.as_str()).unwrap_or("");
            let amount = data.get("amount").and_then(|v| v.as_str()).unwrap_or("");

            info!(
                "🔄 Processed token recovery: {} recovered {}",
                recoverer, amount
            );
        }
        Ok(())
    }

    // Helper methods to update NFT counts in the Elixir database
    #[allow(dead_code)]
    async fn update_nft_likes_count(
        &self,
        contract_address: &str,
        token_id: &str,
        increment: bool,
    ) -> Result<()> {
        let query = if increment {
            "UPDATE nfts SET likes_count = likes_count + 1 WHERE contract_address = $1 AND token_id = $2::integer"
        } else {
            "UPDATE nfts SET likes_count = GREATEST(likes_count - 1, 0) WHERE contract_address = $1 AND token_id = $2::integer"
        };

        sqlx::query(query)
            .bind(contract_address)
            .bind(token_id)
            .execute(&self._elixir_pool)
            .await?;

        Ok(())
    }

    #[allow(dead_code)]
    async fn update_nft_comments_count(
        &self,
        contract_address: &str,
        token_id: &str,
        increment: bool,
    ) -> Result<()> {
        let query = if increment {
            "UPDATE nfts SET comments_count = comments_count + 1 WHERE contract_address = $1 AND token_id = $2::integer"
        } else {
            "UPDATE nfts SET comments_count = GREATEST(comments_count - 1, 0) WHERE contract_address = $1 AND token_id = $2::integer"
        };

        sqlx::query(query)
            .bind(contract_address)
            .bind(token_id)
            .execute(&self._elixir_pool)
            .await?;

        Ok(())
    }

    #[allow(dead_code)]
    async fn update_nft_buys_count(
        &self,
        contract_address: &str,
        token_id: &str,
        increment: bool,
    ) -> Result<()> {
        let query = if increment {
            "UPDATE nfts SET buys_count = buys_count + 1 WHERE contract_address = $1 AND token_id = $2::integer"
        } else {
            "UPDATE nfts SET buys_count = GREATEST(buys_count - 1, 0) WHERE contract_address = $1 AND token_id = $2::integer"
        };

        sqlx::query(query)
            .bind(contract_address)
            .bind(token_id)
            .execute(&self._elixir_pool)
            .await?;

        Ok(())
    }

    // Legacy event handlers for backward compatibility
    async fn handle_legacy_mint(&self, event: &BlockchainEvent) -> Result<()> {
        self.handle_content_minted(event).await
    }

    async fn handle_legacy_like(&self, event: &BlockchainEvent) -> Result<()> {
        self.handle_like(event, InteractionType::Like).await
    }

    async fn handle_legacy_comment(&self, event: &BlockchainEvent) -> Result<()> {
        self.handle_comment(event).await
    }

    async fn handle_legacy_purchase(&self, event: &BlockchainEvent) -> Result<()> {
        self.handle_content_purchase(event).await
    }
}

/// Spawn the event processor
pub fn spawn_event_processor(state: Arc<AppState>) -> tokio::task::JoinHandle<()> {
    let shutdown_rx = state.shutdown.subscribe();

    tokio::spawn(async move {
        let processor = match EventProcessor::new(
            &state.config,
            state.db.pool().clone(),
            state.elixir_db.pool().clone(),
            shutdown_rx,
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
