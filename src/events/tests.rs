use super::*;
use crate::events::abi::{decode_str_uint256, decode_two_uint256, read_amount_timestamp};
use alloy::dyn_abi::DynSolValue;
use alloy::primitives::{Address, Bytes, U256};

fn test_log(topics: Vec<B256>, data: impl Into<Bytes>) -> Log {
    Log {
        inner: alloy::primitives::Log {
            address: Address::ZERO,
            data: alloy::primitives::LogData::new_unchecked(topics, data.into()),
        },
        ..Default::default()
    }
}

#[test]
fn test_event_signature_lookup() {
    let sig = keccak256_signature("SnapMinted(uint256,string,address)");
    assert_eq!(EVENT_SIGNATURES.get(&sig), Some(&EventType::SnapMinted));
    let sig2 =
        b256_from_hex("0xe913bf0f321ec4538e6e03894963538ad29d5bc7610699f655b8d4be77ef3c31");
    assert_eq!(EVENT_SIGNATURES.get(&sig2), Some(&EventType::ContentMinted));

    let listing_updated_sig =
        b256_from_hex("0x249b4fd7f31ce1b27cdf920c73f8526880c641036b50f96dc78f49d5166f5853");
    assert_eq!(EVENT_SIGNATURES.get(&listing_updated_sig), Some(&EventType::ListingUpdated));
    let platform_fee_updated_sig =
        b256_from_hex("0x5ad51d3c935f3f73f5b949c8702f6798df012b7f415aa33e82a6c81c6c9ff6a0");
    assert_eq!(EVENT_SIGNATURES.get(&platform_fee_updated_sig), Some(&EventType::PlatformFeeUpdated));
}

#[test]
fn test_event_type_contract() {
    assert_eq!(EventType::SnapMinted.contract_type(), "snap");
    assert_eq!(EventType::Followed.contract_type(), "friends");
}

