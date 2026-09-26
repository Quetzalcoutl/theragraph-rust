//! Indexed-parameter type dispatch for EVM log topics.
//!
//! EVM ABI rules:
//! - Address: right-aligned in 32 bytes; extract last 20 bytes
//! - uint256: full 32-byte big-endian word
//! - bytes32: full 32 bytes as hex
//!
//! All items are `pub(super)` — callers go through `parse_log` in `mod.rs`.

use alloy::primitives::{B256, U256};

use super::EventType;

/// Encoding of a single indexed topic slot.
#[derive(Debug, Clone, Copy)]
pub(super) enum IndexedParamType {
    Address,
    Uint256,
    Bytes32,
}

/// Build the decoded indexed-params list from a log's topic slice.
pub(super) fn extract_indexed_params(event_type: &EventType, topics: &[B256]) -> Vec<String> {
    topics
        .iter()
        .skip(1)
        .enumerate()
        .map(|(idx, topic)| format_indexed_param(event_type, idx, topic))
        .collect()
}

fn format_indexed_param(event_type: &EventType, param_index: usize, topic: &B256) -> String {
    match get_indexed_param_type(event_type, param_index) {
        IndexedParamType::Address => format!("0x{}", hex::encode(&topic[12..])),
        IndexedParamType::Uint256 => U256::from_be_slice(topic.as_ref()).to_string(),
        IndexedParamType::Bytes32 => format!("{topic:#x}"),
    }
}

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

        // Unified TheraFriendz events
        EventType::ContentMinted => match param_index {
            0 => IndexedParamType::Uint256, // tokenId
            1 => IndexedParamType::Address, // creator
            2 => IndexedParamType::Uint256, // contentType (uint8 encoded as uint256)
            _ => IndexedParamType::Bytes32,
        },
        EventType::ContentCopyMinted | EventType::ContentCopyMintedViaReferral => match param_index {
            0 => IndexedParamType::Uint256, // originalId
            1 => IndexedParamType::Address, // buyer
            2 => IndexedParamType::Uint256, // newTokenId
            _ => IndexedParamType::Bytes32,
        },
        // ListingUpdated(uint256 indexed originalTokenId, uint256 price, uint256 maxCopies, uint256 timestamp)
        EventType::ListingUpdated => match param_index {
            0 => IndexedParamType::Uint256, // originalTokenId
            _ => IndexedParamType::Bytes32,
        },
        EventType::ContentBlocked | EventType::ContentBookmarked => {
            match param_index {
                0 => IndexedParamType::Uint256, // tokenId
                1 => IndexedParamType::Address, // moderator / user
                _ => IndexedParamType::Bytes32,
            }
        }
        EventType::ContentShared => match param_index {
            0 => IndexedParamType::Uint256, // tokenId
            1 => IndexedParamType::Address, // sharer
            2 => IndexedParamType::Address, // recipient
            _ => IndexedParamType::Bytes32,
        },

        // Liked events: (uint256 indexed tokenId, ...)
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

        // Social follow events (legacy contract only — unified UserFollowed/UserUnfollowed retired)
        EventType::Followed
        | EventType::Unfollowed => match param_index {
            0 => IndexedParamType::Address, // follower
            1 => IndexedParamType::Address, // followed / target
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

        // PlatformFeeUpdated(uint64 fee, uint256 timestamp) — neither param indexed.
        EventType::PlatformFeeUpdated => IndexedParamType::Bytes32,

        EventType::Unknown => IndexedParamType::Bytes32,
    }
}
