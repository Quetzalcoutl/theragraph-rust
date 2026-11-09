//! TheraFriendzV2 (v10) contract indexer
//!
//! Watches the v10 `TheraFriendzV2` contract (`THERA_FRIENDZ_V2_ADDRESS`) —
//! the contract therafriendz actually mints every content type against since
//! the v10 cutover (see `therafriendz/tokens.ts`). Until this indexer existed,
//! nothing watched it: `thera_social.rs`/`thera_friendz.rs` only ever watched
//! the v1 `TheraFriendz` contract (`THERA_FRIENDZ_ADDRESS`), so every mint on
//! v10 was invisible to the Postgres-backed read path (museum, profile,
//! feeds, discovery — anything that isn't reading the chain directly).
//!
//! This runs **alongside** `thera_social`/`thera_friendz` (v1), on its own
//! contract address and its own `indexer_state` cursor (`"friends_v2"`) —
//! additive, never a replacement. Old v1 content keeps working unchanged.
//!
//! ## Why this isn't just `GenericIndexer` with a new address
//!
//! v10's event ABI (`therawallet/contracts/TheraFriendzV2.sol`, canonicalized
//! in `thera-core/crates/thera-types/src/events.rs`) is a complete rewrite,
//! not a v1 lookalike:
//!
//! | v1 event (legacy `TheraFriendz`)                                   | v10 equivalent (`TheraFriendzV2`)                                   |
//! |---------------------------------------------------------------------|----------------------------------------------------------------------|
//! | `ContentMinted(tokenId, creator, contentType, price, timestamp)`    | `ListingCreated(originalId, creator, price, maxCopies, royalties…, uriHash)` **+** `ContentMetadata(originalId, contentType, uri)` — two separate logs, always emitted together in the same tx (see `_redeem` in the Solidity source) |
//! | `ContentCopyMinted(originalId, buyer, newTokenId, contentType, ts)` | `CopyPurchased(originalId, copyId, buyer, recipient, grossPrice, platformFee, curatorFee, referrer, creatorPool)` — no `contentType`; distinguishes payer (`buyer`) from who actually receives the copy (`recipient`) |
//! | `ListingUpdated(originalTokenId, price, maxCopies, timestamp)`      | `ListingUpdated(originalId, oldPrice, newPrice)` — `maxCopies` is immutable after listing, so it's never in this event |
//! | `ContentBlocked(tokenId, blockedBy, contentType, reason)`           | `ContentModerated(originalId, blocked)` — no moderator address, no reason, and genuinely bidirectional (`blocked` can go back to `false`) |
//! | `UserBlocked(user, status, timestamp)`                              | `UserModerated(user, blocked)` — same shape, no timestamp |
//!
//! `parse_logs` below decodes v10's real ABI directly (its own local topic0
//! table — it does NOT touch the shared `events::EVENT_SIGNATURES` map that
//! v1 and every other indexer rely on) and re-emits the result under the
//! **same** `event_type` strings Elixir's `TheraGraph.Indexer.EventHandlers`
//! dispatch table already knows ("ContentMinted", "ContentCopyMinted",
//! "ListingUpdated", "ContentBlocked", "UserBlocked") — so no Elixir changes
//! are needed for this to be understood downstream.
//!
//! ## Deliberately not forwarded (see inline comments at each decode site)
//!
//! - **Post-mint `ListingUpdated(originalId, oldPrice, newPrice)`** (a
//!   creator's later price-only repricing): v10's event carries no
//!   `maxCopies`, and `ContentMutations.update_listing/4` on the Elixir side
//!   unconditionally overwrites `max_copies` with whatever it's given. Without
//!   a persisted originalId→maxCopies cache (or an on-chain `listing()`
//!   lookup) forwarding this would silently zero out `max_copies` on every
//!   price change. The **initial** listing (mint time) is still forwarded —
//!   see `build_mint_time_listing_updated` — because `ListingCreated` carries
//!   the real `maxCopies` directly.
//! - **`ContentModerated(originalId, blocked = false)`** (an unblock):
//!   Elixir's `handle_content_blocked`/`ContentMutations.block_content` only
//!   knows how to set `is_blocked: true`; there's no existing unblock path to
//!   map onto safely. Forwarding `false` through the same handler would
//!   incorrectly re-block already-unblocked content, so it's dropped.
//!
//! ## Known-incomplete fields (safe placeholders, not silently wrong)
//!
//! - `ContentBlocked`'s `moderator`/`reason`: v10's `ContentModerated` carries
//!   neither. `ContentMutations.block_content/4` ignores its `moderator` arg
//!   and treats `reason` as optional, so a zero-address placeholder and an
//!   empty reason are safe no-ops, not data corruption.
//! - `ContentCopyMinted`'s `content_type`: v10's `CopyPurchased` doesn't carry
//!   it (unlike v1's `ContentCopyMinted`). Resolving it would require a DB
//!   lookup of the original's stored type from this stateless chain watcher —
//!   left empty; Elixir's `content_type_to_atom/1` falls back to `:snap` with
//!   a warning, same degraded-but-non-fatal path v1 already exercises on any
//!   unrecognised value.
//! - Every event here has no on-chain timestamp field (v10 dropped the
//!   trailing `uint256 timestamp` v1's unified events carried) — `timestamp`
//!   fields are sent empty, matching the same "insufficient data" fallback
//!   `events::data::parse_event_data` already produces for v1 in degenerate
//!   cases.

