//! Event Signatures and Parsing
//!
//! This module provides Ethereum event signature definitions and parsing utilities
//! for TheraGraph smart contracts. It ensures compatibility between Rust and Elixir
//! by using identical event type names and data structures.
//!
//! ## Integration with Elixir
//!
//! Events produced here are consumed by `TheraGraph.Indexer.KafkaConsumer` in Elixir.
//! The `event_type` field maps directly to pattern matches in the Elixir consumer.
//!
//! ## Event Topics
//!
//! - `blockchain.events` - Raw blockchain events with full log data
//! - `user.actions` - Processed user actions for recommendations

use crate::error::Result;
use ethers::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use once_cell::sync::Lazy;

// ============================================================================
// Event Signatures (keccak256 hashes)
// ============================================================================

/// Pre-computed event signatures for TheraGraph contracts
/// These match the signatures in `TheraGraph.Indexer.EventParser` on the Elixir side
pub static EVENT_SIGNATURES: Lazy<HashMap<H256, EventType>> = Lazy::new(|| {
    let mut m = HashMap::new();

    // === TheraSnap Events ===
    m.insert(
        keccak256_signature("SnapMinted(uint256,string,address)"),
        EventType::SnapMinted,
    );
    m.insert(
        keccak256_signature("SnapLiked(uint256,address,uint256)"),
        EventType::SnapLiked,
    );
    m.insert(
        keccak256_signature("SnapCommented(uint256,uint256,address,string)"),
        EventType::SnapCommented,
    );
    m.insert(
        keccak256_signature("SnapBoughtAndMinted(uint256,address,address,uint256,uint256)"),
        EventType::SnapBoughtAndMinted,
    );
    m.insert(
        keccak256_signature("SnapDeleted(uint256,address)"),
        EventType::SnapDeleted,
    );

    // === TheraArt Events ===
    m.insert(
        keccak256_signature("ArtMinted(uint256,string,address)"),
        EventType::ArtMinted,
    );
    m.insert(
        keccak256_signature("ArtLiked(uint256,address,uint256)"),
        EventType::ArtLiked,
    );
    m.insert(
        keccak256_signature("ArtCommented(uint256,uint256,address,string)"),
        EventType::ArtCommented,
    );
    m.insert(
        keccak256_signature("ArtBoughtAndMinted(uint256,address,address,uint256,uint256)"),
        EventType::ArtBoughtAndMinted,
    );
    m.insert(
        keccak256_signature("ArtDeleted(uint256,address)"),
        EventType::ArtDeleted,
    );

    // === TheraMusic Events ===
    m.insert(
        keccak256_signature("MusicMinted(uint256,string,address)"),
        EventType::MusicMinted,
    );
    m.insert(
        keccak256_signature("MusicLiked(uint256,address,uint256)"),
        EventType::MusicLiked,
    );
    m.insert(
        keccak256_signature("MusicCommented(uint256,uint256,address,string)"),
        EventType::MusicCommented,
    );
    m.insert(
        keccak256_signature("MusicBoughtAndMinted(uint256,address,address,uint256,uint256)"),
        EventType::MusicBoughtAndMinted,
    );
    m.insert(
        keccak256_signature("MusicDeleted(uint256,address)"),
        EventType::MusicDeleted,
    );

    // === TheraFlix Events ===
    m.insert(
        keccak256_signature("FlixMinted(uint256,string,address)"),
        EventType::FlixMinted,
    );
    m.insert(
        keccak256_signature("FlixLiked(uint256,address,uint256)"),
        EventType::FlixLiked,
    );
    m.insert(
        keccak256_signature("FlixCommented(uint256,uint256,address,string)"),
        EventType::FlixCommented,
    );
    m.insert(
        keccak256_signature("FlixBoughtAndMinted(uint256,address,address,uint256,uint256)"),
        EventType::FlixBoughtAndMinted,
    );
    m.insert(
        keccak256_signature("FlixDeleted(uint256,address)"),
        EventType::FlixDeleted,
    );

    // === TheraFriendz Events ===
    m.insert(
        keccak256_signature("Followed(address,address,string,string,uint256)"),
        EventType::Followed,
    );
    m.insert(
        keccak256_signature("Unfollowed(address,address,string,string)"),
        EventType::Unfollowed,
    );
    m.insert(
        keccak256_signature("UsernameRegistered(address,string)"),
        EventType::UsernameRegistered,
    );
    m.insert(
        keccak256_signature("UsernameTransferred(address,address,string,uint256)"),
        EventType::UsernameTransferred,
    );
    m.insert(
        keccak256_signature("ProfileUpdated(address,string,string,string,string)"),
        EventType::ProfileUpdated,
    );
    m.insert(
        keccak256_signature(
            "NotificationEvent(address,address,uint8,uint256,string,string,bytes32,string)",
        ),
        EventType::NotificationEvent,
    );
    m.insert(
        keccak256_signature("EarningsWithdrawn(address,uint256)"),
        EventType::EarningsWithdrawn,
    );

    // Newer TheraFriendz social events (UserFollowed/UserUnfollowed)
    m.insert(
        keccak256_signature("UserFollowed(address,address,uint256)"),
        EventType::UserFollowed,
    );
    m.insert(
        keccak256_signature("UserUnfollowed(address,address,uint256)"),
        EventType::UserUnfollowed,
    );
    m.insert(
        keccak256_signature("UserVerified(address,string)"),
        EventType::UserVerified,
    );
    m.insert(
        keccak256_signature("UserUnverified(address,string)"),
        EventType::UserUnverified,
    );
    m.insert(
        keccak256_signature("UserBlocked(address,address)"),
        EventType::UserBlocked,
    );
    m.insert(
        keccak256_signature("UserUnblocked(address,address)"),
        EventType::UserUnblocked,
    );

    // === Common Events ===
    m.insert(
        keccak256_signature("Transfer(address,address,uint256)"),
        EventType::Transfer,
    );
    m.insert(
        keccak256_signature("PurchaseProcessed(uint256,address,uint256)"),
        EventType::PurchaseProcessed,
    );
    m.insert(
        keccak256_signature("RoyaltyDistributed(uint256,address,uint256)"),
        EventType::RoyaltyDistributed,
    );

    // Unified TheraFriendz content & social events (new contract)
    m.insert(
        h256_from_hex("0xe913bf0f321ec4538e6e03894963538ad29d5bc7610699f655b8d4be77ef3c31"),
        EventType::ContentMinted,
    );
    m.insert(
        h256_from_hex("0x80c2e061ec45ed7331a60555bbadc701bd26c6335bcd10063bc2fe287d040f2f"),
        EventType::ContentCopyMinted,
    );
    m.insert(
        h256_from_hex("0x8417b49947e6fe4baaaf043fd8bc39e9a14bdfcac1627dc1c35f75a8e844321b"),
        EventType::ContentLiked,
    );
    m.insert(
        h256_from_hex("0x54a63e587e58f95e1fb1b3a87102a23fac1fa5dd3d99442cc97043cf031b8ac1"),
        EventType::ContentUnliked,
    );
    m.insert(
        h256_from_hex("0x505d1203546d4a3699987fc90279e0a1dfe65117be15cac29d00ca3ed7a673b6"),
        EventType::ContentCommented,
    );
    m.insert(
        h256_from_hex("0x62d3506db24551831d906a4161625343e801105b08beef50f2616a51fd17a7b8"),
        EventType::ContentBlocked,
    );
    m.insert(
        h256_from_hex("0x4bbdc3b759094c64d5ae0d8d46654078d43716a6188ae8eb6bc36de1d06994c1"),
        EventType::ContentBookmarked,
    );
    m.insert(
        keccak256_signature("ContentShared(uint256,address,address,uint256)"),
        EventType::ContentShared,
    );
    m.insert(
        h256_from_hex("0xff02d2c736810756fea3a252038a4e88a63bf500d03dc6e5aeccf306963f9757"),
        EventType::ContentRequirementsUpdated,
    );
    m.insert(
        h256_from_hex("0x528a31b859c72723f16bde373bc45e6e13a4d24d709e07200855baccec618cff"),
        EventType::ContentBurned,
    );
    m.insert(
        h256_from_hex("0x53e62c84b456cda6228f6c0acd671088271c8bb9627a72d3f8c3d631c8473724"),
        EventType::UserFollowed,
    );
    m.insert(
        h256_from_hex("0x594a48474c36e0d85b16b86393fc3d3a2ed770e7b4f0915b2972d5fbdaa99329"),
        EventType::UserUnfollowed,
    );
    m.insert(
        h256_from_hex("0x0a09fa67e91ea818e53d712f63caf32f685bed0c54acdb1cebf8f63a36b454aa"),
        EventType::UsernameRegistered,
    );
    m.insert(
        keccak256_signature("UsernameTransferred(address,address,string,uint256)"),
        EventType::UsernameTransferred,
    );
    m.insert(
        h256_from_hex("0xdcb94c0b2c025b0736b4b62b1c595f2ca7ad4c711eada6026d477e87de9cca08"),
        EventType::ProfileUpdated,
    );
    m.insert(
        h256_from_hex("0xb493045fc13318793ba6deaf400d8f23236835ab7c056d18196896cf98fbd9d9"),
        EventType::ProfileUpdatedExtended,
    );
    m.insert(
        h256_from_hex("0x22b3126528cda4618d13b6945f5e96fe53a5125f386aa591ee89134e2681c621"),
        EventType::UserVerified,
    );
    m.insert(
        h256_from_hex("0x4906653113399be7fcd9c1ea679e52a58c1efeb96169aaa8b1fd94339ce12b57"),
        EventType::UserBlocked,
    );
    m.insert(
        h256_from_hex("0xe3698e4763ee4becca0f71e44047f2c0018e133a8c70ab056c2ad3641fefd54a"),
        EventType::RoyaltyDistributed,
    );
    m.insert(
        h256_from_hex("0x90dac969af4a4897610ef8f0cd934c54409861eb7bd2205e552f8f2296ee5d3e"),
        EventType::EarningsWithdrawn,
    );
    m.insert(
        h256_from_hex("0xc83ca0840994260dfd9b90ce0f552ac8a0424cae524b6dee6b476a78f6fbdc30"),
        EventType::BurnedContentRevenue,
    );
    m.insert(
        h256_from_hex("0x08031759b0a2a99f63000784e546d7320d30692b97de1ea89a1645380cfb16f8"),
        EventType::TreasuryUpdated,
    );
    m.insert(
        h256_from_hex("0x8c2ba571b537bdaa6702790f86f4a470d37ecd91a6d1e57acc410a039d4f6593"),
        EventType::DailyLimitsUpdated,
    );
    m.insert(
        h256_from_hex("0x382768820017a6e69506da8e35e39b17315306885e94830a6b4d97aa3e3587ff"),
        EventType::TokensRecovered,
    );
    m.insert(
        keccak256_signature("TipSent(address,address,uint256,uint256)"),
        EventType::TipSent,
    );
    m.insert(
        keccak256_signature("CollabProposed(uint256,address,address,uint256)"),
        EventType::CollabProposed,
    );
    m.insert(
        keccak256_signature("BadgeAwarded(address,string,uint256)"),
        EventType::BadgeAwarded,
    );
    m.insert(
        keccak256_signature("BadgeRemoved(address,string,uint256)"),
        EventType::BadgeRemoved,
    );
    m.insert(
        keccak256_signature(
            "PricesUpdated(uint128,uint128,uint128,uint128,uint64,uint256)",
        ),
        EventType::PricesUpdated,
    );

    m
});

/// Helper function to compute keccak256 of an event signature
fn keccak256_signature(sig: &str) -> H256 {
    H256::from_slice(&ethers::utils::keccak256(sig.as_bytes()))
}

/// Helper function to create H256 from hex string
fn h256_from_hex(hex: &str) -> H256 {
    if hex.len() < 2 {
        return H256::zero();
    }
    match hex::decode(&hex[2..]) {
        Ok(bytes) if bytes.len() == 32 => H256::from_slice(&bytes),
        _ => {
            tracing::warn!("h256_from_hex: invalid hex input (len={})", hex.len());
            H256::zero()
        }
    }
}

// ============================================================================
// Event Types
// ============================================================================

/// All supported event types
/// These names MUST match exactly with Elixir's EventParser patterns
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
    ContentLiked,
    ContentUnliked,
    ContentCommented,
    ContentBlocked,
    ContentBookmarked,
    ContentShared,
    ContentRequirementsUpdated,
    ContentBurned,
    BurnedContentRevenue,
    UserFollowed,
    UserUnfollowed,
    TreasuryUpdated,
    DailyLimitsUpdated,
    TokensRecovered,
    BadgeAwarded,
    BadgeRemoved,
    TipSent,
    PricesUpdated,

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
            | EventType::ContentLiked
            | EventType::ContentUnliked
            | EventType::ContentCommented
            | EventType::ContentBlocked
            | EventType::ContentBookmarked
            | EventType::ContentShared
            | EventType::ContentRequirementsUpdated
            | EventType::ContentBurned
            | EventType::UserFollowed
            | EventType::UserUnfollowed
            | EventType::TreasuryUpdated
            | EventType::DailyLimitsUpdated
            | EventType::TokensRecovered
            | EventType::BadgeAwarded
            | EventType::BadgeRemoved
            | EventType::TipSent
            | EventType::PricesUpdated => "friends",
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
                | EventType::ContentLiked
                | EventType::ContentUnliked
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
            EventType::ContentLiked
                | EventType::ContentCommented
                | EventType::ContentCopyMinted
                | EventType::UserFollowed
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
                | EventType::UserFollowed
                | EventType::UserUnfollowed
                | EventType::BadgeAwarded
                | EventType::BadgeRemoved
                | EventType::TipSent
                | EventType::PricesUpdated
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
        // Serialize to the same format as serde
        let s = format!("{:?}", self);
        write!(f, "{}", s)
    }
}

// ============================================================================
// Parsed Event
// ============================================================================

