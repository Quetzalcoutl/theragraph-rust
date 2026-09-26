    use super::*;

    // ── classify_event_type ─────────────────────────────────────────────────

    #[test]
    fn classify_follow_variants() {
        assert_eq!(classify_event_type("Followed"), Some("follow"));
    }

    #[test]
    fn classify_unfollow_variants() {
        assert_eq!(classify_event_type("Unfollowed"), Some("unfollow"));
    }

    #[test]
    fn classify_like_variants() {
        for ev in &["SnapLiked", "ArtLiked", "MusicLiked", "FlixLiked"] {
            assert_eq!(classify_event_type(ev), Some("like"), "failed for {ev}");
        }
    }

    #[test]
    fn classify_unlike_variants() {
        for ev in &[
            "SnapUnliked",
            "ArtUnliked",
            "MusicUnliked",
            "FlixUnliked",
        ] {
            assert_eq!(classify_event_type(ev), Some("unlike"), "failed for {ev}");
        }
    }

    #[test]
    fn classify_purchase_variants() {
        for ev in &[
            "ContentCopyMinted",
            "SnapBoughtAndMinted",
            "ArtBoughtAndMinted",
            "MusicBoughtAndMinted",
            "FlixBoughtAndMinted",
        ] {
            assert_eq!(classify_event_type(ev), Some("purchase"), "failed for {ev}");
        }
    }

    #[test]
    fn classify_comment_variants() {
        for ev in &[
            "SnapCommented",
            "ArtCommented",
            "MusicCommented",
            "FlixCommented",
        ] {
            assert_eq!(classify_event_type(ev), Some("comment"), "failed for {ev}");
        }
    }

    #[test]
    fn classify_bookmark_share() {
        assert_eq!(classify_event_type("ContentBookmarked"), Some("save"));
        assert_eq!(classify_event_type("ContentShared"), Some("share"));
    }

    #[test]
    fn classify_unknown_returns_none() {
        assert_eq!(classify_event_type("ContentMinted"), None);
        assert_eq!(classify_event_type("UserFollowed_typo"), None);
        assert_eq!(classify_event_type(""), None);
        assert_eq!(classify_event_type("CONTENTLIKED"), None); // case-sensitive
    }

    // ── parse_token_id ──────────────────────────────────────────────────────

    #[test]
    fn parse_token_id_valid_numbers() {
        assert_eq!(parse_token_id("0"), Some(0));
        assert_eq!(parse_token_id("1"), Some(1));
        assert_eq!(parse_token_id("42"), Some(42));
        assert_eq!(parse_token_id("9223372036854775807"), Some(i64::MAX));
    }

    #[test]
    fn parse_token_id_negative() {
        // Negative token IDs are technically valid i64 parses
        assert_eq!(parse_token_id("-1"), Some(-1));
    }

    #[test]
    fn parse_token_id_non_numeric_returns_none() {
        assert_eq!(parse_token_id(""), None);
        assert_eq!(parse_token_id("abc"), None);
        assert_eq!(parse_token_id("1.5"), None);
        assert_eq!(parse_token_id("0x1a"), None); // hex not supported
    }

    #[test]
    fn parse_token_id_overflow_returns_none() {
        // u128::MAX does not fit in i64
        assert_eq!(parse_token_id("99999999999999999999999"), None);
    }

    // ── normalise_address ───────────────────────────────────────────────────

    #[test]
    fn normalise_address_lowercases() {
        assert_eq!(
            normalise_address("0xABCDEF"),
            Some("0xabcdef".to_string())
        );
    }

    #[test]
    fn normalise_address_already_lowercase_unchanged() {
        assert_eq!(
            normalise_address("0xdeadbeef"),
            Some("0xdeadbeef".to_string())
        );
    }

    #[test]
    fn normalise_address_empty_returns_none() {
        assert_eq!(normalise_address(""), None);
        assert_eq!(normalise_address("   "), None);
    }

    // ── extract_follow_addrs ────────────────────────────────────────────────

    #[test]
    fn extract_follow_addrs_happy_path() {
        let data = serde_json::json!({
            "follower": "0xALICE",
            "target":   "0xBOB"
        });
        let (f, t) = extract_follow_addrs(&data).unwrap();
        assert_eq!(f, "0xalice");
        assert_eq!(t, "0xbob");
    }

    #[test]
    fn extract_follow_addrs_missing_follower_returns_none() {
        let data = serde_json::json!({ "target": "0xBOB" });
        assert!(extract_follow_addrs(&data).is_none());
    }

    #[test]
    fn extract_follow_addrs_missing_target_returns_none() {
        let data = serde_json::json!({ "follower": "0xALICE" });
        assert!(extract_follow_addrs(&data).is_none());
    }

    #[test]
    fn extract_follow_addrs_empty_strings_return_none() {
        let data = serde_json::json!({ "follower": "", "target": "0xBOB" });
        assert!(extract_follow_addrs(&data).is_none());
    }

    // ── follow / unfollow score delta arithmetic ────────────────────────────
    // The SQL uses LEAST/GREATEST to clamp.  Mirror the arithmetic here so
    // the test documents the intended semantics regardless of DB.

    fn follow_score(current: f32) -> f32 {
        (current + 0.15_f32).min(0.95)
    }

    fn unfollow_score(current: f32) -> f32 {
        (current - 0.20_f32).max(0.20)
    }

    #[test]
    fn follow_score_increases_and_caps_at_095() {
        assert!((follow_score(0.3) - 0.45).abs() < 1e-6);
        assert!((follow_score(0.85) - 0.95).abs() < 1e-6); // capped
        assert!((follow_score(0.95) - 0.95).abs() < 1e-6); // already at cap
    }

    #[test]
    fn follow_score_never_exceeds_095() {
        // Even starting at 1.0 the cap holds
        assert!(follow_score(1.0) <= 0.95);
    }

    #[test]
    fn unfollow_score_decreases_and_floors_at_020() {
        assert!((unfollow_score(0.5) - 0.30).abs() < 1e-6);
        assert!((unfollow_score(0.3) - 0.20).abs() < 1e-6); // floored
        assert!((unfollow_score(0.1) - 0.20).abs() < 1e-6); // already below floor
    }

    #[test]
    fn unfollow_score_never_goes_below_020() {
        assert!(unfollow_score(0.0) >= 0.20);
    }
