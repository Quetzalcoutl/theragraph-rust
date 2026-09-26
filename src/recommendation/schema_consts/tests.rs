use super::*;

    #[test]
    fn vid_user_formats_correctly() {
        assert_eq!(vid_user("0xdeadbeef1234567890abcdef1234567890abcdef"), "user:0xdeadbeef1234567890abcdef1234567890abcdef");
    }

    #[test]
    fn vid_post_formats_correctly() {
        let id = "550e8400-e29b-41d4-a716-446655440000";
        assert_eq!(vid_post(id), format!("post:{id}"));
    }

    #[test]
    fn is_safe_address_rejects_uppercase() {
        assert!(!is_safe_address("0xAb5801a7D398351b8bE11C439e05C5B3259aeC9B"));
        assert!(is_safe_address("0xab5801a7d398351b8be11c439e05c5b3259aec9b"));
    }

    #[test]
    fn is_safe_post_vid_id_rejects_oversized() {
        // Migration 25: cap is 59 (FIXED_STRING(64) - "post:" prefix), was 123.
        let long = "a".repeat(60);
        assert!(!is_safe_post_vid_id(&long));
        assert!(is_safe_post_vid_id("550e8400-e29b-41d4-a716-446655440000"));
    }

    #[test]
    fn ensure_user_vertex_nql_contains_if_not_exists() {
        let s = ensure_user_vertex_nql("user:0xabc", "0xabc");
        assert!(s.contains("IF NOT EXISTS"));
        assert!(s.contains("user:0xabc"));
    }

    #[test]
    fn ensure_post_vertex_nql_contains_if_not_exists() {
        let s = ensure_post_vertex_nql("post:uuid-123", "uuid-123");
        assert!(s.contains("IF NOT EXISTS"));
        assert!(s.contains("post:uuid-123"));
    }

    #[test]
    fn parse_schema_version_table_extracts_integer_from_table_row() {
        let output = "\
+-------------------+\n\
| version           |\n\
+-------------------+\n\
| 15                |\n\
+-------------------+\n\
Got 1 rows (time spent 2345/5678 us)\n";
        assert_eq!(parse_schema_version_table(output), Some(15));
    }

    #[test]
    fn parse_schema_version_table_returns_none_for_empty_result() {
        let output = "\
+-------------------+\n\
| version           |\n\
+-------------------+\n\
Empty set (time spent 123/456 us)\n";
        assert_eq!(parse_schema_version_table(output), None);
    }

    #[test]
    fn comment_rank_is_deterministic_for_same_event_id() {
        let id = "0xabc123def4567890";
        assert_eq!(comment_rank(id), comment_rank(id));
    }

    #[test]
    fn comment_rank_handles_uuid_style_event_ids_with_dashes() {
        // Regression: from_str_radix rejects dashes outright — a naive
        // "strip 0x, take 16 chars" derivation falls back to 0 for every
        // UUID-style event_id, collapsing all comments onto one edge.
        let rank = comment_rank("550e8400-e29b-41d4-a716-446655440000");
        assert_ne!(rank, 0);
    }

    #[test]
    fn comment_rank_never_overflows_i64_for_high_leading_nibble() {
        // 16 leading hex digits starting with 'f' overflows i64::MAX (63 bits);
        // the fix caps at 15 digits (60 bits) specifically to avoid this.
        let rank = comment_rank("0xffffffffffffffff");
        assert!(rank > 0);
    }

    #[test]
    fn comment_rank_falls_back_to_zero_for_empty_input() {
        assert_eq!(comment_rank(""), 0);
        assert_eq!(comment_rank("0x"), 0);
    }

    #[test]
    fn vid_genre_formats_correctly() {
        assert_eq!(vid_genre("deep-house"), "genre:deep-house");
    }

    #[test]
    fn sanitize_genre_slug_lowercases_and_hyphenates_spaces() {
        assert_eq!(sanitize_genre_slug("Deep House"), Some("deep-house".to_string()));
    }

    #[test]
    fn sanitize_genre_slug_passes_through_already_kebab_case() {
        assert_eq!(sanitize_genre_slug("psychedelic-trance"), Some("psychedelic-trance".to_string()));
    }

    #[test]
    fn sanitize_genre_slug_collapses_underscores_and_slashes() {
        assert_eq!(sanitize_genre_slug("lo_fi/chill"), Some("lo-fi-chill".to_string()));
    }

    #[test]
    fn sanitize_genre_slug_drops_punctuation() {
        assert_eq!(sanitize_genre_slug("R&B!!"), Some("r-b".to_string()));
    }

    #[test]
    fn sanitize_genre_slug_collapses_repeated_separators() {
        assert_eq!(sanitize_genre_slug("uk   garage---grime"), Some("uk-garage-grime".to_string()));
    }

    #[test]
    fn sanitize_genre_slug_trims_leading_trailing_separators() {
        assert_eq!(sanitize_genre_slug("  -techno- "), Some("techno".to_string()));
    }

    #[test]
    fn sanitize_genre_slug_rejects_empty_and_punctuation_only() {
        assert_eq!(sanitize_genre_slug(""), None);
        assert_eq!(sanitize_genre_slug("!!!"), None);
        assert_eq!(sanitize_genre_slug("   "), None);
    }

    #[test]
    fn sanitize_genre_slug_caps_at_32_bytes_without_trailing_hyphen() {
        let long = "a-very-long-genre-name-that-goes-on-and-on-and-on";
        let slug = sanitize_genre_slug(long).unwrap();
        assert!(slug.len() <= 32, "slug too long: {slug} ({} bytes)", slug.len());
        assert!(!slug.ends_with('-'), "slug should not end with '-': {slug}");
    }

    #[test]
    fn edge_constants_match_schema() {
        // Smoke-check that the constant values are what the schema expects.
        assert_eq!(EDGE_FOLLOWS, "follows");
        assert_eq!(EDGE_LIKES, "likes");
        assert_eq!(EDGE_PURCHASES, "purchases");
        assert_eq!(EDGE_VIEW_EVENT, "view_event");
        assert_eq!(EDGE_CREATOR_AFFINITY, "creator_affinity");
        assert_eq!(EDGE_RECOMMENDED_TO, "recommended_to");
        assert_eq!(EDGE_COMMENTS_ON, "comments_on");
        assert_eq!(SPACE_THERAGRAPH, "theragraph");
        assert_eq!(PROP_PURCHASED_AT, "purchased_at");
    }