/// A fully parsed blockchain event ready for Kafka
#[derive(Debug, Clone, Serialize)]
pub struct ParsedEvent {
    /// Event type (matches Elixir EventParser patterns)
    pub event_type: String,
    /// Contract address
    pub contract_address: String,
    /// Contract type (snap, art, music, flix, friends)
    pub contract_type: String,
    /// Block number
    pub block_number: u64,
    /// Transaction hash
    pub transaction_hash: String,
    /// Log index within transaction
    pub log_index: u64,
    /// Unix timestamp
    pub timestamp: i64,
    /// Indexed parameters from log topics
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub indexed_params: Vec<String>,
    /// Decoded event data
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<ParsedEventData>,
    /// Raw log data (hex encoded)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_data: Option<String>,
    /// Kafka topic this event should be routed to (set at parse time from EventType::kafka_topic())
    pub kafka_topic: &'static str,
}

/// Decoded event data for different event types
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum ParsedEventData {
    /// Minted event data
    Minted {
        token_id: String,
        uri: String,
        creator: String,
        content_type: String,
        price: String,
        timestamp: String,
    },
    /// Content copy minted / purchase-and-minted
    CopyMinted {
        original_id: String,
        buyer: String,
        new_token_id: String,
        content_type: String,
        timestamp: String,
    },
    /// Liked/unliked event data
    Liked {
        token_id: String,
        liker: String,
        creator: String,
        total_likes: String,
        timestamp: String,
    },
    /// Commented event data
    Commented {
        token_id: String,
        comment_id: String,
        commenter: String,
        comment: String,
        content_type: String,
        timestamp: String,
    },
    /// Bookmarked event data
    Bookmarked {
        token_id: String,
        user: String,
        bookmarked: bool,
        timestamp: String,
    },
    /// Shared event data
    Shared {
        token_id: String,
        sharer: String,
        recipient: String,
        timestamp: String,
    },
    /// BoughtAndMinted event data (old naming)
    BoughtAndMinted {
        token_id: String,
        buyer: String,
        seller: String,
        price: String,
        new_token_id: String,
    },
    /// Deleted event data
    Deleted { token_id: String, deleter: String },
    /// Followed event data
    Followed {
        follower: String,
        followed: String,
        follower_username: String,
        followed_username: String,
        timestamp: String,
    },
    /// ProfileUpdatedExtended event data (username, profileHash, bio, website, timestamp)
    ProfileUpdatedExtended {
        username: String,
        profile_hash: String,
        bio: String,
        website: String,
        timestamp: String,
    },
    /// Transfer event data
    Transfer {
        from: String,
        to: String,
        token_id: String,
    },
    /// PurchaseProcessed event data
    Purchase {
        token_id: String,
        buyer: String,
        amount: String,
    },
    /// Royalty distributed when royalties are paid out
    RoyaltyDistributed {
        token_id: String,
        recipient: String,
        amount: String,
        timestamp: String,
    },
    /// EarningsWithdrawn event
    EarningsWithdrawn {
        user: String,
        amount: String,
        timestamp: String,
    },
    /// PricesUpdated admin event
    PricesUpdated {
        copy: String,
        like: String,
        comment: String,
        follow: String,
        fee: String,
        timestamp: String,
    },
    /// TreasuryUpdated admin event
    TreasuryUpdated {
        old_treasury: String,
        new_treasury: String,
        timestamp: String,
    },
    /// DailyLimitsUpdated admin event
    DailyLimitsUpdated {
        max_posts: String,
        max_follows: String,
        timestamp: String,
    },
    /// ContentRequirementsUpdated admin event
    ContentRequirementsUpdated {
        snap: String,
        art: String,
        music: String,
        flix: String,
        timestamp: String,
    },
    /// BurnedContentRevenue event
    BurnedContentRevenue {
        token_id: String,
        amount: String,
        timestamp: String,
    },

    /// UsernameRegistered event data
    UsernameRegistered {
        user: String,
        username: String,
        timestamp: String,
    },

    /// ProfileUpdated (legacy) event data
    ProfileUpdatedSimple {
        user: String,
        username: String,
        timestamp: String,
    },

    /// UserVerified event
    UserVerifiedEvent {
        user: String,
        timestamp: String,
    },

    /// UserBlocked/Unblocked event
    UserBlockedEvent {
        user: String,
        status: bool,
        timestamp: String,
    },

    /// ContentBurned event data
    ContentBurned {
        token_id: String,
        owner: String,
        timestamp: String,
    },

    /// TokensRecovered (admin)
    TokensRecovered {
        token: String,
        to: String,
        amount: String,
        timestamp: String,
    },

    /// TipSent event
    TipSent {
        sender: String,
        recipient: String,
        amount: String,
        timestamp: String,
    },

    /// BadgeAwarded event
    BadgeAwardedData {
        user: String,
        badge: String,
        timestamp: String,
    },

    /// BadgeRemoved event
    BadgeRemovedData {
        user: String,
        badge: String,
        timestamp: String,
    },

    /// CollabProposed event
    CollabProposedData {
        token_id: String,
        proposer: String,
        recipient: String,
        timestamp: String,
    },

    /// UsernameTransferred event
    UsernameTransferredData {
        from: String,
        to: String,
        username: String,
        timestamp: String,
    },

    /// Generic/raw data
    Raw { hex: String },

}

// ============================================================================
// Event Parser
// ============================================================================

/// Parse a raw Ethereum log into a structured event
///
/// This function implements zero-copy parsing where possible and provides
/// detailed indexed parameter extraction following EVM ABI encoding rules.
///
/// # Arguments
/// * `log` - The raw Ethereum log from the blockchain
/// * `fallback_contract_type` - Contract type to use if event signature is unknown
///
/// # Returns
/// A fully parsed event ready for Kafka serialization
pub fn parse_log(log: &Log, fallback_contract_type: &str) -> Result<ParsedEvent> {
    let topics = &log.topics;

    // Get event type from first topic (event signature)
    let event_type = if topics.is_empty() {
        EventType::Unknown
    } else {
        EVENT_SIGNATURES
            .get(&topics[0])
            .copied()
            .unwrap_or(EventType::Unknown)
    };

    // Extract indexed parameters with proper formatting based on event type
    // Following EVM ABI: addresses are right-padded in 32-byte topics
    let indexed_params: Vec<String> = extract_indexed_params(&event_type, topics);

    // Determine contract type from event or fallback
    // Default contract_type from event type (some events like Content* are "social" and
    // include a contentType indexed param that we map into art/music/flix/snap).
    let mut contract_type = if event_type != EventType::Unknown {
        event_type.contract_type().to_string()
    } else {
        fallback_contract_type.to_string()
    };

    // If this is a ContentMinted event, it includes ContentType as the 3rd indexed param
    if matches!(event_type, EventType::ContentMinted) && indexed_params.len() >= 3 {
        // contentType is encoded as decimal string in topics
        let ct = indexed_params.get(2).and_then(|s| s.parse::<u64>().ok());
        if let Some(ctv) = ct {
            contract_type = match ctv {
                0 => "art".to_string(),
                1 => "flix".to_string(),
                2 => "music".to_string(),
                3 => "snap".to_string(),
                unknown => {
                    tracing::warn!(
                        "ContentMinted: unknown contentType={unknown}, keeping contract_type={contract_type}"
                    );
                    contract_type
                }
            };
        }
    }

    // Parse event-specific data
    let data = parse_event_data(&event_type, &indexed_params, &log.data);

    let block_number = log.block_number.map(|b| b.as_u64()).unwrap_or(0);
    let tx_hash = log
        .transaction_hash
        .map(|h| format!("{:?}", h))
        .unwrap_or_default();
    let log_index = log.log_index.map(|i| i.as_u64()).unwrap_or(0);

    Ok(ParsedEvent {
        kafka_topic: event_type.kafka_topic(),
        event_type: event_type.to_string(),
        contract_address: format!("{:?}", log.address),
        contract_type,
        block_number,
        transaction_hash: tx_hash,
        log_index,
        timestamp: chrono::Utc::now().timestamp(),
        indexed_params,
        data,
        raw_data: if log.data.is_empty() {
            None
        } else {
            Some(format!("0x{}", hex::encode(&log.data)))
        },
    })
}

/// Extract indexed parameters from log topics with proper type-aware formatting
///
/// EVM ABI encoding rules:
/// - Addresses: Right-aligned in 32 bytes, extract last 20 bytes
/// - uint256: Full 32 bytes as big-endian integer
/// - bytes32: Full 32 bytes as hex
///
/// This provides Elixir with properly formatted values for immediate use.
fn extract_indexed_params(event_type: &EventType, topics: &[H256]) -> Vec<String> {
    // Skip first topic (event signature) and process remaining
    topics
        .iter()
        .skip(1)
        .enumerate()
        .map(|(idx, topic)| format_indexed_param(event_type, idx, topic))
        .collect()
}

/// Format a single indexed parameter based on event type and position
fn format_indexed_param(event_type: &EventType, param_index: usize, topic: &H256) -> String {
    // Determine the type of the indexed parameter based on event type and position
    let param_type = get_indexed_param_type(event_type, param_index);

    match param_type {
        IndexedParamType::Address => {
            // Addresses are right-aligned in 32 bytes, extract last 20 bytes
            format!("0x{}", hex::encode(&topic.as_bytes()[12..]))
        }
        IndexedParamType::Uint256 => {
            // Convert to decimal string for Elixir compatibility
            let value = U256::from_big_endian(topic.as_bytes());
            value.to_string()
        }
        IndexedParamType::Bytes32 => {
            // Keep as full hex
            format!("{:?}", topic)
        }
    }
}

/// Types of indexed parameters in EVM events
#[derive(Debug, Clone, Copy)]
enum IndexedParamType {
    Address,
    Uint256,
    Bytes32,
}

/// Determine the type of an indexed parameter based on event type and position
fn get_indexed_param_type(event_type: &EventType, param_index: usize) -> IndexedParamType {
    match event_type {
        // Minted events: (uint256 indexed tokenId, ...)
        EventType::SnapMinted
        | EventType::ArtMinted
        | EventType::MusicMinted
        | EventType::FlixMinted => match param_index {
            0 => IndexedParamType::Uint256, // tokenId
            _ => IndexedParamType::Bytes32,
        },

        // Unified TheraFriendz events - map indexed params per event
        EventType::ContentMinted => match param_index {
            0 => IndexedParamType::Uint256, // tokenId
            1 => IndexedParamType::Address, // creator
            2 => IndexedParamType::Uint256, // contentType (uint8 encoded as uint256)
            _ => IndexedParamType::Bytes32,
        },
        EventType::ContentCopyMinted => match param_index {
            0 => IndexedParamType::Uint256, // originalId
            1 => IndexedParamType::Address, // buyer
            2 => IndexedParamType::Uint256, // newTokenId
            _ => IndexedParamType::Bytes32,
        },
        EventType::ContentLiked | EventType::ContentUnliked => match param_index {
            0 => IndexedParamType::Uint256, // tokenId
            1 => IndexedParamType::Address, // liker / unliker
            2 => IndexedParamType::Address, // creator
            _ => IndexedParamType::Bytes32,
        },
        EventType::ContentCommented | EventType::ContentBlocked | EventType::ContentBookmarked => {
            match param_index {
                0 => IndexedParamType::Uint256, // tokenId
                1 => IndexedParamType::Address, // commenter / moderator / user
                _ => IndexedParamType::Bytes32,
            }
        }
        EventType::ContentShared => match param_index {
            0 => IndexedParamType::Uint256, // tokenId
            1 => IndexedParamType::Address, // sharer
            2 => IndexedParamType::Address, // recipient
            _ => IndexedParamType::Bytes32,
        },

        // Liked events: (uint256 indexed tokenId, address liker, ...)
        EventType::SnapLiked
        | EventType::ArtLiked
        | EventType::MusicLiked
        | EventType::FlixLiked => match param_index {
            0 => IndexedParamType::Uint256, // tokenId
            _ => IndexedParamType::Bytes32,
        },

        // Commented events: (uint256 indexed tokenId, ...)
        EventType::SnapCommented
        | EventType::ArtCommented
        | EventType::MusicCommented
        | EventType::FlixCommented => match param_index {
            0 => IndexedParamType::Uint256, // tokenId
            _ => IndexedParamType::Bytes32,
        },

        // BoughtAndMinted events: (uint256 indexed tokenId, ...)
        EventType::SnapBoughtAndMinted
        | EventType::ArtBoughtAndMinted
        | EventType::MusicBoughtAndMinted
        | EventType::FlixBoughtAndMinted => match param_index {
            0 => IndexedParamType::Uint256, // tokenId
            _ => IndexedParamType::Bytes32,
        },

        // Deleted events: (uint256 indexed tokenId, ...)
        EventType::SnapDeleted
        | EventType::ArtDeleted
        | EventType::MusicDeleted
        | EventType::FlixDeleted => match param_index {
            0 => IndexedParamType::Uint256, // tokenId
            _ => IndexedParamType::Bytes32,
        },

        // Social events with addresses (legacy `Followed/Unfollowed` and new `UserFollowed/UserUnfollowed`)
        EventType::Followed
        | EventType::Unfollowed
        | EventType::UserFollowed
        | EventType::UserUnfollowed => match param_index {
            0 => IndexedParamType::Address, // follower
            1 => IndexedParamType::Address, // followed/target
            _ => IndexedParamType::Bytes32,
        },

        EventType::UserBlocked | EventType::UserUnblocked => match param_index {
            0 => IndexedParamType::Address, // user
            _ => IndexedParamType::Bytes32,
        },

        EventType::UsernameRegistered | EventType::UserVerified | EventType::UserUnverified => {
            match param_index {
                0 => IndexedParamType::Address, // user
                _ => IndexedParamType::Bytes32,
            }
        }

        EventType::UsernameTransferred => match param_index {
            0 => IndexedParamType::Address, // from
            1 => IndexedParamType::Address, // to
            _ => IndexedParamType::Bytes32,
        },

        EventType::ProfileUpdated => match param_index {
            0 => IndexedParamType::Address, // user
            _ => IndexedParamType::Bytes32,
        },

        EventType::ContentBurned => match param_index {
            0 => IndexedParamType::Uint256, // tokenId
            1 => IndexedParamType::Address, // owner
            _ => IndexedParamType::Bytes32,
        },

        EventType::TokensRecovered => match param_index {
            0 => IndexedParamType::Address,
            1 => IndexedParamType::Address,
            _ => IndexedParamType::Bytes32,
        },

        EventType::TipSent => match param_index {
            0 => IndexedParamType::Address,
            1 => IndexedParamType::Address,
            _ => IndexedParamType::Bytes32,
        },

        EventType::BadgeAwarded | EventType::BadgeRemoved => match param_index {
            0 => IndexedParamType::Address,
            _ => IndexedParamType::Bytes32,
        },

        EventType::CollabProposed => match param_index {
            0 => IndexedParamType::Uint256,
            1 => IndexedParamType::Address,
            2 => IndexedParamType::Address,
            _ => IndexedParamType::Bytes32,
        },

        EventType::ProfileUpdatedExtended => match param_index {
            0 => IndexedParamType::Address, // user
            _ => IndexedParamType::Bytes32,
        },

        EventType::EarningsWithdrawn => match param_index {
            0 => IndexedParamType::Address, // user
            _ => IndexedParamType::Bytes32,
        },

        EventType::NotificationEvent => match param_index {
            0 => IndexedParamType::Address, // sender
            1 => IndexedParamType::Address, // recipient
            _ => IndexedParamType::Bytes32,
        },

        // Transfer: (address indexed from, address indexed to, uint256 indexed tokenId)
        EventType::Transfer => match param_index {
            0 => IndexedParamType::Address, // from
            1 => IndexedParamType::Address, // to
            2 => IndexedParamType::Uint256, // tokenId
            _ => IndexedParamType::Bytes32,
        },

        EventType::PurchaseProcessed | EventType::RoyaltyDistributed => match param_index {
            0 => IndexedParamType::Uint256, // tokenId or similar
            1 => IndexedParamType::Address,
            _ => IndexedParamType::Bytes32,
        },

        // BurnedContentRevenue(uint256 indexed tokenId, uint256 amount, uint256 timestamp)
        EventType::BurnedContentRevenue => match param_index {
            0 => IndexedParamType::Uint256, // tokenId
            _ => IndexedParamType::Bytes32,
        },

        // TreasuryUpdated(address indexed oldTreasury, address indexed newTreasury, uint256 timestamp)
        EventType::TreasuryUpdated => match param_index {
            0 => IndexedParamType::Address, // oldTreasury
            1 => IndexedParamType::Address, // newTreasury
            _ => IndexedParamType::Bytes32,
        },

        // Default to bytes32 for unknown types (only types not handled above)
        EventType::PricesUpdated
        | EventType::ContentRequirementsUpdated
        | EventType::DailyLimitsUpdated => IndexedParamType::Bytes32,
        EventType::Unknown => IndexedParamType::Bytes32,
    }
}

