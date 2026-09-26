use super::*;
    use super::super::EventDispatcher;
    use uuid::Version;

    #[test]
    fn generate_nft_uuid_is_deterministic() {
        let a = EventDispatcher::generate_nft_uuid("0xABCDEF", "42");
        let b = EventDispatcher::generate_nft_uuid("0xABCDEF", "42");
        assert_eq!(a, b);
    }

    #[test]
    fn generate_nft_uuid_lowercases_contract_address() {
        let upper = EventDispatcher::generate_nft_uuid("0xABCDEF", "1");
        let lower = EventDispatcher::generate_nft_uuid("0xabcdef", "1");
        assert_eq!(upper, lower);
    }

    #[test]
    fn generate_nft_uuid_different_contract_yields_different_uuid() {
        let a = EventDispatcher::generate_nft_uuid("0xAAAA", "1");
        let b = EventDispatcher::generate_nft_uuid("0xBBBB", "1");
        assert_ne!(a, b);
    }

    #[test]
    fn generate_nft_uuid_different_token_id_yields_different_uuid() {
        let a = EventDispatcher::generate_nft_uuid("0xAAAA", "1");
        let b = EventDispatcher::generate_nft_uuid("0xAAAA", "2");
        assert_ne!(a, b);
    }

    #[test]
    fn generate_nft_uuid_output_is_version_5() {
        let uuid = EventDispatcher::generate_nft_uuid("0xABCDEF", "99");
        assert_eq!(uuid.get_version(), Some(Version::Sha1));
    }

    // BUG-TAGS-01: genre_slugs is the shared normalizer used by nft_metadata,
    // lookup_nft_with_metadata, and process_enrichment once each stopped
    // reading the nonexistent nfts.tags column and started reading the real
    // genre/genres columns instead. These lock down that both sides of the
    // eventual nft_features.tags <-> tag_preferences array-overlap match
    // agree on the same normalized string form.

    #[test]
    fn genre_slugs_lowercases_and_dedupes_genre_against_genres() {
        // Mirrors the real "Minimalism" / {minimalism} row observed live —
        // singular `genre` and the `genres` array frequently carry the same
        // value in different casing.
        let slugs = genre_slugs(Some("Minimalism"), &["minimalism".to_string()]);
        assert_eq!(slugs, vec!["minimalism".to_string()]);
    }

    #[test]
    fn genre_slugs_preserves_first_seen_order_across_multiple_distinct_genres() {
        let slugs = genre_slugs(
            Some("Psy Trance"),
            &["ambient".to_string(), "psy-trance".to_string()],
        );
        assert_eq!(slugs, vec!["psy-trance".to_string(), "ambient".to_string()]);
    }

    #[test]
    fn genre_slugs_returns_empty_when_both_inputs_are_absent() {
        let slugs = genre_slugs(None, &[]);
        assert!(slugs.is_empty());
    }

    #[test]
    fn genre_slugs_drops_a_genre_that_sanitizes_to_nothing() {
        // sanitize_genre_slug drops non-alphanumeric-only input entirely.
        let slugs = genre_slugs(Some("!!!"), &["rock".to_string()]);
        assert_eq!(slugs, vec!["rock".to_string()]);
    }
