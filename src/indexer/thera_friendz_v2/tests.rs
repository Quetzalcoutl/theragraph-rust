use super::*;
use alloy::primitives::{Address, Bytes};

fn uint_topic(v: u64) -> B256 {
    B256::from(U256::from(v).to_be_bytes::<32>())
}

fn addr_topic(addr: Address) -> B256 {
    let mut bytes = [0u8; 32];
    bytes[12..].copy_from_slice(addr.as_slice());
    B256::from(bytes)
}

fn addr(byte: u8) -> Address {
    Address::from([byte; 20])
}

/// Builds a synthetic log with full control over topics/data/address/block
/// position — needed (unlike `events::tests::test_log`) because correlation
/// tests depend on matching `transaction_hash` across two logs.
fn v2_log(topics: Vec<B256>, data: impl Into<Bytes>, block_number: u64, log_index: u64, tx_byte: u8) -> Log {
    Log {
        inner: alloy::primitives::Log {
            address: Address::from([0xABu8; 20]),
            data: alloy::primitives::LogData::new_unchecked(topics, data.into()),
        },
        block_number: Some(block_number),
        log_index: Some(log_index),
        transaction_hash: Some(B256::from([tx_byte; 32])),
        ..Default::default()
    }
}

fn encode_listing_created_data(price: u64, max_copies: u32) -> Bytes {
    let tokens = DynSolValue::Tuple(vec![
        DynSolValue::Uint(U256::from(price), 256),
        DynSolValue::Uint(U256::from(max_copies), 32),
        DynSolValue::Array(vec![]),
        DynSolValue::Array(vec![]),
        DynSolValue::Bool(true),
        DynSolValue::FixedBytes(B256::ZERO, 32),
    ]);
    Bytes::from(tokens.abi_encode_params())
}

fn encode_content_metadata_data(content_type: u8, uri: &str) -> Bytes {
    let tokens = DynSolValue::Tuple(vec![
        DynSolValue::Uint(U256::from(content_type), 8),
        DynSolValue::String(uri.to_string()),
    ]);
    Bytes::from(tokens.abi_encode_params())
}

fn encode_copy_purchased_data(recipient: Address) -> Bytes {
    let tokens = DynSolValue::Tuple(vec![
        DynSolValue::Address(recipient),
        DynSolValue::Uint(U256::from(1000u64), 256), // grossPrice
        DynSolValue::Uint(U256::from(50u64), 256),   // platformFee
        DynSolValue::Uint(U256::ZERO, 256),          // curatorFee
        DynSolValue::Address(Address::ZERO),         // referrer
        DynSolValue::Uint(U256::from(950u64), 256),  // creatorPool
    ]);
    Bytes::from(tokens.abi_encode_params())
}

fn bool_word(value: bool) -> Bytes {
    let mut buf = [0u8; 32];
    if value {
        buf[31] = 1;
    }
    Bytes::from(buf.to_vec())
}

#[test]
fn test_topic0_hashes_are_distinct() {
    let all = [
        V2_TOPICS.listing_created,
        V2_TOPICS.content_metadata,
        V2_TOPICS.copy_purchased,
        V2_TOPICS.listing_updated,
        V2_TOPICS.content_moderated,
        V2_TOPICS.user_moderated,
    ];
    for i in 0..all.len() {
        for j in (i + 1)..all.len() {
            assert_ne!(all[i], all[j], "topic0 collision between index {i} and {j}");
        }
    }
}

#[test]
fn test_content_type_contract_bucket_mapping() {
    assert_eq!(content_type_contract_bucket("0"), "art");
    assert_eq!(content_type_contract_bucket("1"), "vid");
    assert_eq!(content_type_contract_bucket("2"), "music");
    assert_eq!(content_type_contract_bucket("3"), "snap");
    assert_eq!(content_type_contract_bucket("99"), "friends");
}

#[test]
fn test_decode_listing_created() {
    let creator = addr(0xAA);
    let log = v2_log(
        vec![V2_TOPICS.listing_created, uint_topic(42), addr_topic(creator)],
        encode_listing_created_data(1_000, 5),
        100,
        0,
        0x01,
    );
    let (original_id, decoded_creator, price, max_copies) =
        decode_listing_created(&log).expect("decode failed");
    assert_eq!(original_id, "42");
    assert_eq!(decoded_creator, format!("{creator:#x}"));
    assert_eq!(price, "1000");
    assert_eq!(max_copies, "5");
}

