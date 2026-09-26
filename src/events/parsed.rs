//! ParsedEvent and ParsedEventData — output types from parse_log.

use serde::Serialize;

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
    Minted {
        token_id: String,
        uri: String,
        creator: String,
        content_type: String,
        price: String,
        timestamp: String,
    },
    CopyMinted {
        original_id: String,
        buyer: String,
        new_token_id: String,
        content_type: String,
        timestamp: String,
    },
    // Fired alongside CopyMinted (same tx), only when the sale was
    // attributed to a curator's referral link. `curator_fee` is TREEZ wei.
    CopyMintedViaReferral {
        original_id: String,
        buyer: String,
        new_token_id: String,
        referrer: String,
        curator_fee: String,
        timestamp: String,
    },
    Liked {
        token_id: String,
        liker: String,
        creator: String,
        total_likes: String,
        timestamp: String,
    },
    ListingUpdated {
        original_token_id: String,
        price: String,
        max_copies: String,
        timestamp: String,
    },
    Bookmarked {
        token_id: String,
        user: String,
        bookmarked: bool,
        timestamp: String,
    },
    Shared {
        token_id: String,
        sharer: String,
        recipient: String,
        timestamp: String,
    },
    BoughtAndMinted {
        token_id: String,
        buyer: String,
        seller: String,
        price: String,
        new_token_id: String,
    },
    Deleted { token_id: String, deleter: String },
    Followed {
        follower: String,
        followed: String,
        follower_username: String,
        followed_username: String,
        timestamp: String,
    },
    ProfileUpdatedExtended {
        username: String,
        profile_hash: String,
        bio: String,
        website: String,
        timestamp: String,
    },
    Transfer {
        from: String,
        to: String,
        token_id: String,
    },
    Purchase {
        token_id: String,
        buyer: String,
        amount: String,
    },
    RoyaltyDistributed {
        token_id: String,
        recipient: String,
        amount: String,
        timestamp: String,
    },
    EarningsWithdrawn {
        user: String,
        amount: String,
        timestamp: String,
    },
    PlatformFeeUpdated {
        fee: String,
        timestamp: String,
    },
    TreasuryUpdated {
        old_treasury: String,
        new_treasury: String,
        timestamp: String,
    },
    BurnedContentRevenue {
        token_id: String,
        amount: String,
        timestamp: String,
    },
    UsernameRegistered {
        user: String,
        username: String,
        timestamp: String,
    },
    ProfileUpdatedSimple {
        user: String,
        username: String,
        timestamp: String,
    },
    UserVerifiedEvent {
        user: String,
        timestamp: String,
    },
    UserBlockedEvent {
        user: String,
        status: bool,
        timestamp: String,
    },
    ContentBurned {
        token_id: String,
        owner: String,
        timestamp: String,
    },
    TokensRecovered {
        token: String,
        to: String,
        amount: String,
        timestamp: String,
    },
    TipSent {
        sender: String,
        recipient: String,
        amount: String,
        timestamp: String,
    },
    BadgeAwardedData {
        user: String,
        badge: String,
        timestamp: String,
    },
    BadgeRemovedData {
        user: String,
        badge: String,
        timestamp: String,
    },
    CollabProposedData {
        token_id: String,
        proposer: String,
        recipient: String,
        timestamp: String,
    },
    UsernameTransferredData {
        from: String,
        to: String,
        username: String,
        timestamp: String,
    },
    Raw { hex: String },
}
