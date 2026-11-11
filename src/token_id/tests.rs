use super::*;
use serde_json::json;

/// The contract's test ids (`.wayfinder/research/037-wire-contract.md`).
const TWO_POW_70: &str = "1180591620717411303424";
/// (7 << 64) | 3: original 7, serial 3.
const COPY_7_3: &str = "129127208515966861315";
/// 2^256 - 1.
const UINT256_MAX: &str =
    "115792089237316195423570985008687907853269984665640564039457584007913129639935";
/// 2^256.
const UINT256_MAX_PLUS_ONE: &str =
    "115792089237316195423570985008687907853269984665640564039457584007913129639936";

const CONTRACT_IDS: [&str; 3] = [TWO_POW_70, COPY_7_3, UINT256_MAX];

// ── parse ────────────────────────────────────────────────────────────────────

#[test]
fn parse_accepts_the_contract_ids_and_small_ids() {
    for s in CONTRACT_IDS {
        let id = TokenId::parse(s).unwrap_or_else(|e| panic!("{s} must parse: {e}"));
        assert_eq!(id.as_str(), s);
        assert_eq!(id.to_string(), s);
    }
    for s in ["0", "1", "42", "9007199254740993", "9223372036854775807", "18446744073709551616"] {
        assert_eq!(TokenId::parse(s).unwrap().as_str(), s);
    }
}

#[test]
fn the_contract_ids_are_what_the_contract_says() {
    assert_eq!(TWO_POW_70, (1u128 << 70).to_string());
    assert_eq!(COPY_7_3, ((7u128 << 64) | 3).to_string());
    // 2^256 - 1 does not fit a u128, so check the digit count and the last digits.
    assert_eq!(UINT256_MAX.len(), 78);
    assert!(UINT256_MAX.ends_with("639935"));
    assert!(UINT256_MAX_PLUS_ONE.ends_with("639936"));
}

#[test]
fn parse_refuses_every_noncanonical_form() {
    let refused: [(&str, TokenIdError); 14] = [
        ("", TokenIdError::Empty),
        ("007", TokenIdError::LeadingZero),
        ("00", TokenIdError::LeadingZero),
        ("-1", TokenIdError::NotDecimal),
        ("+5", TokenIdError::NotDecimal),
        ("1e3", TokenIdError::NotDecimal),
        (" 5", TokenIdError::NotDecimal),
        ("5 ", TokenIdError::NotDecimal),
        ("5\n", TokenIdError::NotDecimal),
        ("1.0", TokenIdError::NotDecimal),
        ("0x10", TokenIdError::NotDecimal),
        ("abc", TokenIdError::NotDecimal),
        // An Arabic-Indic digit three: a Unicode digit, not an ASCII one.
        ("\u{0663}", TokenIdError::NotDecimal),
        (UINT256_MAX_PLUS_ONE, TokenIdError::TooLarge),
    ];
    for (s, why) in refused {
        assert_eq!(TokenId::parse(s), Err(why), "{s:?} must be refused");
    }
}

#[test]
fn parse_refuses_anything_past_78_digits_and_accepts_the_edge() {
    // 79 digits of nines, and 78 digits just above the maximum.
    assert_eq!(TokenId::parse(&"9".repeat(79)), Err(TokenIdError::TooLarge));
    assert_eq!(TokenId::parse(&"9".repeat(78)), Err(TokenIdError::TooLarge));
    // 77 digits of nines is below 2^256 and fine.
    assert!(TokenId::parse(&"9".repeat(77)).is_ok());
    // The very maximum is fine, one more is not.
    assert!(TokenId::parse(UINT256_MAX).is_ok());
    assert_eq!(TokenId::parse(UINT256_MAX_PLUS_ONE), Err(TokenIdError::TooLarge));
}

#[test]
fn from_str_is_parse() {
    assert_eq!(TWO_POW_70.parse::<TokenId>().unwrap().as_str(), TWO_POW_70);
    assert!("007".parse::<TokenId>().is_err());
}