#[test]
fn test_decode_content_metadata() {
    let log = v2_log(
        vec![V2_TOPICS.content_metadata, uint_topic(42)],
        encode_content_metadata_data(2, "ipfs://example"),
        100,
        1,
        0x01,
    );
    let (original_id, content_type) = decode_content_metadata(&log).expect("decode failed");
    assert_eq!(original_id, "42");
    assert_eq!(content_type, "2");
}

#[test]
fn test_mint_correlation_produces_content_minted_and_listing_updated() {
    let creator = addr(0xAA);
    let listing_log = v2_log(
        vec![V2_TOPICS.listing_created, uint_topic(42), addr_topic(creator)],
        encode_listing_created_data(1_000, 5),
        100,
        0,
        0x01,
    );
    let metadata_log = v2_log(
        vec![V2_TOPICS.content_metadata, uint_topic(42)],
        encode_content_metadata_data(2, "ipfs://example"),
        100,
        1,
        0x01, // same tx as listing_log
    );

    let results = parse_v2_logs(&[listing_log, metadata_log]);
    assert_eq!(results.len(), 2, "expected one ContentMinted + one ListingUpdated");

    let minted = results[0].as_ref().expect("ContentMinted should parse");
    assert_eq!(minted.event_type, "ContentMinted");
    assert_eq!(minted.contract_type, "music"); // contentType=2
    match &minted.data {
        Some(ParsedEventData::MintedV2 { token_id, creator: c, content_type, price, contract_address, block_number, transaction_hash, .. }) => {
            assert_eq!(token_id, "42");
            assert_eq!(c, &format!("{creator:#x}"));
            assert_eq!(content_type, "2");
            assert_eq!(price, "1000");
            assert_eq!(contract_address, &format!("{:?}", Address::from([0xABu8; 20])));
            assert_eq!(*block_number, 100);
            assert_eq!(transaction_hash, &format!("{:#x}", B256::from([0x01u8; 32])));
        }
        other => panic!("expected MintedV2, got {other:?}"),
    }

    let listing_updated = results[1].as_ref().expect("ListingUpdated should parse");
    assert_eq!(listing_updated.event_type, "ListingUpdated");
    match &listing_updated.data {
        Some(ParsedEventData::ListingUpdatedV2 { original_token_id, price, max_copies, .. }) => {
            assert_eq!(original_token_id, "42");
            assert_eq!(price, "1000");
            assert_eq!(max_copies, "5");
        }
        other => panic!("expected ListingUpdatedV2, got {other:?}"),
    }
}

#[test]
fn test_content_metadata_without_listing_created_is_dropped_not_crashed() {
    let metadata_log = v2_log(
        vec![V2_TOPICS.content_metadata, uint_topic(42)],
        encode_content_metadata_data(2, "ipfs://example"),
        100,
        1,
        0x02,
    );
    let results = parse_v2_logs(&[metadata_log]);
    assert!(results.is_empty(), "an orphan ContentMetadata must be dropped, not panic or half-emit");
}

#[test]
fn test_decode_copy_purchased_maps_recipient_not_payer() {
    let payer = addr(0x11);
    let recipient = addr(0x22);
    let log = v2_log(
        vec![
            V2_TOPICS.copy_purchased,
            uint_topic(42),   // originalId
            uint_topic(43),   // copyId
            addr_topic(payer), // buyer (payer) — must NOT end up as "buyer" in the output
        ],
        encode_copy_purchased_data(recipient),
        200,
        2,
        0x03,
    );

    let event = decode_copy_purchased(&log).expect("decode failed");
    assert_eq!(event.event_type, "ContentCopyMinted");
    assert_eq!(event.kafka_topic, "notifications.priority");
    match &event.data {
        Some(ParsedEventData::CopyMintedV2 { original_id, buyer, new_token_id, .. }) => {
            assert_eq!(original_id, "42");
            assert_eq!(new_token_id, "43");
            assert_eq!(buyer, &format!("{recipient:#x}"), "buyer field must carry the copy's actual owner (recipient), not the payer");
            assert_ne!(buyer, &format!("{payer:#x}"));
        }
        other => panic!("expected CopyMintedV2, got {other:?}"),
    }
}