use crate::error::{Error, Result};
use crate::events::{ParsedEvent, ParsedEventData};
use crate::indexer::{get_last_indexed_block, parse_address, ContractType, GenericIndexer};
use crate::AppState;
use alloy::dyn_abi::{DynSolType, DynSolValue};
use alloy::network::Ethereum;
use alloy::primitives::{keccak256, B256, U256};
use alloy::providers::RootProvider;
use alloy::rpc::types::Log;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::LazyLock as Lazy;
use std::time::Duration;
use tracing::warn;

/// `ContractType` adapter for the v10 TheraFriendzV2 contract.
pub struct TheraFriendzV2Contract;

impl ContractType for TheraFriendzV2Contract {
    fn cursor_type_name(&self) -> &'static str { "friends_v2" }
    fn event_parse_type(&self) -> &'static str { "friends" }
    fn display_name(&self) -> &'static str { "TheraFriendzV2Indexer" }

    fn parse_logs(&self, logs: &[Log]) -> Vec<Result<ParsedEvent>> {
        parse_v2_logs(logs)
    }
}

pub async fn run_with_state(state: Arc<AppState>) -> Result<()> {
    let contract_address = parse_address(state.config.contracts.thera_friendz_v2.as_str())?;
    let provider = RootProvider::<Ethereum>::new_http(
        state.config.blockchain.rpc_url.parse().map_err(|e| Error::blockchain(format!("Invalid RPC URL: {e}")))?,
    );

    let start_block = get_last_indexed_block(
        state.indexer_db.pool(),
        &format!("{:?}", contract_address),
        "friends_v2",
    )
    .await?
    // DB stores last-processed block; +1 so we start from the next unprocessed block.
    .map(|b| b + 1)
    .unwrap_or(state.config.blockchain.start_block);

    let mut indexer = GenericIndexer {
        contract: TheraFriendzV2Contract,
        provider: Arc::new(provider),
        contract_address,
        kafka: state.kafka.clone(),
        pool: state.indexer_db.pool().clone(),
        poll_interval: state.config.blockchain.poll_interval,
        batch_size: state.config.blockchain.batch_size,
        current_block: start_block,
        request_delay: Duration::from_millis(state.config.blockchain.request_delay_ms),
        max_retries: state.config.blockchain.max_retries,
        retry_delay: state.config.blockchain.retry_delay,
        direct: state.direct_handlers.clone(),
    };

    let mut shutdown_rx = state.shutdown.subscribe();
    indexer.run(&mut shutdown_rx).await
}

// ============================================================================
// topic0 table (local to this module — does not touch events::EVENT_SIGNATURES)
// ============================================================================

struct V2Topics {
    listing_created: B256,
    content_metadata: B256,
    copy_purchased: B256,
    listing_updated: B256,
    content_moderated: B256,
    user_moderated: B256,
}