#[test]
fn the_error_does_not_echo_the_input() {
    let message = TokenId::parse("secret-looking-input").unwrap_err().to_string();
    assert!(!message.contains("secret"), "{message}");
}

// ── draft sentinel ───────────────────────────────────────────────────────────

#[test]
fn draft_is_zero_and_only_zero() {
    assert_eq!(TokenId::draft().as_str(), "0");
    assert!(TokenId::draft().is_draft());
    assert!(TokenId::parse("0").unwrap().is_draft());
    assert!(!TokenId::parse("1").unwrap().is_draft());
    assert!(!TokenId::parse(TWO_POW_70).unwrap().is_draft());
}

// ── conversions ──────────────────────────────────────────────────────────────

#[test]
fn from_integers_is_canonical() {
    assert_eq!(TokenId::from(0u64).as_str(), "0");
    assert_eq!(TokenId::from(u64::MAX).as_str(), "18446744073709551615");
    assert_eq!(TokenId::from(1u128 << 70).as_str(), TWO_POW_70);
    assert_eq!(TokenId::from((7u128 << 64) | 3).as_str(), COPY_7_3);
    assert_eq!(String::from(TokenId::from(5u64)), "5");
}

// ── order: length first, then lexicographic ──────────────────────────────────

#[test]
fn order_is_numeric_without_parsing() {
    let id = |s: &str| TokenId::parse(s).unwrap();
    // The trap for a plain string sort: "9" > "10" lexicographically.
    assert!(id("9") < id("10"));
    assert!(id("0") < id("1"));
    assert!(id("99") < id("100"));
    assert!(id(COPY_7_3) < id(TWO_POW_70));
    assert!(id(TWO_POW_70) < id(UINT256_MAX));
    assert!(id("9007199254740993") < id("9007199254740994"));
    assert_eq!(id("5").cmp(&id("5")), Ordering::Equal);

    let mut ids = vec![id(UINT256_MAX), id("10"), id(TWO_POW_70), id("9"), id("0"), id(COPY_7_3)];
    ids.sort();
    let sorted: Vec<&str> = ids.iter().map(TokenId::as_str).collect();
    assert_eq!(sorted, ["0", "9", "10", COPY_7_3, TWO_POW_70, UINT256_MAX]);
}

// ── serde: JSON ──────────────────────────────────────────────────────────────

#[test]
fn serializes_as_a_json_string_never_a_number() {
    for s in CONTRACT_IDS {
        let id = TokenId::parse(s).unwrap();
        assert_eq!(serde_json::to_string(&id).unwrap(), format!("\"{s}\""));
        assert_eq!(serde_json::to_value(&id).unwrap(), json!(s));
    }
    assert_eq!(serde_json::to_string(&TokenId::from(7u64)).unwrap(), "\"7\"");
}

#[test]
fn deserializes_the_contract_ids_from_json_strings() {
    for s in CONTRACT_IDS {
        let id: TokenId = serde_json::from_str(&format!("\"{s}\"")).unwrap();
        assert_eq!(id.as_str(), s);
    }
}

#[test]
fn deserialize_refuses_the_refused_string_forms() {
    for s in ["007", "-1", "1e3", " 5", "", "1.0", UINT256_MAX_PLUS_ONE] {
        let json = serde_json::to_string(s).unwrap();
        assert!(serde_json::from_str::<TokenId>(&json).is_err(), "{s:?} must be refused");
    }
}

#[test]
fn deserialize_accepts_a_safe_integer_during_the_transition() {
    // What a pre-037 cached ScoredNft carried: the id as a bare JSON number.
    let id: TokenId = serde_json::from_str("42").unwrap();
    assert_eq!(id.as_str(), "42");
    let zero: TokenId = serde_json::from_str("0").unwrap();
    assert!(zero.is_draft());
    let edge: TokenId = serde_json::from_str("9007199254740991").unwrap();
    assert_eq!(edge.as_str(), "9007199254740991");
}