// Ticket 037: the v3 market mints copy ids `(originalId << 64) | serial`. Test ids from the
// wire contract: original 7 / serial 3 = 129127208515966861315, 2^70, and 2^256 - 1.
#[test]
fn test_decode_copy_purchased_with_a_v3_copy_id_past_u64() {
    use crate::token_id::TokenId;
    let payer = addr(0x11);
    let recipient = addr(0x22);
    let copy_id = (U256::from(7u8) << 64usize) | U256::from(3u8);
    let log = v2_log(
        vec![
            V2_TOPICS.copy_purchased,
            B256::from(U256::from(7u8).to_be_bytes::<32>()), // originalId
            B256::from(copy_id.to_be_bytes::<32>()),         // copyId
            addr_topic(payer),
        ],
        encode_copy_purchased_data(recipient),
        200,
        2,
        0x03,
    );

    let event = decode_copy_purchased(&log).expect("decode failed");
    match &event.data {
        Some(ParsedEventData::CopyMintedV2 { original_id, new_token_id, .. }) => {
            assert_eq!(original_id, "7");
            assert_eq!(new_token_id, "129127208515966861315");
            assert_eq!(TokenId::parse(new_token_id).unwrap().as_str(), "129127208515966861315");
        }
        other => panic!("expected CopyMintedV2, got {other:?}"),
    }
    assert_eq!(event.indexed_params[2], "129127208515966861315");
    let json = serde_json::to_value(&event).unwrap();
    assert_eq!(json["data"]["new_token_id"], serde_json::json!("129127208515966861315"));
}

#[test]
fn test_mint_correlation_keeps_an_original_id_of_2_pow_70_and_the_uint256_maximum() {
    use crate::token_id::TokenId;
    let creator = addr(0xAA);
    for (id, expected) in [
        (U256::from(1u8) << 70usize, "1180591620717411303424"),
        (
            U256::MAX,
            "115792089237316195423570985008687907853269984665640564039457584007913129639935",
        ),
    ] {
        let topic = B256::from(id.to_be_bytes::<32>());
        let listing_log = v2_log(
            vec![V2_TOPICS.listing_created, topic, addr_topic(creator)],
            encode_listing_created_data(1_000, 5),
            100,
            0,
            0x07,
        );
        let metadata_log = v2_log(
            vec![V2_TOPICS.content_metadata, topic],
            encode_content_metadata_data(2, "ipfs://example"),
            100,
            1,
            0x07,
        );
        let results = parse_v2_logs(&[listing_log, metadata_log]);
        assert_eq!(results.len(), 2);
        match &results[0].as_ref().unwrap().data {
            Some(ParsedEventData::MintedV2 { token_id, .. }) => {
                assert_eq!(token_id, expected);
                assert!(TokenId::parse(token_id).is_ok());
            }
            other => panic!("expected MintedV2, got {other:?}"),
        }
        match &results[1].as_ref().unwrap().data {
            Some(ParsedEventData::ListingUpdatedV2 { original_token_id, .. }) => {
                assert_eq!(original_token_id, expected);
            }
            other => panic!("expected ListingUpdatedV2, got {other:?}"),
        }
    }
}

#[test]
fn test_decode_content_moderated_blocked_true_emits_content_blocked() {
    let log = v2_log(
        vec![V2_TOPICS.content_moderated, uint_topic(42)],
        bool_word(true),
        300,
        0,
        0x04,
    );
    let event = decode_content_moderated(&log).expect("blocked=true must emit an event");
    assert_eq!(event.event_type, "ContentBlocked");
    match &event.data {
        Some(ParsedEventData::ContentBlockedV2 { token_id, .. }) => assert_eq!(token_id, "42"),
        other => panic!("expected ContentBlockedV2, got {other:?}"),
    }
}

#[test]
fn test_decode_content_moderated_blocked_false_is_dropped() {
    let log = v2_log(
        vec![V2_TOPICS.content_moderated, uint_topic(42)],
        bool_word(false),
        300,
        0,
        0x05,
    );
    assert!(
        decode_content_moderated(&log).is_none(),
        "unblock (blocked=false) must not be forwarded — see module doc"
    );
}

