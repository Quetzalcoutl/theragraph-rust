//! EventType — the canonical enum of all supported on-chain events.
//!
//! Variant names MUST match exactly with Elixir's `TheraGraph.Indexer.EventParser`
//! pattern matches. Do not rename without updating the Elixir side.

use serde::{Deserialize, Serialize};

// ============================================================================
// Event Types
// ============================================================================

/// All supported event types.
/// Names MUST match exactly with Elixir's EventParser patterns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum EventType {
    // Snap events
    SnapMinted,
    SnapLiked,
    SnapCommented,
    SnapBoughtAndMinted,
    SnapDeleted,

    // Art events
    ArtMinted,
    ArtLiked,
    ArtCommented,
    ArtBoughtAndMinted,
    ArtDeleted,

    // Music events
    MusicMinted,
    MusicLiked,
    MusicCommented,
    MusicBoughtAndMinted,
    MusicDeleted,

    // Flix events
    FlixMinted,
    FlixLiked,
    FlixCommented,
    FlixBoughtAndMinted,
    FlixDeleted,

    // Friends events
    Followed,
    Unfollowed,
    UsernameRegistered,
    UsernameTransferred,
    ProfileUpdated,
    ProfileUpdatedExtended,
    NotificationEvent,
    EarningsWithdrawn,
    UserVerified,
    UserUnverified,
    UserBlocked,
    UserUnblocked,

    // Unified TheraFriendz content & social events
    ContentMinted,
    ContentCopyMinted,
    // Fired ALONGSIDE ContentCopyMinted (same tx) only when a copy sale was
    // attributed to a curator's referral link — never a substitute for it.
    // See project_profile_likes_privacy memory ("My Shares").
    ContentCopyMintedViaReferral,
    ListingUpdated,
    ContentBlocked,
    ContentBookmarked,
    ContentShared,
    ContentBurned,
    BurnedContentRevenue,
    TreasuryUpdated,
    TokensRecovered,
    BadgeAwarded,
    BadgeRemoved,
    TipSent,
    PlatformFeeUpdated,

    // Common/ERC events
    Transfer,
    PurchaseProcessed,
    RoyaltyDistributed,
    CollabProposed,

    // Unknown event (fallback)
    Unknown,
}

#[allow(dead_code)]
impl EventType {
    /// Get the contract type for this event
    pub fn contract_type(&self) -> &'static str {
        match self {
            EventType::SnapMinted
            | EventType::SnapLiked
            | EventType::SnapCommented
            | EventType::SnapBoughtAndMinted
            | EventType::SnapDeleted => "snap",

            EventType::ArtMinted
            | EventType::ArtLiked
            | EventType::ArtCommented
            | EventType::ArtBoughtAndMinted
            | EventType::ArtDeleted => "art",

            EventType::MusicMinted
            | EventType::MusicLiked
            | EventType::MusicCommented
            | EventType::MusicBoughtAndMinted
            | EventType::MusicDeleted => "music",

            EventType::FlixMinted
            | EventType::FlixLiked
            | EventType::FlixCommented
            | EventType::FlixBoughtAndMinted
            | EventType::FlixDeleted => "flix",

            EventType::Followed
            | EventType::Unfollowed
            | EventType::UsernameRegistered
            | EventType::UsernameTransferred
            | EventType::ProfileUpdated
            | EventType::ProfileUpdatedExtended
            | EventType::NotificationEvent
            | EventType::EarningsWithdrawn
            | EventType::UserVerified
            | EventType::UserUnverified
            | EventType::UserBlocked
            | EventType::UserUnblocked
            | EventType::ContentMinted
            | EventType::ContentCopyMinted
            | EventType::ContentCopyMintedViaReferral
            | EventType::ListingUpdated
            | EventType::ContentBlocked
            | EventType::ContentBookmarked
            | EventType::ContentShared
            | EventType::ContentBurned
            | EventType::TreasuryUpdated
            | EventType::TokensRecovered
            | EventType::BadgeAwarded
            | EventType::BadgeRemoved
            | EventType::TipSent
            | EventType::PlatformFeeUpdated => "friends",

            EventType::Transfer
            | EventType::PurchaseProcessed
            | EventType::RoyaltyDistributed
            | EventType::BurnedContentRevenue
            | EventType::CollabProposed
            | EventType::Unknown => "common",
        }
    }

    /// Check if this is a minting event
    pub fn is_mint(&self) -> bool {
        matches!(
            self,
            EventType::SnapMinted
                | EventType::ArtMinted
                | EventType::MusicMinted
                | EventType::FlixMinted
                | EventType::ContentMinted
        )
    }

    /// Check if this is a like event
    pub fn is_like(&self) -> bool {
        matches!(
            self,
            EventType::SnapLiked
                | EventType::ArtLiked
                | EventType::MusicLiked
                | EventType::FlixLiked
        )
    }

    /// Check if this is a purchase event
    pub fn is_purchase(&self) -> bool {
        matches!(
            self,
            EventType::SnapBoughtAndMinted
                | EventType::ArtBoughtAndMinted
                | EventType::MusicBoughtAndMinted
                | EventType::FlixBoughtAndMinted
                | EventType::PurchaseProcessed
                | EventType::ContentCopyMinted
        )
    }

    /// Check if this event triggers a user push notification.
    ///
    /// These events are routed to `notifications.priority` Kafka topic
    /// (batch_size=1, timeout=0ms consumer on the Elixir side) so they are
    /// never queued behind analytics bursts like ContentMinted × 200.
    ///
    /// Covers: unified TheraFriendz events + legacy per-contract events.
    pub fn is_notification_event(&self) -> bool {
        matches!(
            self,
            // Unified TheraFriendz contract
            EventType::ContentCopyMinted
                // Legacy per-contract likes
                | EventType::SnapLiked
                | EventType::ArtLiked
                | EventType::MusicLiked
                | EventType::FlixLiked
                // Legacy per-contract comments
                | EventType::SnapCommented
                | EventType::ArtCommented
                | EventType::MusicCommented
                | EventType::FlixCommented
                // Legacy per-contract purchases (trigger copy_purchased notification)
                | EventType::SnapBoughtAndMinted
                | EventType::ArtBoughtAndMinted
                | EventType::MusicBoughtAndMinted
                | EventType::FlixBoughtAndMinted
                // Social follows
                | EventType::Followed
        )
    }

    /// Check if this is a social event
    pub fn is_social(&self) -> bool {
        matches!(
            self,
            EventType::Followed
                | EventType::Unfollowed
                | EventType::UsernameRegistered
                | EventType::UsernameTransferred
                | EventType::ProfileUpdated
                | EventType::NotificationEvent
                | EventType::EarningsWithdrawn
                | EventType::UserVerified
                | EventType::UserUnverified
                | EventType::UserBlocked
                | EventType::UserUnblocked
                | EventType::BadgeAwarded
                | EventType::BadgeRemoved
                | EventType::TipSent
                | EventType::ContentBookmarked
                | EventType::ContentShared
        )
    }

    /// Get Kafka topic for this event type.
    ///
    /// Three-tier routing:
    /// 1. `notifications.priority` — events that trigger user pushes.
    ///    Consumed immediately (batch_size=1) by PriorityKafkaConsumer.
    /// 2. `user.actions` — social/profile events for recommendations.
    /// 3. `blockchain.events` — analytics, mints, admin events.
    pub fn kafka_topic(&self) -> &'static str {
        if self.is_notification_event() {
            "notifications.priority"
        } else if self.is_social() {
            "user.actions"
        } else {
            "blockchain.events"
        }
    }
}

impl std::fmt::Display for EventType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}