#[test]
fn test_parse_profile_updated_extended() {
    let sig = b256_from_hex("0xb493045fc13318793ba6deaf400d8f23236835ab7c056d18196896cf98fbd9d9");
    let user_topic = b256_from_hex("0x000000000000000000000000aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");

    let tokens = vec![
        DynSolValue::String("alice".to_string()),
        DynSolValue::String("Qmabcdef123".to_string()),
        DynSolValue::String("hello bio".to_string()),
        DynSolValue::String("https://example.com".to_string()),
        DynSolValue::Uint(U256::from(1_700_000_500u64), 256),
    ];
    let encoded = DynSolValue::Tuple(tokens).abi_encode_sequence().unwrap();
    let data = Bytes::from(encoded);

    let log = test_log(vec![sig, user_topic], data.clone());
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
    let sig = b256_from_hex("0xe913bf0f321ec4538e6e03894963538ad29d5bc7610699f655b8d4be77ef3c31");
    let token_topic = b256_from_hex("0x000000000000000000000000000000000000000000000000000000000000002a");
    let creator_topic = b256_from_hex("0x000000000000000000000000aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    let content_type_topic = b256_from_hex("0x0000000000000000000000000000000000000000000000000000000000000003");

    let price = U256::from(100u64);
    let timestamp = U256::from(1_700_000_500u64);
    let mut data_vec = vec![0u8; 64];
    data_vec[0..32].copy_from_slice(&price.to_be_bytes::<32>());
    data_vec[32..64].copy_from_slice(&timestamp.to_be_bytes::<32>());
    let data = Bytes::from(data_vec);

    let log = test_log(vec![sig, token_topic, creator_topic, content_type_topic], data.clone());
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
fn test_parse_platform_fee_updated_event() {
    let sig = b256_from_hex("0x5ad51d3c935f3f73f5b949c8702f6798df012b7f415aa33e82a6c81c6c9ff6a0");

    let encoded = DynSolValue::Tuple(vec![
        DynSolValue::Uint(U256::from(2000u64), 256),
        DynSolValue::Uint(U256::from(1_700_000_500u64), 256),
    ]).abi_encode_sequence().unwrap();
    let data = Bytes::from(encoded);

    // PlatformFeeUpdated(uint64 fee, uint256 timestamp) — neither param indexed.
    let log = test_log(vec![sig], data.clone());
    let parsed = parse_log(&log, "friends").expect("parse failed");
    assert_eq!(parsed.event_type, "PlatformFeeUpdated");
    assert!(parsed.indexed_params.is_empty());
    if let Some(ParsedEventData::PlatformFeeUpdated { fee, timestamp }) = parsed.data {
        assert_eq!(fee, "2000");
        assert_eq!(timestamp, "1700000500");
    } else { panic!("Expected PlatformFeeUpdated data"); }
}

#[test]
fn test_parse_listing_updated_event() {
    let sig = b256_from_hex("0x249b4fd7f31ce1b27cdf920c73f8526880c641036b50f96dc78f49d5166f5853");
    let original_token_id_topic = b256_from_hex("0x000000000000000000000000000000000000000000000000000000000000002a");

    let encoded = DynSolValue::Tuple(vec![
        DynSolValue::Uint(U256::from(500u64), 256),
        DynSolValue::Uint(U256::from(10u64), 256),
        DynSolValue::Uint(U256::from(1_700_000_500u64), 256),
    ]).abi_encode_sequence().unwrap();
    let data = Bytes::from(encoded);

    // ListingUpdated(uint256 indexed originalTokenId, uint256 price, uint256 maxCopies, uint256 timestamp)
    let log = test_log(vec![sig, original_token_id_topic], data.clone());
    let parsed = parse_log(&log, "friends").expect("parse failed");
    assert_eq!(parsed.event_type, "ListingUpdated");
    assert_eq!(parsed.contract_type, "friends");
    assert_eq!(parsed.indexed_params, vec!["42".to_string()]);
    if let Some(ParsedEventData::ListingUpdated { original_token_id, price, max_copies, timestamp }) = parsed.data {
        assert_eq!(original_token_id, "42");
        assert_eq!(price, "500");
        assert_eq!(max_copies, "10");
        assert_eq!(timestamp, "1700000500");
    } else { panic!("Expected ListingUpdated data"); }
}

#[test]
fn test_parse_treasury_updated_event() {
    let sig = keccak256_signature("TreasuryUpdated(address,address,uint256)");
    let old_topic = b256_from_hex("0x000000000000000000000000aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    let new_topic = b256_from_hex("0x000000000000000000000000bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
    let timestamp = U256::from(1_700_000_500u64);
    let mut data_vec = vec![0u8; 32];
    data_vec[0..32].copy_from_slice(&timestamp.to_be_bytes::<32>());
    let data = Bytes::from(data_vec);

    let log = test_log(vec![sig, old_topic, new_topic], data.clone());
    let parsed = parse_log(&log, "friends").expect("parse failed");
    assert_eq!(parsed.event_type, "TreasuryUpdated");
    if let Some(ParsedEventData::TreasuryUpdated { old_treasury, new_treasury, timestamp }) = parsed.data {
        assert_eq!(old_treasury, "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        assert_eq!(new_treasury, "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        assert_eq!(timestamp, "1700000500");
    } else { panic!("Expected TreasuryUpdated data"); }
}

#[test]
fn test_parse_burned_content_revenue_event() {
    let sig = keccak256_signature("BurnedContentRevenue(uint256,uint256,uint256)");
    let token_topic = b256_from_hex("0x000000000000000000000000000000000000000000000000000000000000002a");
    let amount = U256::from(77u64);
    let timestamp = U256::from(1_700_000_500u64);
    let mut data_vec = vec![0u8; 64];
    data_vec[0..32].copy_from_slice(&amount.to_be_bytes::<32>());
    data_vec[32..64].copy_from_slice(&timestamp.to_be_bytes::<32>());
    let data = Bytes::from(data_vec);

    let log = test_log(vec![sig, token_topic], data.clone());
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
    let sig = keccak256_signature("PurchaseProcessed(uint256,address,uint256)");
    let token_topic = b256_from_hex("0x000000000000000000000000000000000000000000000000000000000000002a");
    let buyer_topic = b256_from_hex("0x000000000000000000000000bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
    let amount = U256::from(123u64);
    let mut data_vec = vec![0u8; 32];
    data_vec[0..32].copy_from_slice(&amount.to_be_bytes::<32>());
    let data = Bytes::from(data_vec);

    let log = test_log(vec![sig, token_topic, buyer_topic], data.clone());
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
    let sig = keccak256_signature("RoyaltyDistributed(uint256,address,uint256,uint256)");
    let token_topic = b256_from_hex("0x000000000000000000000000000000000000000000000000000000000000002a");
    let recipient_topic = b256_from_hex("0x000000000000000000000000bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
    let amount = U256::from(50u64);
    let timestamp = U256::from(1_700_000_500u64);
    let mut data_vec = vec![0u8; 64];
    data_vec[0..32].copy_from_slice(&amount.to_be_bytes::<32>());
    data_vec[32..64].copy_from_slice(&timestamp.to_be_bytes::<32>());
    let data = Bytes::from(data_vec);

    let log = test_log(vec![sig, token_topic, recipient_topic], data.clone());
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
    let sig = keccak256_signature("EarningsWithdrawn(address,uint256)");
    let user_topic = b256_from_hex("0x000000000000000000000000bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
    let amount = U256::from(200u64);
    let timestamp = U256::from(1_700_000_500u64);
    let mut data_vec = vec![0u8; 64];
    data_vec[0..32].copy_from_slice(&amount.to_be_bytes::<32>());
    data_vec[32..64].copy_from_slice(&timestamp.to_be_bytes::<32>());
    let data = Bytes::from(data_vec);

    let log = test_log(vec![sig, user_topic], data.clone());
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
    let sig = keccak256_signature("UsernameRegistered(address,string)");
    let user_topic = b256_from_hex("0x000000000000000000000000aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    let tokens = vec![
        DynSolValue::String("alice".to_string()),
        DynSolValue::Uint(U256::from(1_700_000_500u64), 256),
    ];
    let data = Bytes::from(DynSolValue::Tuple(tokens).abi_encode_sequence().unwrap());

    let log = test_log(vec![sig, user_topic], data.clone());
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
    let sig = keccak256_signature("ProfileUpdated(address,string,uint256)");
    let user_topic = b256_from_hex("0x000000000000000000000000aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    let tokens = vec![
        DynSolValue::String("bob".to_string()),
        DynSolValue::Uint(U256::from(1_700_000_500u64), 256),
    ];
    let data = Bytes::from(DynSolValue::Tuple(tokens).abi_encode_sequence().unwrap());

    let log = test_log(vec![sig, user_topic], data.clone());
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
    let sig_v = keccak256_signature("UserVerified(address,uint256)");
    let user_topic = b256_from_hex("0x000000000000000000000000cccccccccccccccccccccccccccccccccccccccc");
    let mut data_vec = vec![0u8; 32];
    data_vec.copy_from_slice(&U256::from(1_700_000_500u64).to_be_bytes::<32>());
    let data_v = Bytes::from(data_vec.clone());

    let log_v = test_log(vec![sig_v, user_topic], data_v.clone());
    let parsed_v = parse_log(&log_v, "friends").expect("parse failed");
    assert_eq!(parsed_v.event_type, "UserVerified");
    if let Some(ParsedEventData::UserVerifiedEvent { user, timestamp }) = parsed_v.data {
        assert_eq!(user, "0xcccccccccccccccccccccccccccccccccccccccc");
        assert_eq!(timestamp, "1700000500");
    } else { panic!("Expected UserVerified data"); }

    let sig_b = keccak256_signature("UserBlocked(address,bool,uint256)");
    let data = DynSolValue::Tuple(vec![
        DynSolValue::Bool(true),
        DynSolValue::Uint(U256::from(1_700_000_500u64), 256),
    ]).abi_encode_sequence().unwrap();
    let data_b = Bytes::from(data);
    let log_b = test_log(vec![sig_b, user_topic], data_b.clone());

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
    let sig_cb = keccak256_signature("ContentBurned(uint256,address,uint256)");
    let token_topic = b256_from_hex("0x000000000000000000000000000000000000000000000000000000000000002a");
    let owner_topic = b256_from_hex("0x000000000000000000000000dddddddddddddddddddddddddddddddddddddddd");
    let timestamp = U256::from(1_700_000_500u64);
    let mut data_vec = vec![0u8; 32];
    data_vec[0..32].copy_from_slice(&timestamp.to_be_bytes::<32>());
    let data = Bytes::from(data_vec.clone());

    let log = test_log(vec![sig_cb, token_topic, owner_topic], data.clone());
    let parsed = parse_log(&log, "friends").expect("parse failed");
    assert_eq!(parsed.event_type, "ContentBurned");
    if let Some(ParsedEventData::ContentBurned { token_id, owner, timestamp }) = parsed.data {
        assert_eq!(token_id, "42");
        assert_eq!(owner, "0xdddddddddddddddddddddddddddddddddddddddd");
        assert_eq!(timestamp, "1700000500");
    } else { panic!("Expected ContentBurned data"); }

    let sig_tr = keccak256_signature("TokensRecovered(address,address,uint256,uint256)");
    let token_topic = b256_from_hex("0x000000000000000000000000eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee");
    let to_topic = b256_from_hex("0x000000000000000000000000ffffffffffffffffffffffffffffffffffffffff");
    let amount = U256::from(500u64);
    let ts = U256::from(1_700_000_500u64);
    let encoded = DynSolValue::Tuple(vec![
        DynSolValue::Uint(amount, 256),
        DynSolValue::Uint(ts, 256),
    ]).abi_encode_sequence().unwrap();
    let data_tr = Bytes::from(encoded);

    let log_tr = test_log(vec![sig_tr, token_topic, to_topic], data_tr.clone());
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
    let sig = keccak256_signature("TipSent(address,address,uint256,uint256)");
    let sender = b256_from_hex("0x0000000000000000000000001111111111111111111111111111111111111111");
    let recipient = b256_from_hex("0x0000000000000000000000002222222222222222222222222222222222222222");
    let encoded = DynSolValue::Tuple(vec![
        DynSolValue::Uint(U256::from(42u64), 256),
        DynSolValue::Uint(U256::from(1_700_000_500u64), 256),
    ]).abi_encode_sequence().unwrap();
    let data = Bytes::from(encoded);
    let log = test_log(vec![sig, sender, recipient], data.clone());
    let parsed = parse_log(&log, "friends").expect("parse failed");
    assert_eq!(parsed.event_type, "TipSent");
    if let Some(ParsedEventData::TipSent { sender, recipient, amount, timestamp }) = parsed.data {
        assert_eq!(sender, "0x1111111111111111111111111111111111111111");
        assert_eq!(recipient, "0x2222222222222222222222222222222222222222");
        assert_eq!(amount, "42");
        assert_eq!(timestamp, "1700000500");
    } else { panic!("Expected TipSent data"); }

    let sig_b = keccak256_signature("BadgeAwarded(address,string,uint256)");
    let user = b256_from_hex("0x0000000000000000000000003333333333333333333333333333333333333333");
    let tokens = vec![
        DynSolValue::String("gold".to_string()),
        DynSolValue::Uint(U256::from(1_700_000_500u64), 256),
    ];
    let data_b = Bytes::from(DynSolValue::Tuple(tokens).abi_encode_sequence().unwrap());
    let log_b = test_log(vec![sig_b, user], data_b.clone());
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
    let sig_collab = keccak256_signature("CollabProposed(uint256,address,address,uint256)");
    let token_topic = b256_from_hex("0x0000000000000000000000000000000000000000000000000000000000000042");
    let proposer = DynSolValue::Address("0x0000000000000000000000000000000000000abc".parse::<Address>().unwrap());
    let recipient = DynSolValue::Address("0x0000000000000000000000000000000000000def".parse::<Address>().unwrap());
    let ts = DynSolValue::Uint(U256::from(1_700_000_500u64), 256);
    let encoded = DynSolValue::Tuple(vec![proposer, recipient, ts]).abi_encode_sequence().unwrap();
    let data = Bytes::from(encoded);
    let log = test_log(vec![sig_collab, token_topic], data.clone());
    let parsed = parse_log(&log, "friends").expect("parse failed");
    assert_eq!(parsed.event_type, "CollabProposed");
    if let Some(ParsedEventData::CollabProposedData { token_id, proposer, recipient, timestamp }) = parsed.data {
        assert_eq!(token_id, "66"); // 0x42 hex = 66 decimal
        assert!(proposer.starts_with("0x"));
        assert!(recipient.starts_with("0x"));
        assert_eq!(timestamp, "1700000500");
    } else { panic!("Expected CollabProposed data"); }

    let sig_ut = keccak256_signature("UsernameTransferred(address,address,string,uint256)");
    let from = b256_from_hex("0x0000000000000000000000004444444444444444444444444444444444444444");
    let to = b256_from_hex("0x0000000000000000000000005555555555555555555555555555555555555555");
    let tokens = vec![
        DynSolValue::String("robert".to_string()),
        DynSolValue::Uint(U256::from(1_700_000_500u64), 256),
    ];
    let data_ut = Bytes::from(DynSolValue::Tuple(tokens).abi_encode_sequence().unwrap());
    let log_ut = test_log(vec![sig_ut, from, to], data_ut.clone());
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

#[test]
fn read_amount_timestamp_happy_path() {
    let mut data_vec = vec![0u8; 64];
    data_vec[0..32].copy_from_slice(&U256::from(10u64).to_be_bytes::<32>());
    data_vec[32..64].copy_from_slice(&U256::from(20u64).to_be_bytes::<32>());
    let data = Bytes::from(data_vec);
    let (amount, timestamp) = read_amount_timestamp(&data);
    assert_eq!(amount, "10");
    assert_eq!(timestamp, "20");
}

#[test]
fn read_amount_timestamp_zero_amount() {
    let mut data_vec = vec![0u8; 64];
    data_vec[0..32].copy_from_slice(&U256::from(0u64).to_be_bytes::<32>());
    data_vec[32..64].copy_from_slice(&U256::from(999u64).to_be_bytes::<32>());
    let data = Bytes::from(data_vec);
    let (amount, timestamp) = read_amount_timestamp(&data);
    assert_eq!(amount, "0");
    assert_eq!(timestamp, "999");
}

#[test]
fn read_amount_timestamp_only_32_bytes() {
    let mut data_vec = vec![0u8; 32];
    data_vec[0..32].copy_from_slice(&U256::from(42u64).to_be_bytes::<32>());
    let data = Bytes::from(data_vec);
    let (amount, timestamp) = read_amount_timestamp(&data);
    assert_eq!(amount, "42");
    assert_eq!(timestamp, "");
}

#[test]
fn read_amount_timestamp_empty_data() {
    let data = Bytes::from(vec![]);
    let (amount, timestamp) = read_amount_timestamp(&data);
    assert_eq!(amount, "");
    assert_eq!(timestamp, "");
}

#[test]
fn decode_two_uint256_happy_path() {
    let encoded = DynSolValue::Tuple(vec![
        DynSolValue::Uint(U256::from(10u64), 256),
        DynSolValue::Uint(U256::from(20u64), 256),
    ]).abi_encode_sequence().unwrap();
    let data = Bytes::from(encoded);
    let result = decode_two_uint256(&data);
    let (a, b) = result.expect("should be Some").expect("should be Ok");
    assert_eq!(a, "10");
    assert_eq!(b, "20");
}

#[test]
fn decode_two_uint256_large_values() {
    let amount = U256::from(1_000_000_000_000_000_000u128);
    let ts = U256::from(1_700_000_500u64);
    let encoded = DynSolValue::Tuple(vec![
        DynSolValue::Uint(amount, 256),
        DynSolValue::Uint(ts, 256),
    ]).abi_encode_sequence().unwrap();
    let data = Bytes::from(encoded);
    let (a, b) = decode_two_uint256(&data).unwrap().unwrap();
    assert_eq!(a, "1000000000000000000");
    assert_eq!(b, "1700000500");
}

#[test]
fn decode_two_uint256_empty_data_returns_none() {
    let data = Bytes::from(vec![]);
    assert!(decode_two_uint256(&data).is_none());
}

#[test]
fn decode_two_uint256_too_short_returns_err() {
    let data = Bytes::from(vec![0u8; 31]);
    let result = decode_two_uint256(&data);
    match result {
        Some(Err(hex_str)) => {
            assert!(hex_str.starts_with("0x"));
        }
        other => panic!("expected Some(Err(hex)), got {:?}", other),
    }
}

#[test]
fn decode_str_uint256_happy_path() {
    let encoded = DynSolValue::Tuple(vec![
        DynSolValue::String("alice".to_string()),
        DynSolValue::Uint(U256::from(1_700_000_500u64), 256),
    ]).abi_encode_sequence().unwrap();
    let data = Bytes::from(encoded);
    let result = decode_str_uint256(&data);
    let (s, u) = result.expect("should be Some").expect("should be Ok");
    assert_eq!(s, "alice");
    assert_eq!(u, "1700000500");
}

#[test]
fn decode_str_uint256_empty_string_portion() {
    let encoded = DynSolValue::Tuple(vec![
        DynSolValue::String(String::new()),
        DynSolValue::Uint(U256::from(42u64), 256),
    ]).abi_encode_sequence().unwrap();
    let data = Bytes::from(encoded);
    let (s, u) = decode_str_uint256(&data).unwrap().unwrap();
    assert_eq!(s, "");
    assert_eq!(u, "42");
}

#[test]
fn decode_str_uint256_empty_data_returns_none() {
    let data = Bytes::from(vec![]);
    assert!(decode_str_uint256(&data).is_none());
}

#[test]
fn decode_str_uint256_invalid_bytes_returns_err() {
    let data = Bytes::from(vec![0xffu8; 32]);
    let result = decode_str_uint256(&data);
    match result {
        Some(Err(hex_str)) => {
            assert!(hex_str.starts_with("0x"));
        }
        other => panic!("expected Some(Err(hex)), got {:?}", other),
    }
}

// ── Priority routing tests ──────────────────────────────────────────────

#[test]
fn test_is_notification_event_positive() {
    assert!(EventType::ContentCopyMinted.is_notification_event(), "ContentCopyMinted");
    assert!(EventType::Followed.is_notification_event(), "Followed (legacy)");
    assert!(EventType::SnapLiked.is_notification_event(), "SnapLiked");
    assert!(EventType::ArtLiked.is_notification_event(), "ArtLiked");
    assert!(EventType::MusicLiked.is_notification_event(), "MusicLiked");
    assert!(EventType::FlixLiked.is_notification_event(), "FlixLiked");
}

#[test]
fn test_is_notification_event_negative() {
    assert!(!EventType::ContentMinted.is_notification_event(), "ContentMinted is analytics");
    assert!(!EventType::ListingUpdated.is_notification_event(), "ListingUpdated is analytics");
    assert!(!EventType::ProfileUpdated.is_notification_event(), "ProfileUpdated is analytics");
    assert!(!EventType::RoyaltyDistributed.is_notification_event(), "RoyaltyDistributed is analytics");
    assert!(!EventType::EarningsWithdrawn.is_notification_event(), "EarningsWithdrawn is analytics");
}

#[test]
fn test_kafka_topic_priority_routing() {
    assert_eq!(EventType::SnapLiked.kafka_topic(), "notifications.priority");
    assert_eq!(EventType::Followed.kafka_topic(), "notifications.priority");
    assert_eq!(EventType::ProfileUpdated.kafka_topic(), "user.actions");
    assert_eq!(EventType::ContentMinted.kafka_topic(), "blockchain.events");
    assert_eq!(EventType::ListingUpdated.kafka_topic(), "blockchain.events");
    assert_eq!(EventType::PlatformFeeUpdated.kafka_topic(), "blockchain.events");
}

// ── event_kafka_key ─────────────────────────────────────────────────────

#[test]
fn event_kafka_key_format() {
    let event = ParsedEvent {
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
    let key = event_kafka_key(&event);
    assert_eq!(key, "snap.0xdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef");
}

#[test]
fn event_kafka_key_friends_contract() {
    let event = ParsedEvent {
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
    let key = event_kafka_key(&event);
    assert_eq!(key, "friends.0x1111111111111111111111111111111111111111");
}

#[test]
fn event_kafka_key_common_contract() {
    let event = ParsedEvent {
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
    let key = event_kafka_key(&event);
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
    assert!(!EventType::ContentMinted.is_purchase(), "ContentMinted");
    assert!(!EventType::SnapMinted.is_purchase(), "SnapMinted");
    assert!(!EventType::ListingUpdated.is_purchase(), "ListingUpdated");
    assert!(!EventType::SnapLiked.is_purchase(), "SnapLiked");
    assert!(!EventType::RoyaltyDistributed.is_purchase(), "RoyaltyDistributed");
    assert!(!EventType::Unknown.is_purchase(), "Unknown");
}

// ── EventType::contract_type — full coverage ────────────────────────────

#[test]
fn contract_type_all_media_types() {
    assert_eq!(EventType::ArtMinted.contract_type(), "art");
    assert_eq!(EventType::ArtLiked.contract_type(), "art");
    assert_eq!(EventType::ArtCommented.contract_type(), "art");
    assert_eq!(EventType::ArtBoughtAndMinted.contract_type(), "art");
    assert_eq!(EventType::ArtDeleted.contract_type(), "art");
    assert_eq!(EventType::MusicMinted.contract_type(), "music");
    assert_eq!(EventType::MusicLiked.contract_type(), "music");
    assert_eq!(EventType::MusicCommented.contract_type(), "music");
    assert_eq!(EventType::MusicBoughtAndMinted.contract_type(), "music");
    assert_eq!(EventType::MusicDeleted.contract_type(), "music");
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
    assert_eq!(EventType::ContentMinted.contract_type(), "friends");
    assert_eq!(EventType::ContentCopyMinted.contract_type(), "friends");
    assert_eq!(EventType::ListingUpdated.contract_type(), "friends");
    assert_eq!(EventType::ContentBlocked.contract_type(), "friends");
    assert_eq!(EventType::ContentBookmarked.contract_type(), "friends");
    assert_eq!(EventType::ContentBurned.contract_type(), "friends");
    assert_eq!(EventType::BadgeAwarded.contract_type(), "friends");
    assert_eq!(EventType::BadgeRemoved.contract_type(), "friends");
    assert_eq!(EventType::TipSent.contract_type(), "friends");
    assert_eq!(EventType::PlatformFeeUpdated.contract_type(), "friends");
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
}

#[test]
fn is_like_negative() {
    assert!(!EventType::ContentMinted.is_like());
    assert!(!EventType::ListingUpdated.is_like());
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
    assert!(EventType::BadgeAwarded.is_social());
    assert!(EventType::BadgeRemoved.is_social());
    assert!(EventType::TipSent.is_social());
    assert!(EventType::ContentBookmarked.is_social());
    assert!(EventType::ContentShared.is_social());
}

#[test]
fn is_social_negative() {
    assert!(!EventType::ContentMinted.is_social());
    assert!(!EventType::ListingUpdated.is_social());
    assert!(!EventType::PlatformFeeUpdated.is_social());
    assert!(!EventType::SnapBoughtAndMinted.is_social());
    assert!(!EventType::Transfer.is_social());
    assert!(!EventType::Unknown.is_social());
}

// ── parse_log: empty-topics fallback and unknown-event raw data ──────────

#[test]
fn parse_log_empty_topics_returns_unknown() {
    let log = test_log(vec![], Bytes::new());
    let parsed = parse_log(&log, "snap").expect("parse failed");
    assert_eq!(parsed.event_type, "Unknown");
    assert_eq!(parsed.contract_type, "snap");
    assert!(parsed.indexed_params.is_empty());
}

#[test]
fn parse_log_unrecognised_topic_uses_fallback_contract_type() {
    let mystery_sig = b256_from_hex(
        "0xffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
    );
    let log = test_log(vec![mystery_sig], Bytes::new());
    let parsed = parse_log(&log, "art").expect("parse failed");
    assert_eq!(parsed.event_type, "Unknown");
    assert_eq!(parsed.contract_type, "art");
}

#[test]
fn parse_log_unknown_event_with_data_produces_raw() {
    let mystery_sig = b256_from_hex(
        "0xffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
    );
    let log = test_log(vec![mystery_sig], Bytes::from(vec![0xde, 0xad, 0xbe, 0xef]));
    let parsed = parse_log(&log, "common").expect("parse failed");
    let raw = parsed.raw_data.expect("expected raw_data");
    assert!(raw.starts_with("0x"));
    assert!(raw.contains("deadbeef"));
}

// ── ContentMinted contentType → contract_type mapping ───────────────────

#[test]
fn parse_content_minted_content_type_mapping() {
    let sig = b256_from_hex(
        "0xe913bf0f321ec4538e6e03894963538ad29d5bc7610699f655b8d4be77ef3c31",
    );
    let token_topic = b256_from_hex(
        "0x0000000000000000000000000000000000000000000000000000000000000001",
    );
    let creator_topic = b256_from_hex(
        "0x000000000000000000000000aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    );

    let cases: &[(u64, &str)] = &[(0, "art"), (1, "flix"), (2, "music"), (3, "snap")];

    for (ct_value, expected_type) in cases {
        let mut ct_bytes = [0u8; 32];
        ct_bytes[31] = *ct_value as u8;
        let ct_topic = B256::from(ct_bytes);
        let data = Bytes::from(vec![0u8; 64]);
        let log = test_log(vec![sig, token_topic, creator_topic, ct_topic], data);
        let parsed = parse_log(&log, "friends").expect("parse failed");
        assert_eq!(
            parsed.contract_type, *expected_type,
            "contentType={ct_value} should map to contract_type={expected_type}"
        );
    }
}