static V2_TOPICS: Lazy<V2Topics> = Lazy::new(|| V2Topics {
    listing_created: topic0("ListingCreated(uint256,address,uint256,uint32,address[],uint16[],bool,bytes32)"),
    content_metadata: topic0("ContentMetadata(uint256,uint8,string)"),
    copy_purchased: topic0("CopyPurchased(uint256,uint256,address,address,uint256,uint256,uint256,address,uint256)"),
    listing_updated: topic0("ListingUpdated(uint256,uint256,uint256)"),
    content_moderated: topic0("ContentModerated(uint256,bool)"),
    user_moderated: topic0("UserModerated(address,bool)"),
});

fn topic0(signature: &str) -> B256 {
    keccak256(signature.as_bytes())
}

// ============================================================================
// Batch-level correlation
// ============================================================================

/// `ListingCreated` data held until its paired `ContentMetadata` (same tx,
/// same originalId) arrives — always later in the same transaction per
/// `TheraFriendzV2Contract::_redeem`, and therefore always in the same
/// `get_logs` batch (a transaction's logs can't span a block-range boundary).
struct PendingListing {
    creator: String,
    price: String,
    max_copies: String,
    block_number: u64,
    log_index: u64,
    transaction_hash: String,
    contract_address: String,
}

/// Turn one fetched batch of v10 raw logs into outgoing `ParsedEvent`s.
///
/// Logs matching none of the six known v10 topics (the standard ERC-721
/// `Transfer`, OpenZeppelin `Approval`/`RoleGranted`/`Paused`/…) fall back to
/// the shared generic `events::parse_log`, exactly like every other indexer —
/// `Transfer` decodes normally; the rest classify as `EventType::Unknown` and
/// are forwarded harmlessly, same as today's v1 behavior for anything outside
/// its own known set.
fn parse_v2_logs(logs: &[Log]) -> Vec<Result<ParsedEvent>> {
    let mut out = Vec::with_capacity(logs.len());
    let mut pending_mints: HashMap<(String, String), PendingListing> = HashMap::new();

    for log in logs {
        let topics = log.topics();
        if topics.is_empty() {
            continue;
        }
        let topic0 = topics[0];

        if topic0 == V2_TOPICS.listing_created {
            match decode_listing_created(log) {
                Some((original_id, creator, price, max_copies)) => {
                    let key = (tx_hash_of(log), original_id);
                    pending_mints.insert(
                        key,
                        PendingListing {
                            creator,
                            price,
                            max_copies,
                            block_number: log.block_number.unwrap_or(0),
                            log_index: log.log_index.unwrap_or(0),
                            transaction_hash: tx_hash_of(log),
                            contract_address: format!("{:?}", log.address()),
                        },
                    );
                }
                None => warn!(
                    "TheraFriendzV2Indexer: failed to decode ListingCreated in tx {}",
                    tx_hash_of(log)
                ),
            }
            continue;
        }

        if topic0 == V2_TOPICS.content_metadata {
            match decode_content_metadata(log) {
                Some((original_id, content_type)) => {
                    let key = (tx_hash_of(log), original_id.clone());
                    match pending_mints.remove(&key) {
                        Some(listing) => {
                            out.push(Ok(build_content_minted(&original_id, &listing, &content_type)));
                            out.push(Ok(build_mint_time_listing_updated(&original_id, &listing)));
                        }
                        None => warn!(
                            "TheraFriendzV2Indexer: ContentMetadata for original #{} (tx {}) had no matching \
                             ListingCreated in this batch — contract always emits them together, so this \
                             indicates a decode bug or an unexpected reorg; dropping the mint event",
                            original_id,
                            tx_hash_of(log)
                        ),
                    }
                }
                None => warn!(
                    "TheraFriendzV2Indexer: failed to decode ContentMetadata in tx {}",
                    tx_hash_of(log)
                ),
            }
            continue;
        }

        if topic0 == V2_TOPICS.copy_purchased {
            match decode_copy_purchased(log) {
                Some(event) => out.push(Ok(event)),
                None => warn!(
                    "TheraFriendzV2Indexer: failed to decode CopyPurchased in tx {}",
                    tx_hash_of(log)
                ),
            }
            continue;
        }

        if topic0 == V2_TOPICS.listing_updated {
            // Post-mint repricing — deliberately not forwarded. See module doc.
            continue;
        }

        if topic0 == V2_TOPICS.content_moderated {
            // `None` here covers both a decode failure and a deliberate skip
            // (blocked == false); decode_content_moderated logs the former.
            if let Some(event) = decode_content_moderated(log) {
                out.push(Ok(event));
            }
            continue;
        }

        if topic0 == V2_TOPICS.user_moderated {
            match decode_user_moderated(log) {
                Some(event) => out.push(Ok(event)),
                None => warn!(
                    "TheraFriendzV2Indexer: failed to decode UserModerated in tx {}",
                    tx_hash_of(log)
                ),
            }
            continue;
        }

        out.push(crate::events::parse_log(log, "friends"));
    }

    for (original_id, _) in pending_mints.keys() {
        warn!(
            "TheraFriendzV2Indexer: ListingCreated for original #{} had no matching ContentMetadata \
             anywhere in this batch — mint event dropped",
            original_id
        );
    }

    out
}