#[test]
fn test_decode_user_moderated() {
    let user = addr(0x33);
    let log = v2_log(
        vec![V2_TOPICS.user_moderated, addr_topic(user)],
        bool_word(true),
        400,
        0,
        0x06,
    );
    let event = decode_user_moderated(&log).expect("decode failed");
    assert_eq!(event.event_type, "UserBlocked");
    assert_eq!(event.kafka_topic, "user.actions");
    match &event.data {
        Some(ParsedEventData::UserBlockedEvent { user: u, status, .. }) => {
            assert_eq!(u, &format!("{user:#x}"));
            assert!(*status);
        }
        other => panic!("expected UserBlockedEvent, got {other:?}"),
    }
}

#[test]
fn test_unknown_topic_falls_back_to_generic_parser() {
    // Standard ERC-721 Transfer(address,address,uint256) — not one of the six
    // v10-specific topics, so it must fall back to events::parse_log exactly
    // like every other indexer does for logs outside its own known set.
    let transfer_sig = keccak256("Transfer(address,address,uint256)".as_bytes());
    let from = addr(0x00);
    let to = addr(0x44);
    let log = v2_log(
        vec![transfer_sig, addr_topic(from), addr_topic(to), uint_topic(7)],
        Bytes::new(),
        500,
        0,
        0x07,
    );
    let results = parse_v2_logs(&[log]);
    assert_eq!(results.len(), 1);
    let parsed = results[0].as_ref().expect("Transfer should parse via the generic path");
    assert_eq!(parsed.event_type, "Transfer");
}

