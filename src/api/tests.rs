use super::*;
    use crate::recommendation::scoring::{RecommendationReason, ScoredNft};

    fn make_scored_nft(id: &str) -> ScoredNft {
        ScoredNft {
            nft_id: id.into(),
            token_id: 1,
            contract_address: "0xdeadbeef".into(),
            score: 0.9,
            reason: RecommendationReason::Discovery,
            contract_type: "ERC721".into(),
            creator_address: "0xcafe".into(),
            tags: vec![].into(),
        }
    }

    #[test]
    fn empty_items_returns_zero_total_and_no_more() {
        let resp = build_feed_response(vec![], 20, None);
        assert_eq!(resp.total, 0);
        assert!(resp.items.is_empty());
        assert!(!resp.has_more);
    }

    #[test]
    fn non_empty_items_count_matches_and_items_preserved() {
        let items: Vec<ScoredNft> = (0..3).map(|i| make_scored_nft(&format!("nft-{i}"))).collect();
        let resp = build_feed_response(items, 20, None);
        assert_eq!(resp.total, 3);
        assert_eq!(resp.items.len(), 3);
        assert_eq!(resp.items[0].nft_id.as_ref(), "nft-0");
        assert_eq!(resp.items[2].nft_id.as_ref(), "nft-2");
    }

    #[test]
    fn full_page_infers_has_more_true() {
        let items: Vec<ScoredNft> = (0..5).map(|i| make_scored_nft(&format!("n{i}"))).collect();
        let resp = build_feed_response(items, 5, None);
        assert_eq!(resp.total, 5);
        assert!(resp.has_more, "full page should infer has_more=true");
    }

    #[test]
    fn partial_page_infers_has_more_false() {
        let items: Vec<ScoredNft> = (0..3).map(|i| make_scored_nft(&format!("n{i}"))).collect();
        let resp = build_feed_response(items, 5, None);
        assert!(!resp.has_more, "partial page should infer has_more=false");
    }

    #[test]
    fn has_more_override_true_overrides_inferred_value() {
        let items: Vec<ScoredNft> = (0..2).map(|i| make_scored_nft(&format!("n{i}"))).collect();
        let resp = build_feed_response(items, 20, Some(true));
        assert!(resp.has_more);
    }

    #[test]
    fn has_more_override_false_overrides_inferred_value() {
        let items: Vec<ScoredNft> = (0..5).map(|i| make_scored_nft(&format!("n{i}"))).collect();
        let resp = build_feed_response(items, 5, Some(false));
        assert!(!resp.has_more);
    }

    #[test]
    fn response_total_reflects_actual_item_count_not_limit() {
        let items: Vec<ScoredNft> = (0..7).map(|i| make_scored_nft(&format!("n{i}"))).collect();
        let resp = build_feed_response(items, 20, None);
        assert_eq!(resp.total, 7);
    }

    // -----------------------------------------------------------------------
    // parse_genre_slugs
    // -----------------------------------------------------------------------

    #[test]
    fn parse_genre_slugs_splits_on_comma_and_lowercases() {
        let slugs = parse_genre_slugs("Deep-House,UK-Garage").unwrap();
        assert_eq!(slugs, vec!["deep-house".to_string(), "uk-garage".to_string()]);
    }

    #[test]
    fn parse_genre_slugs_trims_whitespace_around_entries() {
        let slugs = parse_genre_slugs(" deep-house , uk-garage ").unwrap();
        assert_eq!(slugs, vec!["deep-house".to_string(), "uk-garage".to_string()]);
    }

    #[test]
    fn parse_genre_slugs_drops_empty_segments() {
        let slugs = parse_genre_slugs("deep-house,,uk-garage,").unwrap();
        assert_eq!(slugs, vec!["deep-house".to_string(), "uk-garage".to_string()]);
    }

    #[test]
    fn parse_genre_slugs_empty_string_yields_empty_vec() {
        let slugs = parse_genre_slugs("").unwrap();
        assert!(slugs.is_empty());
    }

    #[test]
    fn parse_genre_slugs_at_cap_is_accepted() {
        let raw = (0..MAX_GENRE_SLUGS).map(|i| format!("genre-{i}")).collect::<Vec<_>>().join(",");
        let slugs = parse_genre_slugs(&raw).unwrap();
        assert_eq!(slugs.len(), MAX_GENRE_SLUGS);
    }

    #[test]
    fn parse_genre_slugs_over_cap_is_rejected() {
        let raw = (0..=MAX_GENRE_SLUGS).map(|i| format!("genre-{i}")).collect::<Vec<_>>().join(",");
        assert!(parse_genre_slugs(&raw).is_err(), "should reject overflow, not silently truncate");
    }