fn tx_hash_of(log: &Log) -> String {
    log.transaction_hash.map(|h| format!("{h:#x}")).unwrap_or_default()
}

/// v1's `content_type` indexed param is a plain decimal string ("0".."3");
/// mirrored here (not cast to a native int) so `ParsedEventData::MintedV2`
/// round-trips identically to v1's existing representation.
fn content_type_contract_bucket(content_type: &str) -> &'static str {
    match content_type {
        "0" => "art",
        "1" => "vid",
        "2" => "music",
        "3" => "snap",
        other => {
            warn!("TheraFriendzV2Indexer: unknown contentType={other}, defaulting contract_type to 'friends'");
            "friends"
        }
    }
}

fn dyn_uint_str(value: &DynSolValue) -> Option<String> {
    if let DynSolValue::Uint(u, _) = value { Some(u.to_string()) } else { None }
}

fn dyn_addr_hex(value: &DynSolValue) -> Option<String> {
    if let DynSolValue::Address(a) = value { Some(format!("{a:#x}")) } else { None }
}

// ============================================================================
// Per-event decoders
// ============================================================================

/// `ListingCreated(uint256 indexed originalId, address indexed creator, uint256 price,
///  uint32 maxCopies, address[] royaltyRecipients, uint16[] royaltyBps, bool resharingEnabled, bytes32 uriHash)`
///
/// Only `price` and `maxCopies` are needed downstream today; royalties/
/// resharing/uriHash are decoded (required to walk the ABI correctly, since
/// they're part of the same non-indexed tuple) and then discarded.
fn decode_listing_created(log: &Log) -> Option<(String, String, String, String)> {
    let topics = log.topics();
    if topics.len() < 3 {
        return None;
    }
    let original_id = U256::from_be_slice(topics[1].as_ref()).to_string();
    let creator = format!("0x{}", hex::encode(&topics[2][12..]));

    let data = &log.data().data;
    let ty = DynSolType::Tuple(vec![
        DynSolType::Uint(256),                              // price
        DynSolType::Uint(32),                               // maxCopies
        DynSolType::Array(Box::new(DynSolType::Address)),   // royaltyRecipients
        DynSolType::Array(Box::new(DynSolType::Uint(16))),  // royaltyBps
        DynSolType::Bool,                                   // resharingEnabled
        DynSolType::FixedBytes(32),                         // uriHash
    ]);
    let DynSolValue::Tuple(vals) = ty.abi_decode_params(data).ok()? else {
        return None;
    };
    let price = dyn_uint_str(vals.first()?)?;
    let max_copies = dyn_uint_str(vals.get(1)?)?;

    Some((original_id, creator, price, max_copies))
}

/// `ContentMetadata(uint256 indexed originalId, uint8 contentType, string uri)`
///
/// `uri` is decoded (required to walk the ABI past it correctly) but not
/// forwarded — `handle_content_minted` on the Elixir side re-fetches
/// `tokenURI(tokenId)` itself via RPC regardless of what this field carries,
/// matching v1's existing behavior (v1 always sends `uri: ""` too).
fn decode_content_metadata(log: &Log) -> Option<(String, String)> {
    let topics = log.topics();
    if topics.len() < 2 {
        return None;
    }
    let original_id = U256::from_be_slice(topics[1].as_ref()).to_string();

    let data = &log.data().data;
    let ty = DynSolType::Tuple(vec![DynSolType::Uint(8), DynSolType::String]);
    let DynSolValue::Tuple(vals) = ty.abi_decode_params(data).ok()? else {
        return None;
    };
    let content_type = dyn_uint_str(vals.first()?)?;

    Some((original_id, content_type))
}