// ============================================================================
// ABI decode helpers
// ============================================================================

/// Decode a `Token::Uint` at position `idx` as its decimal string.
fn decode_uint(tokens: &[ethers::abi::Token], idx: usize) -> Option<String> {
    tokens.get(idx).and_then(|t| {
        if let ethers::abi::Token::Uint(u) = t { Some(u.to_string()) } else { None }
    })
}

/// Decode a `Token::String` at position `idx`, cloning the inner value.
fn decode_str(tokens: &[ethers::abi::Token], idx: usize) -> Option<String> {
    tokens.get(idx).and_then(|t| {
        if let ethers::abi::Token::String(s) = t { Some(s.clone()) } else { None }
    })
}

/// Decode a `Token::Bool` at position `idx`, copying the inner value.
fn decode_bool(tokens: &[ethers::abi::Token], idx: usize) -> Option<bool> {
    tokens.get(idx).and_then(|t| {
        if let ethers::abi::Token::Bool(b) = t { Some(*b) } else { None }
    })
}

/// Decode a `Token::Address` at position `idx` as a `0x`-prefixed hex string.
fn decode_addr_token(tokens: &[ethers::abi::Token], idx: usize) -> Option<String> {
    tokens.get(idx).and_then(|t| {
        if let ethers::abi::Token::Address(a) = t {
            Some(format!("0x{}", hex::encode(a.as_bytes())))
        } else {
            None
        }
    })
}

/// Read two consecutive raw U256 fields from `data` at byte offsets 0 and 32.
/// Returns `(field_at_0, field_at_32)` as decimal strings; each falls back to
/// `String::new()` when `data` is too short to contain that word.
///
/// Used by events whose non-indexed data layout is `[amount (32 bytes), timestamp (32 bytes)]`.
#[inline]
fn read_amount_timestamp(data: &Bytes) -> (String, String) {
    let amount = if data.len() >= 32 {
        U256::from_big_endian(&data[0..32]).to_string()
    } else {
        String::new()
    };
    let timestamp = if data.len() >= 64 {
        U256::from_big_endian(&data[32..64]).to_string()
    } else {
        String::new()
    };
    (amount, timestamp)
}

/// ABI-decode `[Uint(256), Uint(256)]` from `data` and return `(field_0, field_1)`.
///
/// Returns `None` when `data` is empty (caller should supply empty-data defaults).
/// Returns `Some(Err(hex))` when the ABI decoder fails (caller wraps as `Raw`).
/// Returns `Some(Ok((a, b)))` on success.
#[inline]
fn decode_two_uint256(
    data: &Bytes,
) -> Option<std::result::Result<(String, String), String>> {
    if data.is_empty() {
        return None;
    }
    Some(
        ethers::abi::decode(
            &[ethers::abi::ParamType::Uint(256), ethers::abi::ParamType::Uint(256)],
            &data.0,
        )
        .map(|tokens| {
            let a = decode_uint(&tokens, 0).unwrap_or_default();
            let b = decode_uint(&tokens, 1).unwrap_or_default();
            (a, b)
        })
        .map_err(|_| format!("0x{}", hex::encode(data))),
    )
}

/// ABI-decode `[String, Uint(256)]` from `data` and return `(string_field, uint_field)`.
///
/// Same tri-state return convention as `decode_two_uint256`.
#[inline]
fn decode_str_uint256(
    data: &Bytes,
) -> Option<std::result::Result<(String, String), String>> {
    if data.is_empty() {
        return None;
    }
    Some(
        ethers::abi::decode(
            &[ethers::abi::ParamType::String, ethers::abi::ParamType::Uint(256)],
            &data.0,
        )
        .map(|tokens| {
            let s = decode_str(&tokens, 0).unwrap_or_default();
            let u = decode_uint(&tokens, 1).unwrap_or_default();
            (s, u)
        })
        .map_err(|_| format!("0x{}", hex::encode(data))),
    )
}

