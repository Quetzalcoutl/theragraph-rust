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

    // === TheraFriends Events ===
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

    // Newer TheraFriends social events (UserFollowed/UserUnfollowed)
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

    // Unified TheraFriends content & social events (new contract)
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
        h256_from_hex("0xe913bf0f321ec4538e6e03894963538ad29d5bc7610699f655b8d4be77ef3c31"),
        EventType::ContentMinted,
    );
    m.insert(
        h256_from_hex("0x80c2e061ec45ed7331a60555bbadc701bd26c6335bcd10063bc2fe287d040f2f"),
        EventType::ContentCopyMinted,
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
            "PricesUpdated(uint128,uint128,uint128,uint128,uint128,uint64,uint64,uint256)",
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
    let bytes = hex::decode(&hex[2..]).unwrap();
    H256::from_slice(&bytes)
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

    // Unified TheraFriends content & social events
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

    /// Get Kafka topic for this event type
    pub fn kafka_topic(&self) -> &'static str {
        if self.is_social() {
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
                _ => contract_type,
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

        // Unified TheraFriends events - map indexed params per event
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
            0 => IndexedParamType::Address, // blocker
            1 => IndexedParamType::Address, // blocked
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

        // Default to bytes32 for unknown types
        EventType::CollabProposed
        | EventType::BadgeAwarded
        | EventType::BadgeRemoved
        | EventType::TipSent
        | EventType::PricesUpdated
        | EventType::ContentRequirementsUpdated
        | EventType::ContentBurned
        | EventType::BurnedContentRevenue
        | EventType::TreasuryUpdated
        | EventType::DailyLimitsUpdated
        | EventType::TokensRecovered => IndexedParamType::Bytes32,
        EventType::Unknown => IndexedParamType::Bytes32,
    }
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
                        use ethers::abi::Token;
                        let comment_id = tokens
                            .get(0)
                            .and_then(|t| match t { Token::Uint(u) => Some(u.to_string()), _ => None })
                            .unwrap_or_default();
                        let comment = tokens
                            .get(1)
                            .and_then(|t| match t { Token::String(s) => Some(s.clone()), _ => None })
                            .unwrap_or_default();
                        let content_type = tokens
                            .get(2)
                            .and_then(|t| match t { Token::Uint(u) => Some(u.to_string()), _ => None })
                            .unwrap_or_default();
                        let timestamp = tokens
                            .get(3)
                            .and_then(|t| match t { Token::Uint(u) => Some(u.to_string()), _ => None })
                            .unwrap_or_default();

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
                        use ethers::abi::Token;
                        let follower_username = tokens
                            .get(0)
                            .and_then(|t| match t { Token::String(s) => Some(s.clone()), _ => None })
                            .unwrap_or_default();
                        let followed_username = tokens
                            .get(1)
                            .and_then(|t| match t { Token::String(s) => Some(s.clone()), _ => None })
                            .unwrap_or_default();
                        let timestamp = tokens
                            .get(2)
                            .and_then(|t| match t { Token::Uint(u) => Some(u.to_string()), _ => None })
                            .unwrap_or_default();

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
                        use ethers::abi::Token;
                        let username = tokens
                            .get(0)
                            .and_then(|t| match t { Token::String(s) => Some(s.clone()), _ => None })
                            .unwrap_or_default();
                        let profile_hash = tokens
                            .get(1)
                            .and_then(|t| match t { Token::String(s) => Some(s.clone()), _ => None })
                            .unwrap_or_default();
                        let bio = tokens
                            .get(2)
                            .and_then(|t| match t { Token::String(s) => Some(s.clone()), _ => None })
                            .unwrap_or_default();
                        let website = tokens
                            .get(3)
                            .and_then(|t| match t { Token::String(s) => Some(s.clone()), _ => None })
                            .unwrap_or_default();
                        let timestamp = tokens
                            .get(4)
                            .and_then(|t| match t { Token::Uint(u) => Some(u.to_string()), _ => None })
                            .unwrap_or_default();

                        Some(ParsedEventData::ProfileUpdatedExtended { username, profile_hash, bio, website, timestamp })
                    }
                    Err(_) => Some(ParsedEventData::Raw { hex: format!("0x{}", hex::encode(data)) }),
                }
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

        // TheraFriends social event signatures
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
    fn test_event_type_categories() {
        assert!(EventType::SnapMinted.is_mint());
        assert!(EventType::ArtLiked.is_like());
        assert!(EventType::Followed.is_social());
    }
}
