use super::*;
    use crate::recommendation::preferences::InteractionType;

    // ── bookmark_to_interaction ─────────────────────────────────────────────

    #[test]
    fn bookmark_true_maps_to_save() {
        assert_eq!(bookmark_to_interaction(true), InteractionType::Save);
    }

    #[test]
    fn bookmark_false_maps_to_unsave() {
        assert_eq!(bookmark_to_interaction(false), InteractionType::Unsave);
    }

    // ── is_positive_like ────────────────────────────────────────────────────

    #[test]
    fn like_is_positive() {
        assert!(is_positive_like(&InteractionType::Like));
    }

    #[test]
    fn unlike_is_not_positive() {
        assert!(!is_positive_like(&InteractionType::Unlike));
    }

    #[test]
    fn other_types_are_not_positive_like() {
        for t in &[
            InteractionType::Comment,
            InteractionType::Purchase,
            InteractionType::Share,
            InteractionType::Save,
            InteractionType::Unsave,
            InteractionType::View,
        ] {
            assert!(!is_positive_like(t), "expected false for {:?}", t);
        }
    }

    // ── extract_generic_user ────────────────────────────────────────────────

    #[test]
    fn extract_user_key_takes_priority() {
        let data = serde_json::json!({
            "user": "0xUSER",
            "commenter": "0xCOMMENTER",
            "sharer": "0xSHARER"
        });
        assert_eq!(extract_generic_user(&data), Some("0xuser".to_string()));
    }

    #[test]
    fn extract_commenter_fallback() {
        let data = serde_json::json!({ "commenter": "0xCOMMENTER" });
        assert_eq!(extract_generic_user(&data), Some("0xcommenter".to_string()));
    }

    #[test]
    fn extract_sharer_fallback() {
        let data = serde_json::json!({ "sharer": "0xSHARER" });
        assert_eq!(extract_generic_user(&data), Some("0xsharer".to_string()));
    }

    #[test]
    fn extract_generic_user_no_keys_returns_none() {
        let data = serde_json::json!({ "liker": "0xLIKER" });
        assert!(extract_generic_user(&data).is_none());
    }

    #[test]
    fn extract_generic_user_empty_string_returns_none() {
        let data = serde_json::json!({ "user": "" });
        assert!(extract_generic_user(&data).is_none());
    }

    #[test]
    fn extract_generic_user_lowercases_address() {
        let data = serde_json::json!({ "user": "0xABCDEF" });
        assert_eq!(extract_generic_user(&data), Some("0xabcdef".to_string()));
    }

    // ── InteractionType::Display ────────────────────────────────────────────

    #[test]
    fn interaction_type_display_strings() {
        use std::fmt::Display;
        let cases = [
            (InteractionType::View,     "view"),
            (InteractionType::Like,     "like"),
            (InteractionType::Unlike,   "unlike"),
            (InteractionType::Comment,  "comment"),
            (InteractionType::Purchase, "purchase"),
            (InteractionType::Share,    "share"),
            (InteractionType::Save,     "save"),
            (InteractionType::Unsave,   "unsave"),
        ];
        for (t, expected) in &cases {
            assert_eq!(t.to_string(), *expected, "mismatch for {:?}", t);
        }
    }

    // ── generate_nft_uuid (deterministic v5 UUID) ───────────────────────────
    // generate_nft_uuid lives on EventProcessor (in elixir_db.rs) and is
    // pub(super). We mirror the algorithm here to verify the contract that
    // direct.rs and interaction.rs both rely on.

    fn uuid_v5_from_contract_token(contract: &str, token: &str) -> uuid::Uuid {
        let combined = format!("{}:{}", contract.to_lowercase(), token);
        uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, combined.as_bytes())
    }

    #[test]
    fn nft_uuid_is_deterministic() {
        let a = uuid_v5_from_contract_token("0xContract", "42");
        let b = uuid_v5_from_contract_token("0xContract", "42");
        assert_eq!(a, b);
    }

    #[test]
    fn nft_uuid_case_insensitive_on_contract_address() {
        let lower = uuid_v5_from_contract_token("0xcontract", "42");
        let upper = uuid_v5_from_contract_token("0xCONTRACT", "42");
        assert_eq!(lower, upper);
    }

    #[test]
    fn nft_uuid_different_token_ids_differ() {
        let a = uuid_v5_from_contract_token("0xcontract", "1");
        let b = uuid_v5_from_contract_token("0xcontract", "2");
        assert_ne!(a, b);
    }

    #[test]
    fn nft_uuid_different_contracts_differ() {
        let a = uuid_v5_from_contract_token("0xcontract1", "42");
        let b = uuid_v5_from_contract_token("0xcontract2", "42");
        assert_ne!(a, b);
    }
