//! Event Signatures and Parsing
//!
//! This module provides Ethereum event signature definitions and parsing utilities
//! for TheraGraph smart contracts. It ensures compatibility between Rust and Elixir
//! by using identical event type names and data structures.
//!
//! ## Module layout
//!
//! | sub-module   | contents |
//! |-------------|----------|
//! | `event_type` | EventType enum + all classification methods + Display |
//! | `parsed`     | ParsedEvent + ParsedEventData output structs |
//! | `abi`        | Low-level ABI decode helpers (abi_tuple, decode_uint, …) |
//! | `indexed`    | EVM topic-slot type dispatch (IndexedParamType, extract_indexed_params) |
//! | `data`       | parse_event_data — 50-arm ABI decode per EventType |
//! | `tests`      | Integration tests for all event parse paths |

mod abi;
mod data;
mod event_type;
mod indexed;
mod parsed;

pub use event_type::EventType;
pub use parsed::{ParsedEvent, ParsedEventData};

use crate::error::Result;
use alloy::primitives::{keccak256, B256};
use alloy::rpc::types::Log;
use std::sync::LazyLock as Lazy;
use std::collections::HashMap;

// ============================================================================
// Event Signatures (keccak256 hashes)
// ============================================================================

/// Pre-computed event signatures for TheraGraph contracts.
/// These match the signatures in `TheraGraph.Indexer.EventParser` on the Elixir side.
pub static EVENT_SIGNATURES: Lazy<HashMap<B256, EventType>> = Lazy::new(|| {
    let mut m = HashMap::new();

    // === TheraSnap Events ===
    m.insert(keccak256_signature("SnapMinted(uint256,string,address)"), EventType::SnapMinted);
    m.insert(keccak256_signature("SnapLiked(uint256,address,uint256)"), EventType::SnapLiked);
    m.insert(keccak256_signature("SnapCommented(uint256,uint256,address,string)"), EventType::SnapCommented);
    m.insert(keccak256_signature("SnapBoughtAndMinted(uint256,address,address,uint256,uint256)"), EventType::SnapBoughtAndMinted);
    m.insert(keccak256_signature("SnapDeleted(uint256,address)"), EventType::SnapDeleted);

    // === TheraArt Events ===
    m.insert(keccak256_signature("ArtMinted(uint256,string,address)"), EventType::ArtMinted);
    m.insert(keccak256_signature("ArtLiked(uint256,address,uint256)"), EventType::ArtLiked);
    m.insert(keccak256_signature("ArtCommented(uint256,uint256,address,string)"), EventType::ArtCommented);
    m.insert(keccak256_signature("ArtBoughtAndMinted(uint256,address,address,uint256,uint256)"), EventType::ArtBoughtAndMinted);
    m.insert(keccak256_signature("ArtDeleted(uint256,address)"), EventType::ArtDeleted);

    // === TheraMusic Events ===
    m.insert(keccak256_signature("MusicMinted(uint256,string,address)"), EventType::MusicMinted);
    m.insert(keccak256_signature("MusicLiked(uint256,address,uint256)"), EventType::MusicLiked);
    m.insert(keccak256_signature("MusicCommented(uint256,uint256,address,string)"), EventType::MusicCommented);
    m.insert(keccak256_signature("MusicBoughtAndMinted(uint256,address,address,uint256,uint256)"), EventType::MusicBoughtAndMinted);
    m.insert(keccak256_signature("MusicDeleted(uint256,address)"), EventType::MusicDeleted);

    // === TheraFlix Events ===
    m.insert(keccak256_signature("FlixMinted(uint256,string,address)"), EventType::FlixMinted);
    m.insert(keccak256_signature("FlixLiked(uint256,address,uint256)"), EventType::FlixLiked);
    m.insert(keccak256_signature("FlixCommented(uint256,uint256,address,string)"), EventType::FlixCommented);
    m.insert(keccak256_signature("FlixBoughtAndMinted(uint256,address,address,uint256,uint256)"), EventType::FlixBoughtAndMinted);
    m.insert(keccak256_signature("FlixDeleted(uint256,address)"), EventType::FlixDeleted);

    // === TheraFriendz Events ===
    m.insert(keccak256_signature("Followed(address,address,string,string,uint256)"), EventType::Followed);
    m.insert(keccak256_signature("Unfollowed(address,address,string,string)"), EventType::Unfollowed);
    m.insert(keccak256_signature("UsernameRegistered(address,string)"), EventType::UsernameRegistered);
    m.insert(keccak256_signature("UsernameTransferred(address,address,string,uint256)"), EventType::UsernameTransferred);
    m.insert(keccak256_signature("ProfileUpdated(address,string,string,string,string)"), EventType::ProfileUpdated);
    m.insert(
        keccak256_signature("NotificationEvent(address,address,uint8,uint256,string,string,bytes32,string)"),
        EventType::NotificationEvent,
    );
    m.insert(keccak256_signature("EarningsWithdrawn(address,uint256)"), EventType::EarningsWithdrawn);

    m.insert(keccak256_signature("UserVerified(address,string)"), EventType::UserVerified);
    m.insert(keccak256_signature("UserUnverified(address,string)"), EventType::UserUnverified);
    m.insert(keccak256_signature("UserBlocked(address,address)"), EventType::UserBlocked);
    m.insert(keccak256_signature("UserUnblocked(address,address)"), EventType::UserUnblocked);

    // === Common Events ===
    m.insert(keccak256_signature("Transfer(address,address,uint256)"), EventType::Transfer);
    m.insert(keccak256_signature("PurchaseProcessed(uint256,address,uint256)"), EventType::PurchaseProcessed);
    m.insert(keccak256_signature("RoyaltyDistributed(uint256,address,uint256)"), EventType::RoyaltyDistributed);

    // Unified TheraFriendz content & social events (new contract)
    m.insert(b256_from_hex("0xe913bf0f321ec4538e6e03894963538ad29d5bc7610699f655b8d4be77ef3c31"), EventType::ContentMinted);
    m.insert(b256_from_hex("0x80c2e061ec45ed7331a60555bbadc701bd26c6335bcd10063bc2fe287d040f2f"), EventType::ContentCopyMinted);
    m.insert(keccak256_signature("ContentCopyMintedViaReferral(uint256,address,uint256,address,uint256,uint256)"), EventType::ContentCopyMintedViaReferral);
    m.insert(b256_from_hex("0x249b4fd7f31ce1b27cdf920c73f8526880c641036b50f96dc78f49d5166f5853"), EventType::ListingUpdated);
    m.insert(b256_from_hex("0x62d3506db24551831d906a4161625343e801105b08beef50f2616a51fd17a7b8"), EventType::ContentBlocked);
    m.insert(b256_from_hex("0x4bbdc3b759094c64d5ae0d8d46654078d43716a6188ae8eb6bc36de1d06994c1"), EventType::ContentBookmarked);
    m.insert(keccak256_signature("ContentShared(uint256,address,address,uint256)"), EventType::ContentShared);
    m.insert(b256_from_hex("0x528a31b859c72723f16bde373bc45e6e13a4d24d709e07200855baccec618cff"), EventType::ContentBurned);
    m.insert(b256_from_hex("0x0a09fa67e91ea818e53d712f63caf32f685bed0c54acdb1cebf8f63a36b454aa"), EventType::UsernameRegistered);
    m.insert(keccak256_signature("UsernameTransferred(address,address,string,uint256)"), EventType::UsernameTransferred);
    m.insert(b256_from_hex("0xdcb94c0b2c025b0736b4b62b1c595f2ca7ad4c711eada6026d477e87de9cca08"), EventType::ProfileUpdated);
    m.insert(b256_from_hex("0xb493045fc13318793ba6deaf400d8f23236835ab7c056d18196896cf98fbd9d9"), EventType::ProfileUpdatedExtended);
    m.insert(b256_from_hex("0x22b3126528cda4618d13b6945f5e96fe53a5125f386aa591ee89134e2681c621"), EventType::UserVerified);
    m.insert(b256_from_hex("0x4906653113399be7fcd9c1ea679e52a58c1efeb96169aaa8b1fd94339ce12b57"), EventType::UserBlocked);
    m.insert(b256_from_hex("0xe3698e4763ee4becca0f71e44047f2c0018e133a8c70ab056c2ad3641fefd54a"), EventType::RoyaltyDistributed);
    m.insert(b256_from_hex("0x90dac969af4a4897610ef8f0cd934c54409861eb7bd2205e552f8f2296ee5d3e"), EventType::EarningsWithdrawn);
    m.insert(b256_from_hex("0xc83ca0840994260dfd9b90ce0f552ac8a0424cae524b6dee6b476a78f6fbdc30"), EventType::BurnedContentRevenue);
    m.insert(b256_from_hex("0x08031759b0a2a99f63000784e546d7320d30692b97de1ea89a1645380cfb16f8"), EventType::TreasuryUpdated);
    m.insert(b256_from_hex("0x382768820017a6e69506da8e35e39b17315306885e94830a6b4d97aa3e3587ff"), EventType::TokensRecovered);
    m.insert(keccak256_signature("TipSent(address,address,uint256,uint256)"), EventType::TipSent);
    m.insert(keccak256_signature("CollabProposed(uint256,address,address,uint256)"), EventType::CollabProposed);
    m.insert(keccak256_signature("BadgeAwarded(address,string,uint256)"), EventType::BadgeAwarded);
    m.insert(keccak256_signature("BadgeRemoved(address,string,uint256)"), EventType::BadgeRemoved);
    m.insert(b256_from_hex("0x5ad51d3c935f3f73f5b949c8702f6798df012b7f415aa33e82a6c81c6c9ff6a0"), EventType::PlatformFeeUpdated);

    m
});

