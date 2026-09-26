//! `parse_event_data` — ABI decode dispatch for all supported event types.
//!
//! Each arm reads `indexed_params` and `data` for one event variant and returns
//! the strongly-typed `ParsedEventData` value, or `None` when the event carries
//! no structured payload.

use alloy::dyn_abi::{DynSolType, DynSolValue};
use alloy::primitives::{Bytes, U256};

use super::abi::{
    abi_tuple, decode_addr_token, decode_bool, decode_str, decode_str_uint256, decode_two_uint256,
    decode_uint, read_amount_timestamp,
};
use super::{EventType, ParsedEventData};

/// Decode `data` as an ABI tuple of `types`, calling `f` on success.
/// On decode failure, returns `ParsedEventData::Raw` so callers never silently drop events.
fn decode_tuple<F>(types: Vec<DynSolType>, data: &Bytes, f: F) -> Option<ParsedEventData>
where
    F: FnOnce(&[DynSolValue]) -> ParsedEventData,
{
    match abi_tuple(types, data) {
        Some(vals) => Some(f(&vals)),
        None => Some(ParsedEventData::Raw { hex: format!("0x{}", hex::encode(data)) }),
    }
}

pub(super) fn parse_event_data(
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
                U256::from_be_slice(&data[0..32]).to_string()
            } else {
                String::new()
            };
            let timestamp = if data.len() >= 64 {
                U256::from_be_slice(&data[32..64]).to_string()
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
                U256::from_be_slice(&data[0..32]).to_string()
            } else {
                String::new()
            };
            let timestamp = if data.len() >= 64 {
                U256::from_be_slice(&data[32..64]).to_string()
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

        EventType::ContentCopyMintedViaReferral => {
            // ContentCopyMintedViaReferral(uint256 indexed originalId, address indexed buyer, uint256 indexed newTokenId, address referrer, uint256 curatorFee, uint256 timestamp)
            let original_id = indexed_params.first().cloned().unwrap_or_default();
            let buyer = indexed_params.get(1).cloned().unwrap_or_default();
            let new_token_id = indexed_params.get(2).cloned().unwrap_or_default();

            decode_tuple(
                vec![DynSolType::Address, DynSolType::Uint(256), DynSolType::Uint(256)],
                data,
                |vals| ParsedEventData::CopyMintedViaReferral {
                    original_id,
                    buyer,
                    new_token_id,
                    referrer:    decode_addr_token(vals, 0).unwrap_or_default(),
                    curator_fee: decode_uint(vals, 1).unwrap_or_default(),
                    timestamp:   decode_uint(vals, 2).unwrap_or_default(),
                },
            )
        }

        EventType::ListingUpdated => {
            // ListingUpdated(uint256 indexed originalTokenId, uint256 price, uint256 maxCopies, uint256 timestamp)
            let original_token_id = indexed_params.first().cloned().unwrap_or_default();

            if data.is_empty() {
                Some(ParsedEventData::ListingUpdated {
                    original_token_id,
                    price: String::new(),
                    max_copies: String::new(),
                    timestamp: String::new(),
                })
            } else {
                decode_tuple(
                    vec![DynSolType::Uint(256), DynSolType::Uint(256), DynSolType::Uint(256)],
                    data,
                    |vals| ParsedEventData::ListingUpdated {
                        original_token_id,
                        price:      decode_uint(vals, 0).unwrap_or_default(),
                        max_copies: decode_uint(vals, 1).unwrap_or_default(),
                        timestamp:  decode_uint(vals, 2).unwrap_or_default(),
                    },
                )
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
                U256::from_be_slice(&data[0..32]) != U256::ZERO
            } else {
                true
            };
            let timestamp = if data.len() >= 64 {
                U256::from_be_slice(&data[32..64]).to_string()
            } else {
                String::new()
            };
            Some(ParsedEventData::Bookmarked {
                token_id,
                user,
                bookmarked,
                timestamp,
            })
        }

        EventType::ContentShared => {
            // ContentShared(uint256 tokenId, address sharer, address recipient, uint256 timestamp)
            let token_id = indexed_params.first().cloned().unwrap_or_default();
            let sharer = indexed_params.get(1).cloned().unwrap_or_default();
            let recipient = indexed_params.get(2).cloned().unwrap_or_default();
            let timestamp = if data.len() >= 32 {
                U256::from_be_slice(&data[0..32]).to_string()
            } else {
                String::new()
            };
            Some(ParsedEventData::Shared {
                token_id,
                sharer,
                recipient,
                timestamp,
            })
        }

        // Legacy per-contract minted events
        EventType::SnapMinted
        | EventType::ArtMinted
        | EventType::MusicMinted
        | EventType::FlixMinted => {
            // Minted(uint256 indexed tokenId, string uri, address creator)
            let token_id = indexed_params.first().cloned().unwrap_or_default();
            if data.len() >= 64 {
                let creator = format!("0x{}", hex::encode(&data[12..32]));
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

        // Legacy per-contract liked events
        EventType::SnapLiked
        | EventType::ArtLiked
        | EventType::MusicLiked
        | EventType::FlixLiked => {
            // Liked(uint256 indexed tokenId, address liker, uint256 totalLikes)
            let token_id = indexed_params.first().cloned().unwrap_or_default();
            if data.len() >= 64 {
                let liker = format!("0x{}", hex::encode(&data[12..32]));
                let total_likes = U256::from_be_slice(&data[32..64]).to_string();
                let timestamp = if data.len() >= 96 {
                    U256::from_be_slice(&data[64..96]).to_string()
                } else {
                    String::new()
                };
                Some(ParsedEventData::Liked {
                    token_id,
                    liker,
                    creator: String::new(),
                    total_likes,
                    timestamp,
                })
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
                U256::from_be_slice(&data[0..32]).to_string()
            } else {
                String::new()
            };
            Some(ParsedEventData::Purchase {
                token_id,
                buyer,
                amount,
            })
        }

        EventType::RoyaltyDistributed => {
            // RoyaltyDistributed(uint256 indexed tokenId, address indexed recipient, uint256 amount, uint256 timestamp)
            let token_id = indexed_params.first().cloned().unwrap_or_default();
            let recipient = indexed_params.get(1).cloned().unwrap_or_default();
            let (amount, timestamp) = read_amount_timestamp(data);
            Some(ParsedEventData::RoyaltyDistributed {
                token_id,
                recipient,
                amount,
                timestamp,
            })
        }

        EventType::EarningsWithdrawn => {
            // EarningsWithdrawn(address indexed user, uint256 amount, uint256 timestamp)
            let user = indexed_params.first().cloned().unwrap_or_default();
            let (amount, timestamp) = read_amount_timestamp(data);
            Some(ParsedEventData::EarningsWithdrawn {
                user,
                amount,
                timestamp,
            })
        }

        EventType::PlatformFeeUpdated => {
            // PlatformFeeUpdated(uint64 fee, uint256 timestamp) — neither param indexed.
            if data.is_empty() {
                Some(ParsedEventData::PlatformFeeUpdated {
                    fee: String::new(),
                    timestamp: String::new(),
                })
            } else {
                decode_tuple(
                    vec![DynSolType::Uint(64), DynSolType::Uint(256)],
                    data,
                    |vals| ParsedEventData::PlatformFeeUpdated {
                        fee:       decode_uint(vals, 0).unwrap_or_default(),
                        timestamp: decode_uint(vals, 1).unwrap_or_default(),
                    },
                )
            }
        }

        EventType::TreasuryUpdated => {
            // TreasuryUpdated(address indexed oldTreasury, address indexed newTreasury, uint256 timestamp)
            let old = indexed_params.first().cloned().unwrap_or_default();
            let new = indexed_params.get(1).cloned().unwrap_or_default();
            let timestamp = if data.len() >= 32 {
                U256::from_be_slice(&data[0..32]).to_string()
            } else {
                String::new()
            };
            Some(ParsedEventData::TreasuryUpdated {
                old_treasury: old,
                new_treasury: new,
                timestamp,
            })
        }

        EventType::BurnedContentRevenue => {
            // BurnedContentRevenue(uint256 indexed tokenId, uint256 amount, uint256 timestamp)
            let token_id = indexed_params.first().cloned().unwrap_or_default();
            let (amount, timestamp) = read_amount_timestamp(data);
            Some(ParsedEventData::BurnedContentRevenue {
                token_id,
                amount,
                timestamp,
            })
        }

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
                decode_tuple(
                    vec![DynSolType::String, DynSolType::String, DynSolType::Uint(256)],
                    data,
                    |vals| ParsedEventData::Followed {
                        follower,
                        followed,
                        follower_username: decode_str(vals, 0).unwrap_or_default(),
                        followed_username: decode_str(vals, 1).unwrap_or_default(),
                        timestamp:         decode_uint(vals, 2).unwrap_or_default(),
                    },
                )
            }
        }

        EventType::ProfileUpdatedExtended => {
            // ProfileUpdatedExtended(address indexed user, string username, string profileHash, string bio, string website, uint256 timestamp)
            if data.is_empty() {
                None
            } else {
                decode_tuple(
                    vec![DynSolType::String, DynSolType::String, DynSolType::String, DynSolType::String, DynSolType::Uint(256)],
                    data,
                    |vals| ParsedEventData::ProfileUpdatedExtended {
                        username:     decode_str(vals, 0).unwrap_or_default(),
                        profile_hash: decode_str(vals, 1).unwrap_or_default(),
                        bio:          decode_str(vals, 2).unwrap_or_default(),
                        website:      decode_str(vals, 3).unwrap_or_default(),
                        timestamp:    decode_uint(vals, 4).unwrap_or_default(),
                    },
                )
            }
        }

        EventType::UsernameRegistered => {
            // UsernameRegistered(address indexed user, string username, uint256 timestamp)
            let user = indexed_params.first().cloned().unwrap_or_default();
            match decode_str_uint256(data) {
                None => Some(ParsedEventData::UsernameRegistered {
                    user,
                    username: String::new(),
                    timestamp: String::new(),
                }),
                Some(Ok((username, timestamp))) => {
                    Some(ParsedEventData::UsernameRegistered { user, username, timestamp })
                }
                Some(Err(hex)) => Some(ParsedEventData::Raw { hex }),
            }
        }

        EventType::ProfileUpdated => {
            // ProfileUpdated(address indexed user, string username, uint256 timestamp)
            let user = indexed_params.first().cloned().unwrap_or_default();
            match decode_str_uint256(data) {
                None => Some(ParsedEventData::ProfileUpdatedSimple {
                    user,
                    username: String::new(),
                    timestamp: String::new(),
                }),
                Some(Ok((username, timestamp))) => {
                    Some(ParsedEventData::ProfileUpdatedSimple { user, username, timestamp })
                }
                Some(Err(hex)) => Some(ParsedEventData::Raw { hex }),
            }
        }

        EventType::UserVerified => {
            // UserVerified(address indexed user, uint256 timestamp)
            let user = indexed_params.first().cloned().unwrap_or_default();
            let timestamp = if data.len() >= 32 {
                U256::from_be_slice(&data[0..32]).to_string()
            } else {
                String::new()
            };
            Some(ParsedEventData::UserVerifiedEvent { user, timestamp })
        }

        EventType::UserBlocked | EventType::UserUnblocked => {
            // UserBlocked(address indexed user, bool status, uint256 timestamp)
            let user = indexed_params.first().cloned().unwrap_or_default();
            if data.is_empty() {
                Some(ParsedEventData::UserBlockedEvent {
                    user,
                    status: true,
                    timestamp: String::new(),
                })
            } else {
                decode_tuple(
                    vec![DynSolType::Bool, DynSolType::Uint(256)],
                    data,
                    |vals| ParsedEventData::UserBlockedEvent {
                        user,
                        status:    decode_bool(vals, 0).unwrap_or(true),
                        timestamp: decode_uint(vals, 1).unwrap_or_default(),
                    },
                )
            }
        }

        EventType::ContentBurned => {
            // ContentBurned(uint256 indexed tokenId, address indexed owner, uint256 timestamp)
            let token_id = indexed_params.first().cloned().unwrap_or_default();
            let owner = indexed_params.get(1).cloned().unwrap_or_default();
            let timestamp = if data.len() >= 32 {
                U256::from_be_slice(&data[0..32]).to_string()
            } else {
                String::new()
            };
            Some(ParsedEventData::ContentBurned { token_id, owner, timestamp })
        }

        EventType::TokensRecovered => {
            // TokensRecovered(address indexed token, address indexed to, uint256 amount, uint256 timestamp)
            let token = indexed_params.first().cloned().unwrap_or_default();
            let to = indexed_params.get(1).cloned().unwrap_or_default();
            match decode_two_uint256(data) {
                None => Some(ParsedEventData::TokensRecovered {
                    token,
                    to,
                    amount: String::new(),
                    timestamp: String::new(),
                }),
                Some(Ok((amount, timestamp))) => {
                    Some(ParsedEventData::TokensRecovered { token, to, amount, timestamp })
                }
                Some(Err(hex)) => Some(ParsedEventData::Raw { hex }),
            }
        }

        EventType::TipSent => {
            // TipSent(address sender, address recipient, uint256 amount, uint256 timestamp)
            let sender = indexed_params.first().cloned().unwrap_or_default();
            let recipient = indexed_params.get(1).cloned().unwrap_or_default();
            match decode_two_uint256(data) {
                None => Some(ParsedEventData::TipSent {
                    sender,
                    recipient,
                    amount: String::new(),
                    timestamp: String::new(),
                }),
                Some(Ok((amount, timestamp))) => {
                    Some(ParsedEventData::TipSent { sender, recipient, amount, timestamp })
                }
                Some(Err(hex)) => Some(ParsedEventData::Raw { hex }),
            }
        }

        EventType::BadgeAwarded | EventType::BadgeRemoved => {
            // BadgeAwarded/Removed(address user, string badge, uint256 timestamp)
            let user = indexed_params.first().cloned().unwrap_or_default();
            if data.is_empty() {
                Some(ParsedEventData::BadgeAwardedData {
                    user,
                    badge: String::new(),
                    timestamp: String::new(),
                })
            } else {
                decode_tuple(
                    vec![DynSolType::String, DynSolType::Uint(256)],
                    data,
                    |vals| {
                        let badge = decode_str(vals, 0).unwrap_or_default();
                        let timestamp = decode_uint(vals, 1).unwrap_or_default();
                        if matches!(event_type, EventType::BadgeAwarded) {
                            ParsedEventData::BadgeAwardedData { user, badge, timestamp }
                        } else {
                            ParsedEventData::BadgeRemovedData { user, badge, timestamp }
                        }
                    },
                )
            }
        }

        EventType::CollabProposed => {
            // CollabProposed(uint256 tokenId, address proposer, address recipient, uint256 timestamp)
            let token_id = indexed_params.first().cloned().unwrap_or_default();
            if data.is_empty() {
                Some(ParsedEventData::CollabProposedData {
                    token_id,
                    proposer: String::new(),
                    recipient: String::new(),
                    timestamp: String::new(),
                })
            } else {
                decode_tuple(
                    vec![DynSolType::Address, DynSolType::Address, DynSolType::Uint(256)],
                    data,
                    |vals| ParsedEventData::CollabProposedData {
                        token_id,
                        proposer:  decode_addr_token(vals, 0).unwrap_or_default(),
                        recipient: decode_addr_token(vals, 1).unwrap_or_default(),
                        timestamp: decode_uint(vals, 2).unwrap_or_default(),
                    },
                )
            }
        }

        EventType::UsernameTransferred => {
            // UsernameTransferred(address from, address to, string username, uint256 timestamp)
            let from = indexed_params.first().cloned().unwrap_or_default();
            let to = indexed_params.get(1).cloned().unwrap_or_default();
            match decode_str_uint256(data) {
                None => Some(ParsedEventData::UsernameTransferredData {
                    from,
                    to,
                    username: String::new(),
                    timestamp: String::new(),
                }),
                Some(Ok((username, timestamp))) => {
                    Some(ParsedEventData::UsernameTransferredData { from, to, username, timestamp })
                }
                Some(Err(hex)) => Some(ParsedEventData::Raw { hex }),
            }
        }

        EventType::Unfollowed
        | EventType::NotificationEvent
        | EventType::UserUnverified
        | EventType::Unknown => None,

        EventType::SnapCommented
        | EventType::ArtCommented
        | EventType::MusicCommented
        | EventType::FlixCommented
        | EventType::SnapBoughtAndMinted
        | EventType::ArtBoughtAndMinted
        | EventType::MusicBoughtAndMinted
        | EventType::FlixBoughtAndMinted
        | EventType::SnapDeleted
        | EventType::ArtDeleted
        | EventType::MusicDeleted
        | EventType::FlixDeleted => {
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