fn build_content_minted(original_id: &str, listing: &PendingListing, content_type: &str) -> ParsedEvent {
    let contract_type = content_type_contract_bucket(content_type).to_string();

    ParsedEvent {
        kafka_topic: "blockchain.events",
        event_type: "ContentMinted".to_string(),
        contract_address: listing.contract_address.clone(),
        contract_type,
        block_number: listing.block_number,
        transaction_hash: listing.transaction_hash.clone(),
        log_index: listing.log_index,
        timestamp: chrono::Utc::now().timestamp(),
        indexed_params: vec![original_id.to_string(), listing.creator.clone(), content_type.to_string()],
        data: Some(ParsedEventData::MintedV2 {
            token_id: original_id.to_string(),
            uri: String::new(),
            creator: listing.creator.clone(),
            content_type: content_type.to_string(),
            price: listing.price.clone(),
            timestamp: String::new(),
            contract_address: listing.contract_address.clone(),
            block_number: listing.block_number,
            transaction_hash: listing.transaction_hash.clone(),
        }),
        raw_data: None,
    }
}

/// Mirrors v1's documented behavior ("`ListingUpdated` fires on a voucher's
/// first redemption AND on every later `updateListing` call") so `max_copies`
/// gets set in Postgres at mint time, since v10's `ListingCreated` is the only
/// place `maxCopies` ever appears on-chain.
fn build_mint_time_listing_updated(original_id: &str, listing: &PendingListing) -> ParsedEvent {
    ParsedEvent {
        kafka_topic: "blockchain.events",
        event_type: "ListingUpdated".to_string(),
        contract_address: listing.contract_address.clone(),
        contract_type: "friends".to_string(),
        block_number: listing.block_number,
        transaction_hash: listing.transaction_hash.clone(),
        log_index: listing.log_index,
        timestamp: chrono::Utc::now().timestamp(),
        indexed_params: vec![original_id.to_string()],
        data: Some(ParsedEventData::ListingUpdatedV2 {
            original_token_id: original_id.to_string(),
            price: listing.price.clone(),
            max_copies: listing.max_copies.clone(),
            timestamp: String::new(),
            contract_address: listing.contract_address.clone(),
        }),
        raw_data: None,
    }
}

/// `CopyPurchased(uint256 indexed originalId, uint256 indexed copyId, address indexed buyer,
///  address recipient, uint256 grossPrice, uint256 platformFee, uint256 curatorFee,
///  address referrer, uint256 creatorPool)`
///
/// Maps `recipient` (who actually receives/owns the new copy) — not the
/// indexed `buyer` (who paid) — onto the "buyer" field Elixir's
/// `ContentMutations.mint_copy_nft/1` stores as the copy's `owner_address`.
/// They're the same address except for a `buyCopyFor` card/Pix purchase paid
/// from the delivery float on someone else's behalf.
fn decode_copy_purchased(log: &Log) -> Option<ParsedEvent> {
    let topics = log.topics();
    if topics.len() < 4 {
        return None;
    }
    let original_id = U256::from_be_slice(topics[1].as_ref()).to_string();
    let new_token_id = U256::from_be_slice(topics[2].as_ref()).to_string();

    let data = &log.data().data;
    let ty = DynSolType::Tuple(vec![
        DynSolType::Address, // recipient
        DynSolType::Uint(256), // grossPrice
        DynSolType::Uint(256), // platformFee
        DynSolType::Uint(256), // curatorFee
        DynSolType::Address, // referrer
        DynSolType::Uint(256), // creatorPool
    ]);
    let DynSolValue::Tuple(vals) = ty.abi_decode_params(data).ok()? else {
        return None;
    };
    let recipient = dyn_addr_hex(vals.first()?)?;

    let contract_address = format!("{:?}", log.address());
    let block_number = log.block_number.unwrap_or(0);
    let transaction_hash = tx_hash_of(log);
    let log_index = log.log_index.unwrap_or(0);

    Some(ParsedEvent {
        // ContentCopyMinted is a notification-priority event for v1 too
        // (EventType::is_notification_event) — matched here so a v10 copy
        // purchase pushes a notification the same way a v1 one does.
        kafka_topic: "notifications.priority",
        event_type: "ContentCopyMinted".to_string(),
        contract_address: contract_address.clone(),
        contract_type: "friends".to_string(),
        block_number,
        transaction_hash: transaction_hash.clone(),
        log_index,
        timestamp: chrono::Utc::now().timestamp(),
        indexed_params: vec![original_id.clone(), recipient.clone(), new_token_id.clone()],
        data: Some(ParsedEventData::CopyMintedV2 {
            original_id,
            buyer: recipient,
            new_token_id,
            // v10's CopyPurchased carries no content_type (unlike v1's
            // ContentCopyMinted) — see module doc.
            content_type: String::new(),
            timestamp: String::new(),
            contract_address,
            block_number,
            transaction_hash,
        }),
        raw_data: Some(format!("0x{}", hex::encode(&data[..]))),
    })
}