/// Compute keccak256 of an event signature string → B256 topic key.
fn keccak256_signature(sig: &str) -> B256 {
    keccak256(sig.as_bytes())
}

fn b256_from_hex(s: &str) -> B256 {
    s.parse::<B256>().unwrap_or(B256::ZERO)
}

// ============================================================================
// Event Parser
// ============================================================================

/// Parse a raw Ethereum log into a structured event.
///
/// # Arguments
/// * `log` - The raw Ethereum log from the blockchain
/// * `fallback_contract_type` - Contract type to use if event signature is unknown
pub fn parse_log(log: &Log, fallback_contract_type: &str) -> Result<ParsedEvent> {
    let topics = log.topics();

    let event_type = if topics.is_empty() {
        EventType::Unknown
    } else {
        EVENT_SIGNATURES
            .get(&topics[0])
            .copied()
            .unwrap_or(EventType::Unknown)
    };

    let indexed_params: Vec<String> = indexed::extract_indexed_params(&event_type, topics);

    let mut contract_type = if event_type != EventType::Unknown {
        event_type.contract_type().to_string()
    } else {
        fallback_contract_type.to_string()
    };

    // ContentMinted includes ContentType as the 3rd indexed param
    if matches!(event_type, EventType::ContentMinted) && indexed_params.len() >= 3 {
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

    let log_data = log.data();
    let data = data::parse_event_data(&event_type, &indexed_params, &log_data.data);

    let block_number = log.block_number.unwrap_or(0);
    let tx_hash = log
        .transaction_hash
        .map(|h| format!("{h:#x}"))
        .unwrap_or_default();
    let log_index = log.log_index.unwrap_or(0);

    Ok(ParsedEvent {
        kafka_topic: event_type.kafka_topic(),
        event_type: event_type.to_string(),
        contract_address: format!("{:?}", log.address()),
        contract_type,
        block_number,
        transaction_hash: tx_hash,
        log_index,
        timestamp: chrono::Utc::now().timestamp(),
        indexed_params,
        data,
        raw_data: if log_data.data.is_empty() {
            None
        } else {
            Some(format!("0x{}", hex::encode(&log_data.data)))
        },
    })
}

/// Get Kafka key for an event (used for partitioning).
pub fn event_kafka_key(event: &ParsedEvent) -> String {
    format!("{}.{}", event.contract_type, event.contract_address)
}

#[cfg(test)]
mod tests;