/// Golden-vector test against a REAL transaction observed live on Sepolia
/// during this indexer's development: `eth_getLogs` against
/// `0x0d43e13f2b7ab58b4239a74842b2f4568b7ebd35` (block range 11798134–11848134
/// on a public Sepolia RPC) turned up
/// `0x1e720680b5d39c4118ccf415a3c3be168b50df24aa25477b237ef242f442380f` — a
/// `redeemAndBuy` call (mint + immediate first copy purchase in one tx). Its
/// topic0s matched this module's computed `V2_TOPICS` exactly, confirming the
/// event signatures against the live contract, not just the Solidity source.
/// The raw topics/data below are copied verbatim from that response; the
/// asserted values (price, content_type, addresses, token ids) were computed
/// by hand from the same raw hex to confirm this decoder, not the other way
/// around — see the price/content-type comments inline.
#[test]
fn test_decodes_real_sepolia_mint_and_buy_transaction() {
    fn hex_topic(s: &str) -> B256 { s.parse().unwrap() }
    fn hex_data(s: &str) -> alloy::primitives::Bytes { s.parse().unwrap() }

    let addr = "0x0d43e13f2b7ab58b4239a74842b2f4568b7ebd35".parse::<alloy::primitives::Address>().unwrap();

    let listing_created_log = Log {
        inner: alloy::primitives::Log {
            address: addr,
            data: alloy::primitives::LogData::new_unchecked(
                vec![
                    hex_topic("0x288d9e656f5cefe66d1bd8353ffd65a71dee875b352d803e73ee5bc1ac99e77b"),
                    hex_topic("0x0000000000000000000000000000000000000000000000000000000000000001"),
                    hex_topic("0x00000000000000000000000056ea028564797007daf5b9af3c67de91537482ce"),
                ],
                hex_data("0x0000000000000000000000000000000000000000000000004563918244f40000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000c000000000000000000000000000000000000000000000000000000000000000e00000000000000000000000000000000000000000000000000000000000000001eec48d7e85425ad846336e07d99b4c1fae48f5be506cc66b18a2511794daa5f200000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000"),
            ),
        },
        block_number: Some(0xb42032),
        log_index: Some(0xc),
        transaction_hash: Some(hex_topic("0x1e720680b5d39c4118ccf415a3c3be168b50df24aa25477b237ef242f442380f")),
        ..Default::default()
    };

    let content_metadata_log = Log {
        inner: alloy::primitives::Log {
            address: addr,
            data: alloy::primitives::LogData::new_unchecked(
                vec![
                    hex_topic("0xf61c065e8b5fe8c7f09e110c192f509e2243faeb6e8a975ff9fb8c04ed9ccac0"),
                    hex_topic("0x0000000000000000000000000000000000000000000000000000000000000001"),
                ],
                hex_data("0x00000000000000000000000000000000000000000000000000000000000000020000000000000000000000000000000000000000000000000000000000000040000000000000000000000000000000000000000000000000000000000000001d697066733a2f2f6532652f6c617a792f31373930363439303234383935000000"),
            ),
        },
        block_number: Some(0xb42032),
        log_index: Some(0xd),
        transaction_hash: Some(hex_topic("0x1e720680b5d39c4118ccf415a3c3be168b50df24aa25477b237ef242f442380f")),
        ..Default::default()
    };

    let copy_purchased_log = Log {
        inner: alloy::primitives::Log {
            address: addr,
            data: alloy::primitives::LogData::new_unchecked(
                vec![
                    hex_topic("0x5d4c465cf4377abcbf2f9d0bf1d98f03b8c3c03d3e3c2ff979092c9facd7ed7e"),
                    hex_topic("0x0000000000000000000000000000000000000000000000000000000000000001"),
                    hex_topic("0x0000000000000000000000000000000000000000000000000000000000000002"),
                    hex_topic("0x000000000000000000000000cab09bffea9fa24342131bd749fc0f63d7b99bbd"),
                ],
                hex_data("0x000000000000000000000000cab09bffea9fa24342131bd749fc0f63d7b99bbd0000000000000000000000000000000000000000000000004563918244f400000000000000000000000000000000000000000000000000000de0b6b3a7640000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000003782dace9d900000"),
            ),
        },
        block_number: Some(0xb42032),
        log_index: Some(0xf),
        transaction_hash: Some(hex_topic("0x1e720680b5d39c4118ccf415a3c3be168b50df24aa25477b237ef242f442380f")),
        ..Default::default()
    };

    let results = parse_v2_logs(&[listing_created_log, content_metadata_log, copy_purchased_log]);
    assert_eq!(results.len(), 3, "expected ContentMinted + mint-time ListingUpdated + ContentCopyMinted");

    let minted = results[0].as_ref().expect("ContentMinted should decode");
    assert_eq!(minted.event_type, "ContentMinted");
    // contentType=2 in the real ContentMetadata payload -> "music" bucket.
    assert_eq!(minted.contract_type, "music");
    assert_eq!(minted.block_number, 0xb42032);
    match &minted.data {
        Some(ParsedEventData::MintedV2 { token_id, creator, content_type, price, contract_address, .. }) => {
            assert_eq!(token_id, "1");
            assert_eq!(creator, "0x56ea028564797007daf5b9af3c67de91537482ce");
            assert_eq!(content_type, "2");
            // 0x4563918244f40000 wei = 5 THERA (18 decimals) — the real on-chain listing price.
            assert_eq!(price, "5000000000000000000");
            assert_eq!(contract_address, "0x0d43e13f2b7ab58b4239a74842b2f4568b7ebd35");
        }
        other => panic!("expected MintedV2, got {other:?}"),
    }

    let listing_updated = results[1].as_ref().expect("mint-time ListingUpdated should decode");
    assert_eq!(listing_updated.event_type, "ListingUpdated");
    match &listing_updated.data {
        Some(ParsedEventData::ListingUpdatedV2 { original_token_id, price, max_copies, .. }) => {
            assert_eq!(original_token_id, "1");
            assert_eq!(price, "5000000000000000000");
            assert_eq!(max_copies, "0", "the real voucher listed unlimited copies");
        }
        other => panic!("expected ListingUpdatedV2, got {other:?}"),
    }

    let copy_minted = results[2].as_ref().expect("ContentCopyMinted should decode");
    assert_eq!(copy_minted.event_type, "ContentCopyMinted");
    match &copy_minted.data {
        Some(ParsedEventData::CopyMintedV2 { original_id, buyer, new_token_id, .. }) => {
            assert_eq!(original_id, "1");
            assert_eq!(new_token_id, "2");
            // `recipient` in the real CopyPurchased data equalled the indexed
            // `buyer` (payer) here — a direct buyCopy, not buyCopyFor — but this
            // asserts the field this decoder actually reads (recipient).
            assert_eq!(buyer, "0xcab09bffea9fa24342131bd749fc0f63d7b99bbd");
        }
        other => panic!("expected CopyMintedV2, got {other:?}"),
    }
}