/// Parse event-specific data based on event type
fn parse_event_data(
    event_type: &EventType,
    indexed_params: &[String],
    data: &Bytes,
) -> Option<ParsedEventData> {
    match event_type {
        EventType::ContentMinted => {
            // ContentMinted(uint256 tokenId, address creator, ContentType contentType, uint256 price, uint256 timestamp)
            let token_id = indexed_params.first().cloned().unwrap_or_default();
            let creator = indexed_params.get(1).cloned().unwrap_or_default();
            let content_type = indexed_params.get(2).cloned().unwrap_or_default();

            // data layout: [price (32 bytes), timestamp (32 bytes)]
            let price = if data.len() >= 32 {
                U256::from_big_endian(&data[0..32]).to_string()
            } else {
                String::new()
            };

            let timestamp = if data.len() >= 64 {
                U256::from_big_endian(&data[32..64]).to_string()
            } else {
                String::new()
            };

            Some(ParsedEventData::Minted {
                token_id,
                uri: String::new(),
                creator,
                content_type,
                price,
                timestamp,
            })
        }

        EventType::ContentCopyMinted => {
            // ContentCopyMinted(uint256 originalId, address buyer, uint256 newTokenId, ContentType contentType, uint256 timestamp)
            let original = indexed_params.first().cloned().unwrap_or_default();
            let buyer = indexed_params.get(1).cloned().unwrap_or_default();
            let new_token_id = indexed_params.get(2).cloned().unwrap_or_default();

            // data layout: [contentType (32 bytes -> uint8), timestamp (32 bytes)]
            let content_type = if data.len() >= 32 {
                U256::from_big_endian(&data[0..32]).to_string()
            } else {
                String::new()
            };

            let timestamp = if data.len() >= 64 {
                U256::from_big_endian(&data[32..64]).to_string()
            } else {
                String::new()
            };

            Some(ParsedEventData::CopyMinted {
                original_id: original,
                buyer,
                new_token_id,
                content_type,
                timestamp,
            })
        }

        EventType::ContentLiked => {
            // ContentLiked(uint256 tokenId, address liker, address creator, ContentType contentType, uint256 timestamp)
            let token_id = indexed_params.first().cloned().unwrap_or_default();
            let liker = indexed_params.get(1).cloned().unwrap_or_default();
            let creator = indexed_params.get(2).cloned().unwrap_or_default();

            // data layout: [contentType (32 bytes), timestamp (32 bytes)]
            let timestamp = if data.len() >= 64 {
                U256::from_big_endian(&data[32..64]).to_string()
            } else {
                String::new()
            };

            Some(ParsedEventData::Liked {
                token_id,
                liker,
                creator,
                total_likes: String::new(),
                timestamp,
            })
        }

        EventType::ContentUnliked => {
            // ContentUnliked(uint256 tokenId, address unliker, address creator, ContentType contentType, uint256 timestamp)
            let token_id = indexed_params.first().cloned().unwrap_or_default();
            let unliker = indexed_params.get(1).cloned().unwrap_or_default();
            let creator = indexed_params.get(2).cloned().unwrap_or_default();

            let timestamp = if data.len() >= 64 {
                U256::from_big_endian(&data[32..64]).to_string()
            } else {
                String::new()
            };

            Some(ParsedEventData::Liked {
                token_id,
                liker: unliker,
                creator,
                total_likes: String::new(),
                timestamp,
            })
        }

        EventType::ContentCommented => {
            // ContentCommented(uint256 tokenId, address commenter, uint256 commentId, string comment, ContentType contentType, uint256 timestamp)
            let token_id = indexed_params.first().cloned().unwrap_or_default();
            let commenter = indexed_params.get(1).cloned().unwrap_or_default();

            // decode ABI: [commentId (uint256), comment (string), contentType (uint8), timestamp (uint256)]
            if data.is_empty() {
                Some(ParsedEventData::Commented {
                    token_id,
                    comment_id: String::new(),
                    commenter,
                    comment: String::new(),
                    content_type: String::new(),
                    timestamp: String::new(),
                })
            } else {
                match ethers::abi::decode(
                    &[
                        ethers::abi::ParamType::Uint(256),
                        ethers::abi::ParamType::String,
                        ethers::abi::ParamType::Uint(8),
                        ethers::abi::ParamType::Uint(256),
                    ],
                    &data.0,
                ) {
                    Ok(tokens) => {
                        let comment_id    = decode_uint(&tokens, 0).unwrap_or_default();
                        let comment       = decode_str(&tokens, 1).unwrap_or_default();
                        let content_type  = decode_uint(&tokens, 2).unwrap_or_default();
                        let timestamp     = decode_uint(&tokens, 3).unwrap_or_default();
                        Some(ParsedEventData::Commented { token_id, comment_id, commenter, comment, content_type, timestamp })
                    }
                    Err(_) => Some(ParsedEventData::Raw { hex: format!("0x{}", hex::encode(data)) }),
                }
            }
        }

        EventType::ContentBlocked => {
            // ContentBlocked(uint256 tokenId, address blockedBy, uint8 contentType, string reason)
            let token_id = indexed_params.first().cloned().unwrap_or_default();
            let blocked_by = indexed_params.get(1).cloned().unwrap_or_default();
            Some(ParsedEventData::Deleted {
                token_id,
                deleter: blocked_by,
            })
        }

        EventType::ContentBookmarked => {
            // ContentBookmarked(uint256 tokenId, address user, bool bookmarked, uint256 timestamp)
            let token_id = indexed_params.first().cloned().unwrap_or_default();
            let user = indexed_params.get(1).cloned().unwrap_or_default();
            let bookmarked = if data.len() >= 32 {
                U256::from_big_endian(&data[0..32]) != U256::zero()
            } else {
                true
            };
            let timestamp = if data.len() >= 64 {
                U256::from_big_endian(&data[32..64]).to_string()
            } else {
                String::new()
            };

            Some(ParsedEventData::Bookmarked { token_id, user, bookmarked, timestamp })
        }

        EventType::ContentShared => {
            // ContentShared(uint256 tokenId, address sharer, address recipient, uint256 timestamp)
            let token_id = indexed_params.first().cloned().unwrap_or_default();
            let sharer = indexed_params.get(1).cloned().unwrap_or_default();
            let recipient = indexed_params.get(2).cloned().unwrap_or_default();
            let timestamp = if data.len() >= 32 {
                U256::from_big_endian(&data[0..32]).to_string()
            } else {
                String::new()
            };
            Some(ParsedEventData::Shared { token_id, sharer, recipient, timestamp })
        }
        EventType::SnapMinted
        | EventType::ArtMinted
        | EventType::MusicMinted
        | EventType::FlixMinted => {
            // Minted(uint256 indexed tokenId, string uri, address creator)
            // tokenId is in indexed_params[0]
            // uri and creator are in data
            let token_id = indexed_params.first().cloned().unwrap_or_default();

            if data.len() >= 64 {
                // Decode creator address (last 20 bytes of first 32-byte word)
                let creator = format!("0x{}", hex::encode(&data[12..32]));
                // Legacy minted events did not include price/timestamp; keep fields empty
                Some(ParsedEventData::Minted {
                    token_id,
                    uri: String::new(),
                    creator,
                    content_type: String::new(),
                    price: String::new(),
                    timestamp: String::new(),
                })
            } else {
                Some(ParsedEventData::Raw {
                    hex: format!("0x{}", hex::encode(data)),
                })
            }
        }

        EventType::SnapLiked
        | EventType::ArtLiked
        | EventType::MusicLiked
        | EventType::FlixLiked => {
            // Liked(uint256 indexed tokenId, address liker, uint256 totalLikes)
            let token_id = indexed_params.first().cloned().unwrap_or_default();

            if data.len() >= 64 {
                let liker = format!("0x{}", hex::encode(&data[12..32]));
                let total_likes = U256::from_big_endian(&data[32..64]).to_string();
                let timestamp = if data.len() >= 96 {
                    U256::from_big_endian(&data[64..96]).to_string()
                } else {
                    String::new()
                };
                // creator is not present in legacy liked events
                Some(ParsedEventData::Liked { token_id, liker, creator: String::new(), total_likes, timestamp })
            } else {
                None
            }
        }

        EventType::Transfer => {
            // Transfer(address indexed from, address indexed to, uint256 indexed tokenId)
            let from = indexed_params.first().cloned().unwrap_or_default();
            let to = indexed_params.get(1).cloned().unwrap_or_default();
            let token_id = indexed_params.get(2).cloned().unwrap_or_default();
            Some(ParsedEventData::Transfer { from, to, token_id })
        }

        EventType::PurchaseProcessed => {
            // PurchaseProcessed(uint256 tokenId, address buyer, uint256 amount)
            let token_id = indexed_params.first().cloned().unwrap_or_default();
            let buyer = indexed_params.get(1).cloned().unwrap_or_default();
            let amount = if data.len() >= 32 {
                U256::from_big_endian(&data[0..32]).to_string()
            } else {
                String::new()
            };
            Some(ParsedEventData::Purchase { token_id, buyer, amount })
        }

        EventType::RoyaltyDistributed => {
            // RoyaltyDistributed(uint256 indexed tokenId, address indexed recipient, uint256 amount, uint256 timestamp)
            let token_id = indexed_params.first().cloned().unwrap_or_default();
            let recipient = indexed_params.get(1).cloned().unwrap_or_default();
            let (amount, timestamp) = read_amount_timestamp(data);
            Some(ParsedEventData::RoyaltyDistributed { token_id, recipient, amount, timestamp })
        }

        EventType::EarningsWithdrawn => {
            // EarningsWithdrawn(address indexed user, uint256 amount, uint256 timestamp)
            let user = indexed_params.first().cloned().unwrap_or_default();
            let (amount, timestamp) = read_amount_timestamp(data);
            Some(ParsedEventData::EarningsWithdrawn { user, amount, timestamp })
        }

        EventType::PricesUpdated => {
            // PricesUpdated(uint128 copy, uint128 like, uint128 comment, uint128 follow, uint64 fee, uint256 timestamp)
            if data.is_empty() {
                Some(ParsedEventData::PricesUpdated { copy: String::new(), like: String::new(), comment: String::new(), follow: String::new(), fee: String::new(), timestamp: String::new() })
            } else {
                match ethers::abi::decode(
                    &[
                        ethers::abi::ParamType::Uint(128),
                        ethers::abi::ParamType::Uint(128),
                        ethers::abi::ParamType::Uint(128),
                        ethers::abi::ParamType::Uint(128),
                        ethers::abi::ParamType::Uint(64),
                        ethers::abi::ParamType::Uint(256),
                    ],
                    &data.0,
                ) {
                    Ok(tokens) => {
                        let copy      = decode_uint(&tokens, 0).unwrap_or_default();
                        let like      = decode_uint(&tokens, 1).unwrap_or_default();
                        let comment   = decode_uint(&tokens, 2).unwrap_or_default();
                        let follow    = decode_uint(&tokens, 3).unwrap_or_default();
                        let fee       = decode_uint(&tokens, 4).unwrap_or_default();
                        let timestamp = decode_uint(&tokens, 5).unwrap_or_default();
                        Some(ParsedEventData::PricesUpdated { copy, like, comment, follow, fee, timestamp })
                    }
                    Err(_) => Some(ParsedEventData::Raw { hex: format!("0x{}", hex::encode(data)) }),
                }
            }
        }

        EventType::TreasuryUpdated => {
            // TreasuryUpdated(address indexed oldTreasury, address indexed newTreasury, uint256 timestamp)
            let old = indexed_params.first().cloned().unwrap_or_default();
            let new = indexed_params.get(1).cloned().unwrap_or_default();
            let timestamp = if data.len() >= 32 { U256::from_big_endian(&data[0..32]).to_string() } else { String::new() };
            Some(ParsedEventData::TreasuryUpdated { old_treasury: old, new_treasury: new, timestamp })
        }

        EventType::DailyLimitsUpdated => {
            // DailyLimitsUpdated(uint64 maxPosts, uint64 maxFollows, uint256 timestamp)
            if data.is_empty() {
                Some(ParsedEventData::DailyLimitsUpdated { max_posts: String::new(), max_follows: String::new(), timestamp: String::new() })
            } else {
                match ethers::abi::decode(&[ethers::abi::ParamType::Uint(64), ethers::abi::ParamType::Uint(64), ethers::abi::ParamType::Uint(256)], &data.0) {
                    Ok(tokens) => {
                        let max_posts   = decode_uint(&tokens, 0).unwrap_or_default();
                        let max_follows = decode_uint(&tokens, 1).unwrap_or_default();
                        let timestamp   = decode_uint(&tokens, 2).unwrap_or_default();
                        Some(ParsedEventData::DailyLimitsUpdated { max_posts, max_follows, timestamp })
                    }
                    Err(_) => Some(ParsedEventData::Raw { hex: format!("0x{}", hex::encode(data)) }),
                }
            }
        }

        EventType::ContentRequirementsUpdated => {
            // ContentRequirementsUpdated(uint128 snap, uint128 art, uint128 music, uint128 flix, uint256 timestamp)
            if data.is_empty() {
                Some(ParsedEventData::ContentRequirementsUpdated { snap: String::new(), art: String::new(), music: String::new(), flix: String::new(), timestamp: String::new() })
            } else {
                match ethers::abi::decode(&[ethers::abi::ParamType::Uint(128), ethers::abi::ParamType::Uint(128), ethers::abi::ParamType::Uint(128), ethers::abi::ParamType::Uint(128), ethers::abi::ParamType::Uint(256)], &data.0) {
                    Ok(tokens) => {
                        let snap      = decode_uint(&tokens, 0).unwrap_or_default();
                        let art       = decode_uint(&tokens, 1).unwrap_or_default();
                        let music     = decode_uint(&tokens, 2).unwrap_or_default();
                        let flix      = decode_uint(&tokens, 3).unwrap_or_default();
                        let timestamp = decode_uint(&tokens, 4).unwrap_or_default();
                        Some(ParsedEventData::ContentRequirementsUpdated { snap, art, music, flix, timestamp })
                    }
                    Err(_) => Some(ParsedEventData::Raw { hex: format!("0x{}", hex::encode(data)) }),
                }
            }
        }

        EventType::BurnedContentRevenue => {
            // BurnedContentRevenue(uint256 indexed tokenId, uint256 amount, uint256 timestamp)
            let token_id = indexed_params.first().cloned().unwrap_or_default();
            let (amount, timestamp) = read_amount_timestamp(data);
            Some(ParsedEventData::BurnedContentRevenue { token_id, amount, timestamp })
        }

        // Social follow events
        EventType::Followed => {
            // Followed(address follower, address followed, string followerUsername, string followedUsername, uint256 timestamp)
            let follower = indexed_params.first().cloned().unwrap_or_default();
            let followed = indexed_params.get(1).cloned().unwrap_or_default();

            if data.is_empty() {
                Some(ParsedEventData::Followed {
                    follower,
                    followed,
                    follower_username: String::new(),
                    followed_username: String::new(),
                    timestamp: String::new(),
                })
            } else {
                // Decode dynamic strings + uint256 from data payload
                match ethers::abi::decode(
                    &[
                        ethers::abi::ParamType::String,
                        ethers::abi::ParamType::String,
                        ethers::abi::ParamType::Uint(256),
                    ],
                    &data.0,
                ) {
                    Ok(tokens) => {
                        let follower_username = decode_str(&tokens, 0).unwrap_or_default();
                        let followed_username = decode_str(&tokens, 1).unwrap_or_default();
                        let timestamp         = decode_uint(&tokens, 2).unwrap_or_default();
                        Some(ParsedEventData::Followed { follower, followed, follower_username, followed_username, timestamp })
                    }
                    Err(_) => Some(ParsedEventData::Raw { hex: format!("0x{}", hex::encode(data)) }),
                }
            }
        }

        EventType::UserFollowed | EventType::UserUnfollowed => {
            // UserFollowed(address follower, address target, uint256 timestamp)
            let follower = indexed_params.first().cloned().unwrap_or_default();
            let target = indexed_params.get(1).cloned().unwrap_or_default();
            let timestamp = if data.len() >= 32 {
                U256::from_big_endian(&data[0..32]).to_string()
            } else {
                String::new()
            };

            Some(ParsedEventData::Followed {
                follower,
                followed: target,
                follower_username: String::new(),
                followed_username: String::new(),
                timestamp,
            })
        }

        EventType::ProfileUpdatedExtended => {
            // ProfileUpdatedExtended(address indexed user, string username, string profileHash, string bio, string website, uint256 timestamp)
            if data.is_empty() {
                None
            } else {
                // Decode dynamic strings + uint256 from data payload
                match ethers::abi::decode(
                    &[
                        ethers::abi::ParamType::String,
                        ethers::abi::ParamType::String,
                        ethers::abi::ParamType::String,
                        ethers::abi::ParamType::String,
                        ethers::abi::ParamType::Uint(256),
                    ],
                    &data.0,
                ) {
                    Ok(tokens) => {
                        let username      = decode_str(&tokens, 0).unwrap_or_default();
                        let profile_hash  = decode_str(&tokens, 1).unwrap_or_default();
                        let bio           = decode_str(&tokens, 2).unwrap_or_default();
                        let website       = decode_str(&tokens, 3).unwrap_or_default();
                        let timestamp     = decode_uint(&tokens, 4).unwrap_or_default();
                        Some(ParsedEventData::ProfileUpdatedExtended { username, profile_hash, bio, website, timestamp })
                    }
                    Err(_) => Some(ParsedEventData::Raw { hex: format!("0x{}", hex::encode(data)) }),
                }
            }
        }

        // UsernameRegistered(address indexed user, string username, uint256 timestamp)
        EventType::UsernameRegistered => {
            let user = indexed_params.first().cloned().unwrap_or_default();
            match decode_str_uint256(data) {
                None => Some(ParsedEventData::UsernameRegistered { user, username: String::new(), timestamp: String::new() }),
                Some(Ok((username, timestamp))) => Some(ParsedEventData::UsernameRegistered { user, username, timestamp }),
                Some(Err(hex)) => Some(ParsedEventData::Raw { hex }),
            }
        }

        // ProfileUpdated(address indexed user, string username, uint256 timestamp)
        EventType::ProfileUpdated => {
            let user = indexed_params.first().cloned().unwrap_or_default();
            match decode_str_uint256(data) {
                None => Some(ParsedEventData::ProfileUpdatedSimple { user, username: String::new(), timestamp: String::new() }),
                Some(Ok((username, timestamp))) => Some(ParsedEventData::ProfileUpdatedSimple { user, username, timestamp }),
                Some(Err(hex)) => Some(ParsedEventData::Raw { hex }),
            }
        }

        // UserVerified(address indexed user, uint256 timestamp)
        EventType::UserVerified => {
            let user = indexed_params.first().cloned().unwrap_or_default();
            let timestamp = if data.len() >= 32 { U256::from_big_endian(&data[0..32]).to_string() } else { String::new() };
            Some(ParsedEventData::UserVerifiedEvent { user, timestamp })
        }

        // UserBlocked(address indexed user, bool status, uint256 timestamp)
        EventType::UserBlocked | EventType::UserUnblocked => {
            let user = indexed_params.first().cloned().unwrap_or_default();
            if data.is_empty() {
                Some(ParsedEventData::UserBlockedEvent { user, status: true, timestamp: String::new() })
            } else {
                match ethers::abi::decode(&[ethers::abi::ParamType::Bool, ethers::abi::ParamType::Uint(256)], &data.0) {
                    Ok(tokens) => {
                        let status    = decode_bool(&tokens, 0).unwrap_or(true);
                        let timestamp = decode_uint(&tokens, 1).unwrap_or_default();
                        Some(ParsedEventData::UserBlockedEvent { user, status, timestamp })
                    }
                    Err(_) => Some(ParsedEventData::Raw { hex: format!("0x{}", hex::encode(data)) }),
                }
            }
        }

        // ContentBurned(uint256 indexed tokenId, address indexed owner, uint256 timestamp)
        EventType::ContentBurned => {
            let token_id = indexed_params.first().cloned().unwrap_or_default();
            let owner = indexed_params.get(1).cloned().unwrap_or_default();
            let timestamp = if data.len() >= 32 { U256::from_big_endian(&data[0..32]).to_string() } else { String::new() };
            Some(ParsedEventData::ContentBurned { token_id, owner, timestamp })
        }

        // TokensRecovered(address indexed token, address indexed to, uint256 amount, uint256 timestamp)
        EventType::TokensRecovered => {
            let token = indexed_params.first().cloned().unwrap_or_default();
            let to = indexed_params.get(1).cloned().unwrap_or_default();
            match decode_two_uint256(data) {
                None => Some(ParsedEventData::TokensRecovered { token, to, amount: String::new(), timestamp: String::new() }),
                Some(Ok((amount, timestamp))) => Some(ParsedEventData::TokensRecovered { token, to, amount, timestamp }),
                Some(Err(hex)) => Some(ParsedEventData::Raw { hex }),
            }
        }

        // TipSent(address sender, address recipient, uint256 amount, uint256 timestamp)
        EventType::TipSent => {
            let sender = indexed_params.first().cloned().unwrap_or_default();
            let recipient = indexed_params.get(1).cloned().unwrap_or_default();
            match decode_two_uint256(data) {
                None => Some(ParsedEventData::TipSent { sender, recipient, amount: String::new(), timestamp: String::new() }),
                Some(Ok((amount, timestamp))) => Some(ParsedEventData::TipSent { sender, recipient, amount, timestamp }),
                Some(Err(hex)) => Some(ParsedEventData::Raw { hex }),
            }
        }

        // BadgeAwarded(address user, string badge, uint256 timestamp)
        EventType::BadgeAwarded | EventType::BadgeRemoved => {
            let user = indexed_params.first().cloned().unwrap_or_default();
            if data.is_empty() {
                Some(ParsedEventData::BadgeAwardedData { user, badge: String::new(), timestamp: String::new() })
            } else {
                match ethers::abi::decode(&[ethers::abi::ParamType::String, ethers::abi::ParamType::Uint(256)], &data.0) {
                    Ok(tokens) => {
                        let badge     = decode_str(&tokens, 0).unwrap_or_default();
                        let timestamp = decode_uint(&tokens, 1).unwrap_or_default();
                        if matches!(event_type, EventType::BadgeAwarded) {
                            Some(ParsedEventData::BadgeAwardedData { user, badge, timestamp })
                        } else {
                            Some(ParsedEventData::BadgeRemovedData { user, badge, timestamp })
                        }
                    }
                    Err(_) => Some(ParsedEventData::Raw { hex: format!("0x{}", hex::encode(data)) }),
                }
            }
        }

        // CollabProposed(uint256 tokenId, address proposer, address recipient, uint256 timestamp)
        EventType::CollabProposed => {
            let token_id = indexed_params.first().cloned().unwrap_or_default();
            // Try decoding [address proposer, address recipient, uint256 timestamp] from data
            if data.is_empty() {
                Some(ParsedEventData::CollabProposedData { token_id, proposer: String::new(), recipient: String::new(), timestamp: String::new() })
            } else {
                match ethers::abi::decode(&[ethers::abi::ParamType::Address, ethers::abi::ParamType::Address, ethers::abi::ParamType::Uint(256)], &data.0) {
                    Ok(tokens) => {
                        let proposer  = decode_addr_token(&tokens, 0).unwrap_or_default();
                        let recipient = decode_addr_token(&tokens, 1).unwrap_or_default();
                        let timestamp = decode_uint(&tokens, 2).unwrap_or_default();
                        Some(ParsedEventData::CollabProposedData { token_id, proposer, recipient, timestamp })
                    }
                    Err(_) => Some(ParsedEventData::Raw { hex: format!("0x{}", hex::encode(data)) }),
                }
            }
        }

        // UsernameTransferred(address from, address to, string username, uint256 timestamp)
        EventType::UsernameTransferred => {
            let from = indexed_params.first().cloned().unwrap_or_default();
            let to = indexed_params.get(1).cloned().unwrap_or_default();
            match decode_str_uint256(data) {
                None => Some(ParsedEventData::UsernameTransferredData { from, to, username: String::new(), timestamp: String::new() }),
                Some(Ok((username, timestamp))) => Some(ParsedEventData::UsernameTransferredData { from, to, username, timestamp }),
                Some(Err(hex)) => Some(ParsedEventData::Raw { hex }),
            }
        }

        _ => {
            // For unknown events, just return raw data
            if data.is_empty() {
                None
            } else {
                Some(ParsedEventData::Raw {
                    hex: format!("0x{}", hex::encode(data)),
                })
            }
        }
    }
}