/// `ContentModerated(uint256 indexed originalId, bool blocked)` — returns
/// `None` both on decode failure and when `blocked == false` (an unblock);
/// callers only warn-log the former since the latter is an intentional,
/// documented skip (see module doc).
fn decode_content_moderated(log: &Log) -> Option<ParsedEvent> {
    let topics = log.topics();
    if topics.len() < 2 {
        return None;
    }
    let original_id = U256::from_be_slice(topics[1].as_ref()).to_string();

    let data = &log.data().data;
    let blocked = if data.len() >= 32 {
        U256::from_be_slice(&data[0..32]) != U256::ZERO
    } else {
        warn!(
            "TheraFriendzV2Indexer: ContentModerated for original #{} in tx {} had no data payload \
             — treating as unblock (no-op)",
            original_id,
            tx_hash_of(log)
        );
        false
    };

    if !blocked {
        return None;
    }

    let contract_address = format!("{:?}", log.address());
    Some(ParsedEvent {
        kafka_topic: "blockchain.events",
        event_type: "ContentBlocked".to_string(),
        contract_address: contract_address.clone(),
        contract_type: "friends".to_string(),
        block_number: log.block_number.unwrap_or(0),
        transaction_hash: tx_hash_of(log),
        log_index: log.log_index.unwrap_or(0),
        timestamp: chrono::Utc::now().timestamp(),
        indexed_params: vec![original_id.clone()],
        data: Some(ParsedEventData::ContentBlockedV2 {
            token_id: original_id,
            contract_address,
            // v10's ContentModerated carries neither a moderator address nor a
            // reason string — see module doc for why these placeholders are
            // safe no-ops against ContentMutations.block_content/4.
            moderator: "0x0000000000000000000000000000000000000000".to_string(),
            reason: String::new(),
        }),
        raw_data: Some(format!("0x{}", hex::encode(&data[..]))),
    })
}

/// `UserModerated(address indexed user, bool blocked)` — same shape as the
/// unified v1 `UserBlocked(address indexed user, bool status, uint256
/// timestamp)` (minus the trailing timestamp), so this maps directly onto the
/// existing `ParsedEventData::UserBlockedEvent` variant with no new type
/// needed; `handle_user_blocked` only reads `user`/`status`, both present.
fn decode_user_moderated(log: &Log) -> Option<ParsedEvent> {
    let topics = log.topics();
    if topics.len() < 2 {
        return None;
    }
    let user = format!("0x{}", hex::encode(&topics[1][12..]));

    let data = &log.data().data;
    let status = if data.len() >= 32 {
        U256::from_be_slice(&data[0..32]) != U256::ZERO
    } else {
        true
    };

    Some(ParsedEvent {
        kafka_topic: "user.actions",
        event_type: "UserBlocked".to_string(),
        contract_address: format!("{:?}", log.address()),
        contract_type: "friends".to_string(),
        block_number: log.block_number.unwrap_or(0),
        transaction_hash: tx_hash_of(log),
        log_index: log.log_index.unwrap_or(0),
        timestamp: chrono::Utc::now().timestamp(),
        indexed_params: vec![user.clone()],
        data: Some(ParsedEventData::UserBlockedEvent {
            user,
            status,
            timestamp: String::new(),
        }),
        raw_data: Some(format!("0x{}", hex::encode(&data[..]))),
    })
}

#[cfg(test)]
mod tests;