#[test]
fn deserialize_refuses_a_number_past_2_pow_53_and_every_non_integer() {
    // 2^53 itself is already ambiguous (a JS sender rounds 2^53 + 1 down to it).
    for json in [
        "9007199254740992",
        "9007199254740993",
        "1180591620717411303424", // 2^70 as a bare number: serde_json reads it as a float
        "-1",
        "-0.0",
        "1.0",
        "1e3",
        "1.5",
        "true",
        "null",
        "[]",
        "{}",
    ] {
        assert!(
            serde_json::from_str::<TokenId>(json).is_err(),
            "{json} must be refused"
        );
    }
}

#[test]
fn from_json_value_takes_a_string_or_a_safe_integer_only() {
    assert_eq!(TokenId::from_json(&json!(TWO_POW_70)).unwrap().as_str(), TWO_POW_70);
    assert_eq!(TokenId::from_json(&json!(COPY_7_3)).unwrap().as_str(), COPY_7_3);
    assert_eq!(TokenId::from_json(&json!(5)).unwrap().as_str(), "5");
    assert_eq!(TokenId::from_json(&json!("0")).unwrap(), TokenId::draft());
    assert_eq!(TokenId::from_json(&json!(9_007_199_254_740_993u64)), None);
    assert_eq!(TokenId::from_json(&json!(-1)), None);
    assert_eq!(TokenId::from_json(&json!(1.5)), None);
    assert_eq!(TokenId::from_json(&json!("007")), None);
    assert_eq!(TokenId::from_json(&json!("")), None);
    assert_eq!(TokenId::from_json(&json!(null)), None);
    assert_eq!(TokenId::from_json(&json!(true)), None);
    assert_eq!(TokenId::from_json(&json!(["5"])), None);
}

#[test]
fn an_optional_id_round_trips_and_null_is_none() {
    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Row {
        token_id: Option<TokenId>,
    }
    let some = Row { token_id: Some(TokenId::parse(TWO_POW_70).unwrap()) };
    let text = serde_json::to_string(&some).unwrap();
    assert_eq!(text, format!("{{\"token_id\":\"{TWO_POW_70}\"}}"));
    assert_eq!(serde_json::from_str::<Row>(&text).unwrap(), some);

    let none: Row = serde_json::from_str("{\"token_id\":null}").unwrap();
    assert_eq!(none, Row { token_id: None });

    let old: Row = serde_json::from_str("{\"token_id\":7}").unwrap();
    assert_eq!(old.token_id, Some(TokenId::from(7u64)));
}

// ── serde: MessagePack (the Redis wire format of RecCache) ───────────────────

#[test]
fn messagepack_round_trip_keeps_every_contract_id_exact() {
    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Row {
        token_id: TokenId,
    }
    for s in CONTRACT_IDS {
        let row = Row { token_id: TokenId::parse(s).unwrap() };
        let bytes = rmp_serde::to_vec_named(&row).unwrap();
        let back: Row = rmp_serde::from_slice(&bytes).unwrap();
        assert_eq!(back, row);
        assert_eq!(back.token_id.as_str(), s);
    }
}

#[test]
fn messagepack_written_before_037_with_an_integer_id_still_decodes() {
    #[derive(Serialize)]
    struct OldRow {
        token_id: i64,
    }
    #[derive(Debug, Deserialize)]
    struct NewRow {
        token_id: TokenId,
    }
    for (old, expected) in [(0i64, "0"), (1, "1"), (42, "42"), (9_007_199_254_740_991, "9007199254740991")] {
        let bytes = rmp_serde::to_vec_named(&OldRow { token_id: old }).unwrap();
        let row: NewRow = rmp_serde::from_slice(&bytes).unwrap();
        assert_eq!(row.token_id.as_str(), expected);
    }
    // A pre-037 id past 2^53 cannot be trusted any more: refused, so the cache
    // entry is a miss instead of a wrong token.
    let bytes = rmp_serde::to_vec_named(&OldRow { token_id: i64::MAX }).unwrap();
    assert!(rmp_serde::from_slice::<NewRow>(&bytes).is_err());
    let bytes = rmp_serde::to_vec_named(&OldRow { token_id: -1 }).unwrap();
    assert!(rmp_serde::from_slice::<NewRow>(&bytes).is_err());
}