/// Get Kafka key for an event (used for partitioning)
pub fn event_kafka_key(event: &ParsedEvent) -> String {
    // Use contract_address as key for ordering guarantees per contract
    format!("{}.{}", event.contract_type, event.contract_address)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_event_signature_lookup() {
        let sig = keccak256_signature("SnapMinted(uint256,string,address)");
        assert_eq!(EVENT_SIGNATURES.get(&sig), Some(&EventType::SnapMinted));
        let sig2 =
            h256_from_hex("0xe913bf0f321ec4538e6e03894963538ad29d5bc7610699f655b8d4be77ef3c31");
        assert_eq!(EVENT_SIGNATURES.get(&sig2), Some(&EventType::ContentMinted));

        // TheraFriendz social event signatures
        let user_follow_sig = keccak256_signature("UserFollowed(address,address,uint256)");
        assert_eq!(EVENT_SIGNATURES.get(&user_follow_sig), Some(&EventType::UserFollowed));
        let user_unfollow_sig = keccak256_signature("UserUnfollowed(address,address,uint256)");
        assert_eq!(EVENT_SIGNATURES.get(&user_unfollow_sig), Some(&EventType::UserUnfollowed));
    }

    #[test]
    fn test_event_type_contract() {
        assert_eq!(EventType::SnapMinted.contract_type(), "snap");
        assert_eq!(EventType::Followed.contract_type(), "friends");
    }

    #[test]
    fn test_parse_user_followed_event() {
        // Prepare signature and topics
        let sig = keccak256_signature("UserFollowed(address,address,uint256)");
        let follower_topic = h256_from_hex("0x000000000000000000000000aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        let target_topic = h256_from_hex("0x000000000000000000000000bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");

        // timestamp in data (uint256)
        let timestamp = ethers::types::U256::from(1_700_000_500u64);
        let mut data_vec = vec![0u8; 32];
        timestamp.to_big_endian(&mut data_vec);
        let data = ethers::types::Bytes::from(data_vec);

        let mut log = ethers::types::Log::default();
        log.topics = vec![sig, follower_topic, target_topic];
        log.data = data.clone();

        let parsed = parse_log(&log, "friends").expect("parse failed");
        assert_eq!(parsed.event_type, "UserFollowed");
        assert_eq!(parsed.contract_type, "friends");
        assert_eq!(parsed.indexed_params.len(), 2);
        assert_eq!(parsed.indexed_params[0], "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        assert_eq!(parsed.indexed_params[1], "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");

        if let Some(ParsedEventData::Followed { follower, followed, follower_username, followed_username, timestamp }) = parsed.data {
            assert_eq!(follower, "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
            assert_eq!(followed, "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
            assert_eq!(follower_username, "");
            assert_eq!(followed_username, "");
            assert_eq!(timestamp, "1700000500");
        } else {
            panic!("Expected Followed data");
        }
    }

    #[test]
    fn test_parse_profile_updated_extended() {
        use ethers::abi::Token;
        use ethers::types::Bytes;
        // Signature for ProfileUpdatedExtended
        let sig = h256_from_hex("0xb493045fc13318793ba6deaf400d8f23236835ab7c056d18196896cf98fbd9d9");
        let user_topic = h256_from_hex("0x000000000000000000000000aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");

        // Build ABI-encoded data: [username, profileHash, bio, website, timestamp]
        let tokens = vec![
            Token::String("alice".to_string()),
            Token::String("Qmabcdef123".to_string()),
            Token::String("hello bio".to_string()),
            Token::String("https://example.com".to_string()),
            Token::Uint(ethers::types::U256::from(1_700_000_500u64)),
        ];

        let encoded = ethers::abi::encode(&tokens);
        let data = Bytes::from(encoded);

        let mut log = ethers::types::Log::default();
        log.address = ethers::types::H160::from_low_u64_be(0xabc);
        log.topics = vec![sig, user_topic];
        log.data = data.clone();

        let parsed = parse_log(&log, "friends").expect("parse failed");
        assert_eq!(parsed.event_type, "ProfileUpdatedExtended");
        if let Some(ParsedEventData::ProfileUpdatedExtended { username, profile_hash, bio, website, timestamp }) = parsed.data {
            assert_eq!(username, "alice");
            assert_eq!(profile_hash, "Qmabcdef123");
            assert_eq!(bio, "hello bio");
            assert_eq!(website, "https://example.com");
            assert_eq!(timestamp, "1700000500");
        } else {
            panic!("Expected ProfileUpdatedExtended data");
        }
    }

    #[test]
    fn test_parse_content_minted_event() {
        use ethers::types::Bytes;
        // Signature for ContentMinted
        let sig = h256_from_hex("0xe913bf0f321ec4538e6e03894963538ad29d5bc7610699f655b8d4be77ef3c31");
        let token_topic = h256_from_hex("0x000000000000000000000000000000000000000000000000000000000000002a");
        let creator_topic = h256_from_hex("0x000000000000000000000000aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        let content_type_topic = h256_from_hex("0x0000000000000000000000000000000000000000000000000000000000000003");

        // price + timestamp
        let price = ethers::types::U256::from(100u64);
        let timestamp = ethers::types::U256::from(1_700_000_500u64);
        let mut data_vec = vec![0u8; 64];
        price.to_big_endian(&mut data_vec[0..32]);
        timestamp.to_big_endian(&mut data_vec[32..64]);
        let data = Bytes::from(data_vec);

        let mut log = ethers::types::Log::default();
        log.topics = vec![sig, token_topic, creator_topic, content_type_topic];
        log.data = data.clone();

        let parsed = parse_log(&log, "friends").expect("parse failed");
        assert_eq!(parsed.event_type, "ContentMinted");
        if let Some(ParsedEventData::Minted { token_id, creator, content_type, price, timestamp, .. }) = parsed.data {
            assert_eq!(token_id, "42");
            assert_eq!(creator, "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
            assert_eq!(content_type, "3");
            assert_eq!(price, "100");
            assert_eq!(timestamp, "1700000500");
        } else { panic!("Expected Minted data"); }
    }

    #[test]
    fn test_parse_content_liked_event() {
        use ethers::types::Bytes;
        // Signature for ContentLiked
        let sig = h256_from_hex("0x8417b49947e6fe4baaaf043fd8bc39e9a14bdfcac1627dc1c35f75a8e844321b");
        let token_topic = h256_from_hex("0x000000000000000000000000000000000000000000000000000000000000002a");
        let liker_topic = h256_from_hex("0x000000000000000000000000bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        let creator_topic = h256_from_hex("0x000000000000000000000000cccccccccccccccccccccccccccccccccccccccc");

        // contentType + timestamp
        let content_type = ethers::types::U256::from(0u64);
        let timestamp = ethers::types::U256::from(1_700_000_500u64);
        let mut data_vec = vec![0u8; 64];
        content_type.to_big_endian(&mut data_vec[0..32]);
        timestamp.to_big_endian(&mut data_vec[32..64]);
        let data = Bytes::from(data_vec);

        let mut log = ethers::types::Log::default();
        log.topics = vec![sig, token_topic, liker_topic, creator_topic];
        log.data = data.clone();

        let parsed = parse_log(&log, "friends").expect("parse failed");
        assert_eq!(parsed.event_type, "ContentLiked");
        if let Some(ParsedEventData::Liked { token_id, liker, creator, total_likes: _, timestamp }) = parsed.data {
            assert_eq!(token_id, "42");
            assert_eq!(liker, "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
            assert_eq!(creator, "0xcccccccccccccccccccccccccccccccccccccccc");
            assert_eq!(timestamp, "1700000500");
        } else { panic!("Expected Liked data"); }
    }

    #[test]
    fn test_parse_content_commented_event() {
        use ethers::abi::Token;
        use ethers::types::Bytes;
        // Signature for ContentCommented
        let sig = h256_from_hex("0x505d1203546d4a3699987fc90279e0a1dfe65117be15cac29d00ca3ed7a673b6");
        let token_topic = h256_from_hex("0x000000000000000000000000000000000000000000000000000000000000002a");
        let commenter_topic = h256_from_hex("0x000000000000000000000000bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");

        let tokens = vec![
            Token::Uint(ethers::types::U256::from(7u64)),
            Token::String("nice".to_string()),
            Token::Uint(ethers::types::U256::from(0u64)),
            Token::Uint(ethers::types::U256::from(1_700_000_500u64)),
        ];
        let data = Bytes::from(ethers::abi::encode(&tokens));

        let mut log = ethers::types::Log::default();
        log.topics = vec![sig, token_topic, commenter_topic];
        log.data = data.clone();

        let parsed = parse_log(&log, "friends").expect("parse failed");
        assert_eq!(parsed.event_type, "ContentCommented");
        if let Some(ParsedEventData::Commented { token_id, comment_id, commenter, comment, content_type, timestamp }) = parsed.data {
            assert_eq!(token_id, "42");
            assert_eq!(comment_id, "7");
            assert_eq!(commenter, "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
            assert_eq!(comment, "nice");
            assert_eq!(content_type, "0");
            assert_eq!(timestamp, "1700000500");
        } else { panic!("Expected Commented data"); }
    }

    #[test]
    fn test_parse_prices_updated_event() {
        use ethers::types::Bytes;
        let sig = keccak256_signature("PricesUpdated(uint128,uint128,uint128,uint128,uint64,uint256)");
        // values: copy=10, like=1, comment=2, follow=0, fee=2000, timestamp
        let copy = ethers::types::U256::from(10u64);
        let like = ethers::types::U256::from(1u64);
        let comment = ethers::types::U256::from(2u64);
        let follow = ethers::types::U256::from(0u64);
        let fee = ethers::types::U256::from(2000u64);
        let timestamp = ethers::types::U256::from(1_700_000_500u64);
        let encoded = ethers::abi::encode(&[
            ethers::abi::Token::Uint(copy),
            ethers::abi::Token::Uint(like),
            ethers::abi::Token::Uint(comment),
            ethers::abi::Token::Uint(follow),
            ethers::abi::Token::Uint(fee),
            ethers::abi::Token::Uint(timestamp),
        ]);
        let data = Bytes::from(encoded);

        let mut log = ethers::types::Log::default();
        log.topics = vec![sig];
        log.data = data.clone();

        let parsed = parse_log(&log, "friends").expect("parse failed");
        assert_eq!(parsed.event_type, "PricesUpdated");
        if let Some(ParsedEventData::PricesUpdated { copy, like, comment, follow, fee, timestamp }) = parsed.data {
            assert_eq!(copy, "10");
            assert_eq!(like, "1");
            assert_eq!(comment, "2");
            assert_eq!(follow, "0");
            assert_eq!(fee, "2000");
            assert_eq!(timestamp, "1700000500");
        } else { panic!("Expected PricesUpdated data"); }
    }

    #[test]
    fn test_parse_treasury_updated_event() {
        use ethers::types::Bytes;
        let sig = keccak256_signature("TreasuryUpdated(address,address,uint256)");
        let old_topic = h256_from_hex("0x000000000000000000000000aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        let new_topic = h256_from_hex("0x000000000000000000000000bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        let timestamp = ethers::types::U256::from(1_700_000_500u64);
        let mut data_vec = vec![0u8; 32];
        timestamp.to_big_endian(&mut data_vec[0..32]);
        let data = Bytes::from(data_vec);

        let mut log = ethers::types::Log::default();
        log.topics = vec![sig, old_topic, new_topic];
        log.data = data.clone();

        let parsed = parse_log(&log, "friends").expect("parse failed");
        assert_eq!(parsed.event_type, "TreasuryUpdated");
        if let Some(ParsedEventData::TreasuryUpdated { old_treasury, new_treasury, timestamp }) = parsed.data {
            assert_eq!(old_treasury, "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
            assert_eq!(new_treasury, "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
            assert_eq!(timestamp, "1700000500");
        } else { panic!("Expected TreasuryUpdated data"); }
    }

    #[test]
    fn test_parse_daily_limits_updated_event() {
        use ethers::types::Bytes;
        let sig = keccak256_signature("DailyLimitsUpdated(uint64,uint64,uint256)");
        let max_posts = ethers::abi::Token::Uint(ethers::types::U256::from(50u64));
        let max_follows = ethers::abi::Token::Uint(ethers::types::U256::from(100u64));
        let timestamp = ethers::abi::Token::Uint(ethers::types::U256::from(1_700_000_500u64));
        let encoded = ethers::abi::encode(&[max_posts, max_follows, timestamp]);
        let data = Bytes::from(encoded);

        let mut log = ethers::types::Log::default();
        log.topics = vec![sig];
        log.data = data.clone();

        let parsed = parse_log(&log, "friends").expect("parse failed");
        assert_eq!(parsed.event_type, "DailyLimitsUpdated");
        if let Some(ParsedEventData::DailyLimitsUpdated { max_posts, max_follows, timestamp }) = parsed.data {
            assert_eq!(max_posts, "50");
            assert_eq!(max_follows, "100");
            assert_eq!(timestamp, "1700000500");
        } else { panic!("Expected DailyLimitsUpdated data"); }
    }

    #[test]
    fn test_parse_content_requirements_updated_event() {
        use ethers::types::Bytes;
        let sig = keccak256_signature("ContentRequirementsUpdated(uint128,uint128,uint128,uint128,uint256)");
        let snap = ethers::abi::Token::Uint(ethers::types::U256::from(0u64));
        let art = ethers::abi::Token::Uint(ethers::types::U256::from(10u64));
        let music = ethers::abi::Token::Uint(ethers::types::U256::from(50u64));
        let flix = ethers::abi::Token::Uint(ethers::types::U256::from(100u64));
        let timestamp = ethers::abi::Token::Uint(ethers::types::U256::from(1_700_000_500u64));
        let encoded = ethers::abi::encode(&[snap, art, music, flix, timestamp]);
        let data = Bytes::from(encoded);

        let mut log = ethers::types::Log::default();
        log.topics = vec![sig];
        log.data = data.clone();

        let parsed = parse_log(&log, "friends").expect("parse failed");
        assert_eq!(parsed.event_type, "ContentRequirementsUpdated");
        if let Some(ParsedEventData::ContentRequirementsUpdated { snap, art, music, flix, timestamp }) = parsed.data {
            assert_eq!(snap, "0");
            assert_eq!(art, "10");
            assert_eq!(music, "50");
            assert_eq!(flix, "100");
            assert_eq!(timestamp, "1700000500");
        } else { panic!("Expected ContentRequirementsUpdated data"); }
    }

    #[test]
    fn test_parse_burned_content_revenue_event() {
        use ethers::types::Bytes;
        let sig = keccak256_signature("BurnedContentRevenue(uint256,uint256,uint256)");
        let token_topic = h256_from_hex("0x000000000000000000000000000000000000000000000000000000000000002a");
        let amount = ethers::types::U256::from(77u64);
        let timestamp = ethers::types::U256::from(1_700_000_500u64);
        let mut data_vec = vec![0u8; 64];
        amount.to_big_endian(&mut data_vec[0..32]);
        timestamp.to_big_endian(&mut data_vec[32..64]);
        let data = Bytes::from(data_vec);

        let mut log = ethers::types::Log::default();
        log.topics = vec![sig, token_topic];
        log.data = data.clone();

        let parsed = parse_log(&log, "friends").expect("parse failed");
        assert_eq!(parsed.event_type, "BurnedContentRevenue");
        if let Some(ParsedEventData::BurnedContentRevenue { token_id, amount, timestamp }) = parsed.data {
            assert_eq!(token_id, "42");
            assert_eq!(amount, "77");
            assert_eq!(timestamp, "1700000500");
        } else { panic!("Expected BurnedContentRevenue data"); }
    }

    #[test]
    fn test_parse_purchase_processed_event() {
        use ethers::types::Bytes;
        let sig = keccak256_signature("PurchaseProcessed(uint256,address,uint256)");
        let token_topic = h256_from_hex("0x000000000000000000000000000000000000000000000000000000000000002a");
        let buyer_topic = h256_from_hex("0x000000000000000000000000bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        let amount = ethers::types::U256::from(123u64);
        let mut data_vec = vec![0u8; 32];
        amount.to_big_endian(&mut data_vec[0..32]);
        let data = Bytes::from(data_vec);

        let mut log = ethers::types::Log::default();
        log.topics = vec![sig, token_topic, buyer_topic];
        log.data = data.clone();

        let parsed = parse_log(&log, "common").expect("parse failed");
        assert_eq!(parsed.event_type, "PurchaseProcessed");
        if let Some(ParsedEventData::Purchase { token_id, buyer, amount }) = parsed.data {
            assert_eq!(token_id, "42");
            assert_eq!(buyer, "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
            assert_eq!(amount, "123");
        } else { panic!("Expected Purchase data"); }
    }

    #[test]
    fn test_parse_royalty_distributed_event() {
        use ethers::types::Bytes;
        let sig = keccak256_signature("RoyaltyDistributed(uint256,address,uint256,uint256)");
        let token_topic = h256_from_hex("0x000000000000000000000000000000000000000000000000000000000000002a");
        let recipient_topic = h256_from_hex("0x000000000000000000000000bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        let amount = ethers::types::U256::from(50u64);
        let timestamp = ethers::types::U256::from(1_700_000_500u64);
        let mut data_vec = vec![0u8; 64];
        amount.to_big_endian(&mut data_vec[0..32]);
        timestamp.to_big_endian(&mut data_vec[32..64]);
        let data = Bytes::from(data_vec);

        let mut log = ethers::types::Log::default();
        log.topics = vec![sig, token_topic, recipient_topic];
        log.data = data.clone();

        let parsed = parse_log(&log, "friends").expect("parse failed");
        assert_eq!(parsed.event_type, "RoyaltyDistributed");
        if let Some(ParsedEventData::RoyaltyDistributed { token_id, recipient, amount, timestamp }) = parsed.data {
            assert_eq!(token_id, "42");
            assert_eq!(recipient, "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
            assert_eq!(amount, "50");
            assert_eq!(timestamp, "1700000500");
        } else { panic!("Expected RoyaltyDistributed data"); }
    }

    #[test]
    fn test_parse_earnings_withdrawn_event() {
        use ethers::types::Bytes;
        let sig = keccak256_signature("EarningsWithdrawn(address,uint256)");
        let user_topic = h256_from_hex("0x000000000000000000000000bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        let amount = ethers::types::U256::from(200u64);
        let timestamp = ethers::types::U256::from(1_700_000_500u64);
        let mut data_vec = vec![0u8; 64];
        amount.to_big_endian(&mut data_vec[0..32]);
        timestamp.to_big_endian(&mut data_vec[32..64]);
        let data = Bytes::from(data_vec);

        let mut log = ethers::types::Log::default();
        log.topics = vec![sig, user_topic];
        log.data = data.clone();

        let parsed = parse_log(&log, "friends").expect("parse failed");
        assert_eq!(parsed.event_type, "EarningsWithdrawn");
        if let Some(ParsedEventData::EarningsWithdrawn { user, amount, timestamp }) = parsed.data {
            assert_eq!(user, "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
            assert_eq!(amount, "200");
            assert_eq!(timestamp, "1700000500");
        } else { panic!("Expected EarningsWithdrawn data"); }
    }

    #[test]
    fn test_event_type_categories() {
        assert!(EventType::SnapMinted.is_mint());
        assert!(EventType::ArtLiked.is_like());
        assert!(EventType::Followed.is_social());
    }

    #[test]
    fn test_parse_username_registered_event() {
        use ethers::abi::Token;
        use ethers::types::Bytes;
        let sig = keccak256_signature("UsernameRegistered(address,string)");
        let user_topic = h256_from_hex("0x000000000000000000000000aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        let tokens = vec![Token::String("alice".to_string()), Token::Uint(ethers::types::U256::from(1_700_000_500u64))];
        let data = Bytes::from(ethers::abi::encode(&tokens));

        let mut log = ethers::types::Log::default();
        log.topics = vec![sig, user_topic];
        log.data = data.clone();

        let parsed = parse_log(&log, "friends").expect("parse failed");
        assert_eq!(parsed.event_type, "UsernameRegistered");
        if let Some(ParsedEventData::UsernameRegistered { user, username, timestamp }) = parsed.data {
            assert_eq!(user, "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
            assert_eq!(username, "alice");
            assert_eq!(timestamp, "1700000500");
        } else { panic!("Expected UsernameRegistered data"); }
    }

    #[test]
    fn test_parse_profile_updated_event() {
        use ethers::abi::Token;
        use ethers::types::Bytes;
        let sig = keccak256_signature("ProfileUpdated(address,string,uint256)");
        let user_topic = h256_from_hex("0x000000000000000000000000aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        let tokens = vec![Token::String("bob".to_string()), Token::Uint(ethers::types::U256::from(1_700_000_500u64))];
        let data = Bytes::from(ethers::abi::encode(&tokens));

        let mut log = ethers::types::Log::default();
        log.topics = vec![sig, user_topic];
        log.data = data.clone();

        let parsed = parse_log(&log, "friends").expect("parse failed");
        assert_eq!(parsed.event_type, "ProfileUpdated");
        if let Some(ParsedEventData::ProfileUpdatedSimple { user, username, timestamp }) = parsed.data {
            assert_eq!(user, "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
            assert_eq!(username, "bob");
            assert_eq!(timestamp, "1700000500");
        } else { panic!("Expected ProfileUpdated data"); }
    }

    #[test]
    fn test_parse_user_verified_and_blocked_events() {
        use ethers::abi::Token;
        use ethers::types::Bytes;

        // UserVerified
        let sig_v = keccak256_signature("UserVerified(address,uint256)");
        let user_topic = h256_from_hex("0x000000000000000000000000cccccccccccccccccccccccccccccccccccccccc");
        let mut data_vec = vec![0u8; 32];
        ethers::types::U256::from(1_700_000_500u64).to_big_endian(&mut data_vec);
        let data_v = Bytes::from(data_vec.clone());

        let mut log_v = ethers::types::Log::default();
        log_v.topics = vec![sig_v, user_topic];
        log_v.data = data_v.clone();

        let parsed_v = parse_log(&log_v, "friends").expect("parse failed");
        assert_eq!(parsed_v.event_type, "UserVerified");
        if let Some(ParsedEventData::UserVerifiedEvent { user, timestamp }) = parsed_v.data {
            assert_eq!(user, "0xcccccccccccccccccccccccccccccccccccccccc");
            assert_eq!(timestamp, "1700000500");
        } else { panic!("Expected UserVerified data"); }

        // UserBlocked
        let sig_b = keccak256_signature("UserBlocked(address,bool,uint256)");
        let data = ethers::abi::encode(&[Token::Bool(true), Token::Uint(ethers::types::U256::from(1_700_000_500u64))]);
        let data_b = Bytes::from(data);
        let mut log_b = ethers::types::Log::default();
        log_b.topics = vec![sig_b, user_topic];
        log_b.data = data_b.clone();

        let parsed_b = parse_log(&log_b, "friends").expect("parse failed");
        assert_eq!(parsed_b.event_type, "UserBlocked");
        if let Some(ParsedEventData::UserBlockedEvent { user, status, timestamp }) = parsed_b.data {
            assert_eq!(user, "0xcccccccccccccccccccccccccccccccccccccccc");
            assert!(status);
            assert_eq!(timestamp, "1700000500");
        } else { panic!("Expected UserBlocked data"); }
    }

    #[test]
    fn test_parse_content_burned_and_tokens_recovered() {
        use ethers::types::Bytes;
        // ContentBurned
        let sig_cb = keccak256_signature("ContentBurned(uint256,address,uint256)");
        let token_topic = h256_from_hex("0x000000000000000000000000000000000000000000000000000000000000002a");
        let owner_topic = h256_from_hex("0x000000000000000000000000dddddddddddddddddddddddddddddddddddddddd");
        let timestamp = ethers::types::U256::from(1_700_000_500u64);
        let mut data_vec = vec![0u8; 32];
        timestamp.to_big_endian(&mut data_vec[0..32]);
        let data = Bytes::from(data_vec.clone());

        let mut log = ethers::types::Log::default();
        log.topics = vec![sig_cb, token_topic, owner_topic];
        log.data = data.clone();

        let parsed = parse_log(&log, "friends").expect("parse failed");
        assert_eq!(parsed.event_type, "ContentBurned");
        if let Some(ParsedEventData::ContentBurned { token_id, owner, timestamp }) = parsed.data {
            assert_eq!(token_id, "42");
            assert_eq!(owner, "0xdddddddddddddddddddddddddddddddddddddddd");
            assert_eq!(timestamp, "1700000500");
        } else { panic!("Expected ContentBurned data"); }

        // TokensRecovered
        let sig_tr = keccak256_signature("TokensRecovered(address,address,uint256,uint256)");
        let token_topic = h256_from_hex("0x000000000000000000000000eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee");
        let to_topic = h256_from_hex("0x000000000000000000000000ffffffffffffffffffffffffffffffffffffffff");
        let amount = ethers::types::U256::from(500u64);
        let ts = ethers::types::U256::from(1_700_000_500u64);
        let encoded = ethers::abi::encode(&[ethers::abi::Token::Uint(amount), ethers::abi::Token::Uint(ts)]);
        let data_tr = Bytes::from(encoded);

        let mut log_tr = ethers::types::Log::default();
        log_tr.topics = vec![sig_tr, token_topic, to_topic];
        log_tr.data = data_tr.clone();

        let parsed_tr = parse_log(&log_tr, "friends").expect("parse failed");
        assert_eq!(parsed_tr.event_type, "TokensRecovered");
        if let Some(ParsedEventData::TokensRecovered { token, to, amount, timestamp }) = parsed_tr.data {
            assert_eq!(token, "0xeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee");
            assert_eq!(to, "0xffffffffffffffffffffffffffffffffffffffff");
            assert_eq!(amount, "500");
            assert_eq!(timestamp, "1700000500");
        } else { panic!("Expected TokensRecovered data"); }
    }

    #[test]
    fn test_parse_tip_and_badge_events() {
        use ethers::abi::Token;
        use ethers::types::Bytes;

        // TipSent
        let sig = keccak256_signature("TipSent(address,address,uint256,uint256)");
        let sender = h256_from_hex("0x0000000000000000000000001111111111111111111111111111111111111111");
        let recipient = h256_from_hex("0x0000000000000000000000002222222222222222222222222222222222222222");
        let encoded = ethers::abi::encode(&[Token::Uint(ethers::types::U256::from(42u64)), Token::Uint(ethers::types::U256::from(1_700_000_500u64))]);
        let data = Bytes::from(encoded);
        let mut log = ethers::types::Log::default();
        log.topics = vec![sig, sender, recipient];
        log.data = data.clone();
        let parsed = parse_log(&log, "friends").expect("parse failed");
        assert_eq!(parsed.event_type, "TipSent");
        if let Some(ParsedEventData::TipSent { sender, recipient, amount, timestamp }) = parsed.data {
            assert_eq!(sender, "0x1111111111111111111111111111111111111111");
            assert_eq!(recipient, "0x2222222222222222222222222222222222222222");
            assert_eq!(amount, "42");
            assert_eq!(timestamp, "1700000500");
        } else { panic!("Expected TipSent data"); }

        // BadgeAwarded
        let sig_b = keccak256_signature("BadgeAwarded(address,string,uint256)");
        let user = h256_from_hex("0x0000000000000000000000003333333333333333333333333333333333333333");
        let tokens = vec![Token::String("gold".to_string()), Token::Uint(ethers::types::U256::from(1_700_000_500u64))];
        let data_b = Bytes::from(ethers::abi::encode(&tokens));
        let mut log_b = ethers::types::Log::default();
        log_b.topics = vec![sig_b, user];
        log_b.data = data_b.clone();
        let parsed_b = parse_log(&log_b, "friends").expect("parse failed");
        assert_eq!(parsed_b.event_type, "BadgeAwarded");
        if let Some(ParsedEventData::BadgeAwardedData { user, badge, timestamp }) = parsed_b.data {
            assert_eq!(user, "0x3333333333333333333333333333333333333333");
            assert_eq!(badge, "gold");
            assert_eq!(timestamp, "1700000500");
        } else { panic!("Expected BadgeAwarded data"); }
    }

    #[test]
    fn test_parse_collab_and_username_transferred() {
        use ethers::abi::Token;
        use ethers::types::Bytes;

        let sig_collab = keccak256_signature("CollabProposed(uint256,address,address,uint256)");
        // 0x42 hex = 66 decimal
        let token_topic = h256_from_hex("0x0000000000000000000000000000000000000000000000000000000000000042");
        let proposer = ethers::abi::Token::Address(ethers::types::H160::from_low_u64_be(0xabc));
        let recipient = ethers::abi::Token::Address(ethers::types::H160::from_low_u64_be(0xdef));
        let ts = ethers::abi::Token::Uint(ethers::types::U256::from(1_700_000_500u64));
        let encoded = ethers::abi::encode(&[proposer, recipient, ts]);
        let data = Bytes::from(encoded);
        let mut log = ethers::types::Log::default();
        log.topics = vec![sig_collab, token_topic];
        log.data = data.clone();
        let parsed = parse_log(&log, "friends").expect("parse failed");
        assert_eq!(parsed.event_type, "CollabProposed");
        if let Some(ParsedEventData::CollabProposedData { token_id, proposer, recipient, timestamp }) = parsed.data {
            assert_eq!(token_id, "66"); // 0x42 hex = 66 decimal
            assert!(proposer.starts_with("0x"));
            assert!(recipient.starts_with("0x"));
            assert_eq!(timestamp, "1700000500");
        } else { panic!("Expected CollabProposed data"); }

        // UsernameTransferred
        let sig_ut = keccak256_signature("UsernameTransferred(address,address,string,uint256)");
        let from = h256_from_hex("0x0000000000000000000000004444444444444444444444444444444444444444");
        let to = h256_from_hex("0x0000000000000000000000005555555555555555555555555555555555555555");
        let tokens = vec![Token::String("robert".to_string()), Token::Uint(ethers::types::U256::from(1_700_000_500u64))];
        let data_ut = Bytes::from(ethers::abi::encode(&tokens));
        let mut log_ut = ethers::types::Log::default();
        log_ut.topics = vec![sig_ut, from, to];
        log_ut.data = data_ut.clone();
        let parsed_ut = parse_log(&log_ut, "friends").expect("parse failed");
        assert_eq!(parsed_ut.event_type, "UsernameTransferred");
        if let Some(ParsedEventData::UsernameTransferredData { from, to, username, timestamp }) = parsed_ut.data {
            assert_eq!(from, "0x4444444444444444444444444444444444444444");
            assert_eq!(to, "0x5555555555555555555555555555555555555555");
            assert_eq!(username, "robert");
            assert_eq!(timestamp, "1700000500");
        } else { panic!("Expected UsernameTransferred data"); }
    }

    // ── Pure helper unit tests ──────────────────────────────────────────────
    //
    // These tests exercise the three private decode helpers directly.
    // They are reachable here because #[cfg(test)] sub-modules can access
    // private items in their parent module.

    // --- read_amount_timestamp -----------------------------------------------

    #[test]
    fn read_amount_timestamp_happy_path() {
        // 64 bytes of raw big-endian data: amount=10, timestamp=20
        let mut data_vec = vec![0u8; 64];
        ethers::types::U256::from(10u64).to_big_endian(&mut data_vec[0..32]);
        ethers::types::U256::from(20u64).to_big_endian(&mut data_vec[32..64]);
        let data = ethers::types::Bytes::from(data_vec);
        let (amount, timestamp) = read_amount_timestamp(&data);
        assert_eq!(amount, "10");
        assert_eq!(timestamp, "20");
    }

    #[test]
    fn read_amount_timestamp_zero_amount() {
        // Zero amount must return "0", not an error or empty string
        let mut data_vec = vec![0u8; 64];
        ethers::types::U256::from(0u64).to_big_endian(&mut data_vec[0..32]);
        ethers::types::U256::from(999u64).to_big_endian(&mut data_vec[32..64]);
        let data = ethers::types::Bytes::from(data_vec);
        let (amount, timestamp) = read_amount_timestamp(&data);
        assert_eq!(amount, "0");
        assert_eq!(timestamp, "999");
    }

    #[test]
    fn read_amount_timestamp_only_32_bytes() {
        // Only first word present: amount set, timestamp empty
        let mut data_vec = vec![0u8; 32];
        ethers::types::U256::from(42u64).to_big_endian(&mut data_vec[0..32]);
        let data = ethers::types::Bytes::from(data_vec);
        let (amount, timestamp) = read_amount_timestamp(&data);
        assert_eq!(amount, "42");
        assert_eq!(timestamp, "");
    }

    #[test]
    fn read_amount_timestamp_empty_data() {
        // No bytes at all: both fields empty
        let data = ethers::types::Bytes::from(vec![]);
        let (amount, timestamp) = read_amount_timestamp(&data);
        assert_eq!(amount, "");
        assert_eq!(timestamp, "");
    }

    // --- decode_two_uint256 --------------------------------------------------

    #[test]
    fn decode_two_uint256_happy_path() {
        // ABI-encode [Uint(10), Uint(20)] and verify round-trip
        let encoded = ethers::abi::encode(&[
            ethers::abi::Token::Uint(ethers::types::U256::from(10u64)),
            ethers::abi::Token::Uint(ethers::types::U256::from(20u64)),
        ]);
        let data = ethers::types::Bytes::from(encoded);
        let result = decode_two_uint256(&data);
        let (a, b) = result.expect("should be Some").expect("should be Ok");
        assert_eq!(a, "10");
        assert_eq!(b, "20");
    }

    #[test]
    fn decode_two_uint256_large_values() {
        // Test with a realistic amount (1e18 wei) and a Unix timestamp
        let amount = ethers::types::U256::from(1_000_000_000_000_000_000u128); // 1 ether in wei
        let ts = ethers::types::U256::from(1_700_000_500u64);
        let encoded = ethers::abi::encode(&[
            ethers::abi::Token::Uint(amount),
            ethers::abi::Token::Uint(ts),
        ]);
        let data = ethers::types::Bytes::from(encoded);
        let (a, b) = decode_two_uint256(&data).unwrap().unwrap();
        assert_eq!(a, "1000000000000000000");
        assert_eq!(b, "1700000500");
    }

    #[test]
    fn decode_two_uint256_empty_data_returns_none() {
        let data = ethers::types::Bytes::from(vec![]);
        assert!(decode_two_uint256(&data).is_none());
    }

    #[test]
    fn decode_two_uint256_too_short_returns_err() {
        // 31 bytes — not enough for even one uint256 word; ABI decoder should fail
        let data = ethers::types::Bytes::from(vec![0u8; 31]);
        let result = decode_two_uint256(&data);
        // Should be Some(Err(_)) — not None and not Ok
        match result {
            Some(Err(hex_str)) => {
                // Error value is the 0x-prefixed hex of the input
                assert!(hex_str.starts_with("0x"));
            }
            other => panic!("expected Some(Err(hex)), got {:?}", other),
        }
    }

    // --- decode_str_uint256 --------------------------------------------------

    #[test]
    fn decode_str_uint256_happy_path() {
        let encoded = ethers::abi::encode(&[
            ethers::abi::Token::String("alice".to_string()),
            ethers::abi::Token::Uint(ethers::types::U256::from(1_700_000_500u64)),
        ]);
        let data = ethers::types::Bytes::from(encoded);
        let result = decode_str_uint256(&data);
        let (s, u) = result.expect("should be Some").expect("should be Ok");
        assert_eq!(s, "alice");
        assert_eq!(u, "1700000500");
    }

    #[test]
    fn decode_str_uint256_empty_string_portion() {
        // Empty string is valid ABI; should decode to ("", uint_value)
        let encoded = ethers::abi::encode(&[
            ethers::abi::Token::String(String::new()),
            ethers::abi::Token::Uint(ethers::types::U256::from(42u64)),
        ]);
        let data = ethers::types::Bytes::from(encoded);
        let (s, u) = decode_str_uint256(&data).unwrap().unwrap();
        assert_eq!(s, "");
        assert_eq!(u, "42");
    }

    #[test]
    fn decode_str_uint256_empty_data_returns_none() {
        let data = ethers::types::Bytes::from(vec![]);
        assert!(decode_str_uint256(&data).is_none());
    }

    #[test]
    fn decode_str_uint256_invalid_bytes_returns_err() {
        // 32 bytes of 0xff — not a valid ABI-encoded (string, uint256)
        // The string offset would point past the end of the buffer
        let data = ethers::types::Bytes::from(vec![0xffu8; 32]);
        let result = decode_str_uint256(&data);
        match result {
            Some(Err(hex_str)) => {
                assert!(hex_str.starts_with("0x"));
            }
            other => panic!("expected Some(Err(hex)), got {:?}", other),
        }
    }

    // ── Priority routing tests ──────────────────────────────────────────────
    //
    // is_notification_event() drives the 3-tier Kafka topic split introduced
    // in Round 3. A regression here silently routes all notification events
    // to the analytics topic, starving the priority consumer and adding up
    // to 1s latency to UserFollowed / ContentLiked delivery.

    #[test]
    fn test_is_notification_event_positive() {
        // All events that should go to notifications.priority
        assert!(EventType::ContentLiked.is_notification_event(), "ContentLiked");
        assert!(EventType::ContentCommented.is_notification_event(), "ContentCommented");
        assert!(EventType::ContentCopyMinted.is_notification_event(), "ContentCopyMinted");
        assert!(EventType::UserFollowed.is_notification_event(), "UserFollowed");
        assert!(EventType::Followed.is_notification_event(), "Followed (legacy)");
        assert!(EventType::SnapLiked.is_notification_event(), "SnapLiked");
        assert!(EventType::ArtLiked.is_notification_event(), "ArtLiked");
        assert!(EventType::MusicLiked.is_notification_event(), "MusicLiked");
        assert!(EventType::FlixLiked.is_notification_event(), "FlixLiked");
    }

    #[test]
    fn test_is_notification_event_negative() {
        // Analytics/admin events should NOT go to notifications.priority
        assert!(!EventType::ContentMinted.is_notification_event(), "ContentMinted is analytics");
        assert!(!EventType::ContentUnliked.is_notification_event(), "ContentUnliked is analytics");
        assert!(!EventType::UserUnfollowed.is_notification_event(), "UserUnfollowed is analytics");
        assert!(!EventType::ProfileUpdated.is_notification_event(), "ProfileUpdated is analytics");
        assert!(!EventType::RoyaltyDistributed.is_notification_event(), "RoyaltyDistributed is analytics");
        assert!(!EventType::EarningsWithdrawn.is_notification_event(), "EarningsWithdrawn is analytics");
    }

    #[test]
    fn test_kafka_topic_priority_routing() {
        // Verify the 3-tier routing: notification → priority topic
        assert_eq!(EventType::ContentLiked.kafka_topic(), "notifications.priority");
        assert_eq!(EventType::UserFollowed.kafka_topic(), "notifications.priority");
        // Social (non-notification) → user.actions
        assert_eq!(EventType::ProfileUpdated.kafka_topic(), "user.actions");
        // Analytics → blockchain.events
        assert_eq!(EventType::ContentMinted.kafka_topic(), "blockchain.events");
    }

    // ── event_kafka_key ─────────────────────────────────────────────────────

    #[test]
    fn event_kafka_key_format() {
        // event_kafka_key returns "<contract_type>.<contract_address>"
        use ethers::types::Log;
        let mut event = super::ParsedEvent {
            event_type: "SnapMinted".to_string(),
            contract_address: "0xdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef".to_string(),
            contract_type: "snap".to_string(),
            block_number: 1,
            transaction_hash: "0xabc".to_string(),
            log_index: 0,
            timestamp: 0,
            indexed_params: vec![],
            data: None,
            raw_data: None,
            kafka_topic: "blockchain.events",
        };

        let key = super::event_kafka_key(&event);
        assert_eq!(key, "snap.0xdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef");
    }

    #[test]
    fn event_kafka_key_friends_contract() {
        let event = super::ParsedEvent {
            event_type: "UserFollowed".to_string(),
            contract_address: "0x1111111111111111111111111111111111111111".to_string(),
            contract_type: "friends".to_string(),
            block_number: 100,
            transaction_hash: "0xfeed".to_string(),
            log_index: 2,
            timestamp: 1_700_000_000,
            indexed_params: vec![],
            data: None,
            raw_data: None,
            kafka_topic: "notifications.priority",
        };

        let key = super::event_kafka_key(&event);
        assert_eq!(key, "friends.0x1111111111111111111111111111111111111111");
    }

    #[test]
    fn event_kafka_key_common_contract() {
        let event = super::ParsedEvent {
            event_type: "Transfer".to_string(),
            contract_address: "0x0000000000000000000000000000000000000000".to_string(),
            contract_type: "common".to_string(),
            block_number: 0,
            transaction_hash: String::new(),
            log_index: 0,
            timestamp: 0,
            indexed_params: vec![],
            data: None,
            raw_data: None,
            kafka_topic: "blockchain.events",
        };

        // Zero-address edge case: key must still be well-formed
        let key = super::event_kafka_key(&event);
        assert_eq!(key, "common.0x0000000000000000000000000000000000000000");
    }

    // ── EventType::is_purchase ───────────────────────────────────────────────

    #[test]
    fn is_purchase_positive() {
        assert!(EventType::SnapBoughtAndMinted.is_purchase(), "SnapBoughtAndMinted");
        assert!(EventType::ArtBoughtAndMinted.is_purchase(), "ArtBoughtAndMinted");
        assert!(EventType::MusicBoughtAndMinted.is_purchase(), "MusicBoughtAndMinted");
        assert!(EventType::FlixBoughtAndMinted.is_purchase(), "FlixBoughtAndMinted");
        assert!(EventType::PurchaseProcessed.is_purchase(), "PurchaseProcessed");
        assert!(EventType::ContentCopyMinted.is_purchase(), "ContentCopyMinted");
    }

    #[test]
    fn is_purchase_negative() {
        // Minting original content is NOT a purchase
        assert!(!EventType::ContentMinted.is_purchase(), "ContentMinted");
        assert!(!EventType::SnapMinted.is_purchase(), "SnapMinted");
        // Social actions are NOT purchases
        assert!(!EventType::UserFollowed.is_purchase(), "UserFollowed");
        assert!(!EventType::ContentLiked.is_purchase(), "ContentLiked");
        // Admin/common events are NOT purchases
        assert!(!EventType::RoyaltyDistributed.is_purchase(), "RoyaltyDistributed");
        assert!(!EventType::Transfer.is_purchase(), "Transfer");
        assert!(!EventType::Unknown.is_purchase(), "Unknown");
    }

    // ── EventType::contract_type — full coverage ────────────────────────────

    #[test]
    fn contract_type_all_media_types() {
        // art
        assert_eq!(EventType::ArtMinted.contract_type(), "art");
        assert_eq!(EventType::ArtLiked.contract_type(), "art");
        assert_eq!(EventType::ArtCommented.contract_type(), "art");
        assert_eq!(EventType::ArtBoughtAndMinted.contract_type(), "art");
        assert_eq!(EventType::ArtDeleted.contract_type(), "art");

        // music
        assert_eq!(EventType::MusicMinted.contract_type(), "music");
        assert_eq!(EventType::MusicLiked.contract_type(), "music");
        assert_eq!(EventType::MusicCommented.contract_type(), "music");
        assert_eq!(EventType::MusicBoughtAndMinted.contract_type(), "music");
        assert_eq!(EventType::MusicDeleted.contract_type(), "music");

        // flix
        assert_eq!(EventType::FlixMinted.contract_type(), "flix");
        assert_eq!(EventType::FlixLiked.contract_type(), "flix");
        assert_eq!(EventType::FlixCommented.contract_type(), "flix");
        assert_eq!(EventType::FlixBoughtAndMinted.contract_type(), "flix");
        assert_eq!(EventType::FlixDeleted.contract_type(), "flix");
    }

    #[test]
    fn contract_type_common_and_unknown() {
        assert_eq!(EventType::Transfer.contract_type(), "common");
        assert_eq!(EventType::PurchaseProcessed.contract_type(), "common");
        assert_eq!(EventType::RoyaltyDistributed.contract_type(), "common");
        assert_eq!(EventType::BurnedContentRevenue.contract_type(), "common");
        assert_eq!(EventType::CollabProposed.contract_type(), "common");
        assert_eq!(EventType::Unknown.contract_type(), "common");
    }

    #[test]
    fn contract_type_unified_friendz_events() {
        // All unified TheraFriendz content & social events are "friends"
        assert_eq!(EventType::ContentMinted.contract_type(), "friends");
        assert_eq!(EventType::ContentCopyMinted.contract_type(), "friends");
        assert_eq!(EventType::ContentLiked.contract_type(), "friends");
        assert_eq!(EventType::ContentUnliked.contract_type(), "friends");
        assert_eq!(EventType::ContentCommented.contract_type(), "friends");
        assert_eq!(EventType::ContentBlocked.contract_type(), "friends");
        assert_eq!(EventType::ContentBookmarked.contract_type(), "friends");
        assert_eq!(EventType::ContentBurned.contract_type(), "friends");
        assert_eq!(EventType::UserFollowed.contract_type(), "friends");
        assert_eq!(EventType::UserUnfollowed.contract_type(), "friends");
        assert_eq!(EventType::BadgeAwarded.contract_type(), "friends");
        assert_eq!(EventType::BadgeRemoved.contract_type(), "friends");
        assert_eq!(EventType::TipSent.contract_type(), "friends");
        assert_eq!(EventType::PricesUpdated.contract_type(), "friends");
    }

    // ── EventType::is_mint — full coverage ──────────────────────────────────

    #[test]
    fn is_mint_positive_all_variants() {
        assert!(EventType::SnapMinted.is_mint());
        assert!(EventType::ArtMinted.is_mint());
        assert!(EventType::MusicMinted.is_mint());
        assert!(EventType::FlixMinted.is_mint());
        assert!(EventType::ContentMinted.is_mint());
    }

    #[test]
    fn is_mint_negative() {
        // Copy mint (purchase) is NOT is_mint
        assert!(!EventType::ContentCopyMinted.is_mint());
        assert!(!EventType::SnapBoughtAndMinted.is_mint());
        assert!(!EventType::Unknown.is_mint());
        assert!(!EventType::Transfer.is_mint());
    }

    // ── EventType::is_like — full coverage ──────────────────────────────────

    #[test]
    fn is_like_positive_all_variants() {
        assert!(EventType::SnapLiked.is_like());
        assert!(EventType::ArtLiked.is_like());
        assert!(EventType::MusicLiked.is_like());
        assert!(EventType::FlixLiked.is_like());
        assert!(EventType::ContentLiked.is_like());
        // ContentUnliked is included in is_like
        assert!(EventType::ContentUnliked.is_like());
    }

    #[test]
    fn is_like_negative() {
        assert!(!EventType::ContentMinted.is_like());
        assert!(!EventType::UserFollowed.is_like());
        assert!(!EventType::Unknown.is_like());
    }

    // ── EventType::is_social — full coverage ────────────────────────────────

    #[test]
    fn is_social_positive_all_variants() {
        assert!(EventType::Followed.is_social());
        assert!(EventType::Unfollowed.is_social());
        assert!(EventType::UsernameRegistered.is_social());
        assert!(EventType::UsernameTransferred.is_social());
        assert!(EventType::ProfileUpdated.is_social());
        assert!(EventType::NotificationEvent.is_social());
        assert!(EventType::EarningsWithdrawn.is_social());
        assert!(EventType::UserVerified.is_social());
        assert!(EventType::UserUnverified.is_social());
        assert!(EventType::UserBlocked.is_social());
        assert!(EventType::UserUnblocked.is_social());
        assert!(EventType::UserFollowed.is_social());
        assert!(EventType::UserUnfollowed.is_social());
        assert!(EventType::BadgeAwarded.is_social());
        assert!(EventType::BadgeRemoved.is_social());
        assert!(EventType::TipSent.is_social());
        assert!(EventType::PricesUpdated.is_social());
        assert!(EventType::ContentBookmarked.is_social());
        assert!(EventType::ContentShared.is_social());
    }

    #[test]
    fn is_social_negative() {
        // Content creation and purchases are NOT social
        assert!(!EventType::ContentMinted.is_social());
        assert!(!EventType::ContentLiked.is_social());
        assert!(!EventType::SnapBoughtAndMinted.is_social());
        assert!(!EventType::Transfer.is_social());
        assert!(!EventType::Unknown.is_social());
    }

    // ── parse_log: empty-topics fallback and unknown-event raw data ──────────

    #[test]
    fn parse_log_empty_topics_returns_unknown() {
        // A log with no topics should produce EventType::Unknown
        let log = ethers::types::Log::default();
        let parsed = parse_log(&log, "snap").expect("parse failed");
        assert_eq!(parsed.event_type, "Unknown");
        assert_eq!(parsed.contract_type, "snap"); // uses fallback_contract_type
        assert!(parsed.indexed_params.is_empty());
    }

    #[test]
    fn parse_log_unrecognised_topic_uses_fallback_contract_type() {
        // A log whose topic0 is not in EVENT_SIGNATURES → Unknown event,
        // contract_type falls back to the caller-supplied value.
        let mystery_sig = h256_from_hex(
            "0xffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
        );
        let mut log = ethers::types::Log::default();
        log.topics = vec![mystery_sig];
        let parsed = parse_log(&log, "art").expect("parse failed");
        assert_eq!(parsed.event_type, "Unknown");
        assert_eq!(parsed.contract_type, "art");
    }

    #[test]
    fn parse_log_unknown_event_with_data_produces_raw() {
        // Unknown event with non-empty data → raw_data is Some(hex)
        let mystery_sig = h256_from_hex(
            "0xffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
        );
        let mut log = ethers::types::Log::default();
        log.topics = vec![mystery_sig];
        log.data = ethers::types::Bytes::from(vec![0xde, 0xad, 0xbe, 0xef]);
        let parsed = parse_log(&log, "common").expect("parse failed");
        // raw_data should contain the hex-encoded payload
        let raw = parsed.raw_data.expect("expected raw_data");
        assert!(raw.starts_with("0x"));
        assert!(raw.contains("deadbeef"));
    }

    // ── ContentMinted contentType → contract_type mapping ───────────────────

    #[test]
    fn parse_content_minted_content_type_mapping() {
        // contentType encoding: 0→art, 1→flix, 2→music, 3→snap
        let sig = h256_from_hex(
            "0xe913bf0f321ec4538e6e03894963538ad29d5bc7610699f655b8d4be77ef3c31",
        );
        let token_topic =
            h256_from_hex("0x0000000000000000000000000000000000000000000000000000000000000001");
        let creator_topic =
            h256_from_hex("0x000000000000000000000000aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");

        let cases: &[(u64, &str)] = &[(0, "art"), (1, "flix"), (2, "music"), (3, "snap")];

        for (ct_value, expected_type) in cases {
            // Encode contentType as a 32-byte big-endian word in topic3
            let mut ct_bytes = [0u8; 32];
            ct_bytes[31] = *ct_value as u8;
            let ct_topic = ethers::types::H256::from(ct_bytes);

            // data: price=0, timestamp=0 (64 zero bytes)
            let data = ethers::types::Bytes::from(vec![0u8; 64]);

            let mut log = ethers::types::Log::default();
            log.topics = vec![sig, token_topic, creator_topic, ct_topic];
            log.data = data;

            let parsed = parse_log(&log, "friends").expect("parse failed");
            assert_eq!(
                parsed.contract_type, *expected_type,
                "contentType={ct_value} should map to contract_type={expected_type}"
            );
        }
    }
}

