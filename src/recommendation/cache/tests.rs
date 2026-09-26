use super::*;
    use super::super::graph_client::FofBucket;

    // ── fof_bucket ───────────────────────────────────────────────────────────

    #[test]
    fn cache_key_fof_bucket_lowercases_address() {
        assert_eq!(
            CacheKey::fof_bucket(FofBucket::FollowLike, "0xABCDEF1234567890ABCDEF1234567890ABCDEF12"),
            "rec:fof:v2:0xabcdef1234567890abcdef1234567890abcdef12"
        );
    }

    #[test]
    fn cache_key_fof_bucket_already_lowercase_is_identity() {
        let addr = "0xabcdef1234567890abcdef1234567890abcdef12";
        assert_eq!(CacheKey::fof_bucket(FofBucket::FollowLike, addr), format!("rec:fof:v2:{addr}"));
    }

    #[test]
    fn cache_key_fof_bucket_short_address_still_lowercased() {
        assert_eq!(CacheKey::fof_bucket(FofBucket::FollowLike, "0xABC"), "rec:fof:v2:0xabc");
    }

    // ── nebula ───────────────────────────────────────────────────────────────

    #[test]
    fn cache_key_nebula_has_correct_prefix() {
        let hash = "QmHash123AbcDef";
        let key = CacheKey::nebula(hash);
        assert!(
            key.starts_with("rec:nebula:"),
            "expected key to start with 'rec:nebula:', got: {key}"
        );
    }

    #[test]
    fn cache_key_nebula_preserves_hash_casing() {
        // Nebula keys are keyed by FNV hash (hex string), not user addresses.
        // The hash is already lowercase hex so casing must be preserved as-is.
        let hash = "a1b2c3d4e5f60000";
        assert_eq!(CacheKey::nebula(hash), format!("rec:nebula:{hash}"));
    }

    // ── features ─────────────────────────────────────────────────────────────

    #[test]
    fn cache_key_features_has_correct_prefix() {
        let nft_id = "42";
        let key = CacheKey::features(nft_id);
        // Wire format v3 (MessagePack) so old JSON/v2 entries are isolated
        assert!(
            key.starts_with("rec:features:v3:"),
            "expected key to start with 'rec:features:v3:', got: {key}"
        );
    }

    #[test]
    fn cache_key_features_appends_nft_id() {
        // Wire format v3
        assert_eq!(CacheKey::features("42"), "rec:features:v3:42");
        assert_eq!(CacheKey::features("99999"), "rec:features:v3:99999");
    }

    // ── prefs ─────────────────────────────────────────────────────────────────

    #[test]
    fn cache_key_prefs_lowercases_address() {
        assert_eq!(
            CacheKey::prefs("0xDEF0DEF0DEF0DEF0DEF0DEF0DEF0DEF0DEF0DEF0"),
            "rec:prefs:v2:0xdef0def0def0def0def0def0def0def0def0def0"
        );
    }

    #[test]
    fn cache_key_prefs_has_correct_prefix() {
        let key = CacheKey::prefs("0xaabbccddaabbccddaabbccddaabbccddaabbccdd");
        assert!(
            key.starts_with("rec:prefs:"),
            "expected key to start with 'rec:prefs:', got: {key}"
        );
    }

    // ── recs ─────────────────────────────────────────────────────────────────

    #[test]
    fn cache_key_recs_lowercases_address_and_appends_feed_type() {
        let addr = "0xABCDABCDABCDABCDABCDABCDABCDABCDABCDABCD";
        let key = CacheKey::recs(addr, "trending");
        // Prefix is "rec:results:v2:" per PREFIX_RECS constant
        assert_eq!(
            key,
            "rec:results:v2:0xabcdabcdabcdabcdabcdabcdabcdabcdabcdabcd:trending"
        );
    }

    #[test]
    fn cache_key_recs_different_feed_types_produce_different_keys() {
        let addr = "0x1234567890123456789012345678901234567890";
        let key_personalized = CacheKey::recs(addr, "personalized");
        let key_trending = CacheKey::recs(addr, "trending");
        let key_following = CacheKey::recs(addr, "following");

        assert_ne!(key_personalized, key_trending);
        assert_ne!(key_trending, key_following);
        assert_ne!(key_personalized, key_following);
    }

    // ── following ────────────────────────────────────────────────────────────

    #[test]
    fn cache_key_following_lowercases_address() {
        let addr = "0xGHIGHIGHIGHIGHIGHIGHIGHIGHIGHIGHIGHIGHI";
        // Even though 'G', 'H', 'I' are not valid hex — the function lowercases
        // the raw string regardless. Validation is the caller's responsibility.
        let key = CacheKey::following(addr);
        assert_eq!(
            key,
            format!("rec:following:v2:{}", addr.to_lowercase())
        );
    }

    #[test]
    fn cache_key_following_has_correct_prefix() {
        let key = CacheKey::following("0xf0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0");
        assert!(
            key.starts_with("rec:following:"),
            "expected key to start with 'rec:following:', got: {key}"
        );
    }

    // ── seen ─────────────────────────────────────────────────────────────────

    #[test]
    fn cache_key_seen_lowercases_address() {
        let addr = "0xJKLJKLJKLJKLJKLJKLJKLJKLJKLJKLJKLJKLJKL";
        let key = CacheKey::seen(addr);
        assert_eq!(key, format!("rec:seen:{}", addr.to_lowercase()));
    }

    #[test]
    fn cache_key_seen_has_correct_prefix() {
        let key = CacheKey::seen("0xdeaddeaddeaddeaddeaddeaddeaddeaddeaddead");
        assert!(
            key.starts_with("rec:seen:"),
            "expected key to start with 'rec:seen:', got: {key}"
        );
    }

    // ── namespace isolation ──────────────────────────────────────────────────

    #[test]
    fn all_key_prefixes_are_distinct() {
        // Each namespace must map to a unique prefix so a SCAN or key-pattern
        // operation on one namespace never bleeds into another.
        let addr = "0xaaaa000000000000000000000000000000000000";
        let hash = "0000000000000000";
        let nft_id = "1";
        let feed = "trending";

        let keys = vec![
            CacheKey::fof_bucket(FofBucket::FollowLike, addr),
            CacheKey::nebula(hash),
            CacheKey::features(nft_id),
            CacheKey::prefs(addr),
            CacheKey::recs(addr, feed),
            CacheKey::following(addr),
            CacheKey::seen(addr),
            CacheKey::board("trending"),
            CacheKey::session(addr),
            CacheKey::fof_bucket(FofBucket::ViewEvent, addr),
            CacheKey::fof_bucket(FofBucket::Comment, addr),
            CacheKey::fof_bucket(FofBucket::Purchase, addr),
            CacheKey::fof_bucket(FofBucket::FlixWatch, addr),
            CacheKey::user_suggestions(addr, addr),
        ];

        // Extract the namespace prefix (e.g. "rec:fof:", "board:").
        // Strategy: take everything up to and including the second colon when
        // there are two colons; if there is only one colon (e.g. "board:foo"),
        // take up to and including the first colon.
        let prefixes: Vec<&str> = keys.iter().map(|k| {
            // Position of the first colon (all our keys have at least one).
            let first_colon = k.find(':').unwrap_or(k.len());
            // Look for a second colon after the first.
            let second_colon = k[first_colon + 1..]
                .find(':')
                .map(|i| first_colon + 1 + i);
            match second_colon {
                Some(pos) => &k[..=pos],        // includes the second colon
                None      => &k[..=first_colon], // only one colon (e.g. "board:")
            }
        }).collect();

        let unique: std::collections::HashSet<&str> = prefixes.iter().copied().collect();
        assert_eq!(
            unique.len(),
            prefixes.len(),
            "duplicate prefix detected among CacheKey constructors: {:?}",
            prefixes
        );
    }

    // ── board ─────────────────────────────────────────────────────────────────

    #[test]
    fn cache_key_board_has_correct_prefix() {
        let key = CacheKey::board("trending");
        assert!(
            key.starts_with("board:"),
            "expected key to start with 'board:', got: {key}"
        );
    }

    #[test]
    fn cache_key_board_appends_key_verbatim() {
        // Board keys are opaque slugs (not addresses), so no lowercasing is applied.
        assert_eq!(CacheKey::board("trending"), "board:v2:trending");
        assert_eq!(CacheKey::board("user:0xABCD:posts"), "board:v2:user:0xABCD:posts");
    }

    // ── empty-string inputs ──────────────────────────────────────────────────

    #[test]
    fn cache_key_fof_bucket_empty_string_does_not_panic() {
        let key = CacheKey::fof_bucket(FofBucket::FollowLike, "");
        assert_eq!(key, "rec:fof:v2:");
    }

    #[test]
    fn cache_key_nebula_empty_string_does_not_panic() {
        let key = CacheKey::nebula("");
        assert_eq!(key, "rec:nebula:");
    }

    #[test]
    fn cache_key_features_empty_string_does_not_panic() {
        let key = CacheKey::features("");
        // Wire format v3
        assert_eq!(key, "rec:features:v3:");
    }

    #[test]
    fn cache_key_prefs_empty_string_does_not_panic() {
        let key = CacheKey::prefs("");
        assert_eq!(key, "rec:prefs:v2:");
    }

    #[test]
    fn cache_key_following_empty_string_does_not_panic() {
        let key = CacheKey::following("");
        assert_eq!(key, "rec:following:v2:");
    }

    #[test]
    fn cache_key_seen_empty_string_does_not_panic() {
        let key = CacheKey::seen("");
        assert_eq!(key, "rec:seen:");
    }

    #[test]
    fn cache_key_recs_empty_strings_do_not_panic() {
        let key = CacheKey::recs("", "");
        assert_eq!(key, "rec:results:v2::");
    }

    #[test]
    fn cache_key_board_empty_string_does_not_panic() {
        let key = CacheKey::board("");
        assert_eq!(key, "board:v2:");
    }

    // ── genre_feed_type (GENRE-02 follow-up) ────────────────────────────────

    #[test]
    fn genre_feed_type_has_correct_prefix() {
        let ft = genre_feed_type(&["deep-house".to_string()]);
        assert!(ft.starts_with("genre:"), "expected 'genre:' prefix, got: {ft}");
    }

    #[test]
    fn genre_feed_type_fits_varchar_20() {
        // recommendation_cache.feed_type is VARCHAR(20) — an oversized value
        // would fail the INSERT with a Postgres error, not degrade gracefully.
        for slugs in [
            vec!["deep-house".to_string()],
            vec!["a".to_string(); 30],
            (0..30).map(|i| format!("genre-{i}")).collect(),
        ] {
            let ft = genre_feed_type(&slugs);
            assert!(ft.len() <= 20, "genre_feed_type {ft:?} (len {}) exceeds VARCHAR(20)", ft.len());
        }
    }

    #[test]
    fn genre_feed_type_is_order_independent() {
        let a = genre_feed_type(&["deep-house".to_string(), "uk-garage".to_string()]);
        let b = genre_feed_type(&["uk-garage".to_string(), "deep-house".to_string()]);
        assert_eq!(a, b, "same slug set in a different order must hash to the same key");
    }

    #[test]
    fn genre_feed_type_is_case_independent() {
        let a = genre_feed_type(&["Deep-House".to_string()]);
        let b = genre_feed_type(&["deep-house".to_string()]);
        assert_eq!(a, b, "casing differences must not change the key");
    }

    #[test]
    fn genre_feed_type_trims_whitespace() {
        let a = genre_feed_type(&[" deep-house ".to_string()]);
        let b = genre_feed_type(&["deep-house".to_string()]);
        assert_eq!(a, b);
    }

    #[test]
    fn genre_feed_type_dedups() {
        let a = genre_feed_type(&["deep-house".to_string(), "deep-house".to_string()]);
        let b = genre_feed_type(&["deep-house".to_string()]);
        assert_eq!(a, b, "a duplicated slug must not change the key");
    }

    #[test]
    fn genre_feed_type_distinct_slug_sets_produce_distinct_keys() {
        let a = genre_feed_type(&["deep-house".to_string()]);
        let b = genre_feed_type(&["uk-garage".to_string()]);
        let c = genre_feed_type(&["deep-house".to_string(), "uk-garage".to_string()]);
        assert_ne!(a, b);
        assert_ne!(a, c);
        assert_ne!(b, c);
    }

    #[test]
    fn genre_feed_type_no_separator_collision() {
        // ["ab", "c"] and ["a", "bc"] must not collide despite concatenating
        // to the same bytes without a separator.
        let a = genre_feed_type(&["ab".to_string(), "c".to_string()]);
        let b = genre_feed_type(&["a".to_string(), "bc".to_string()]);
        assert_ne!(a, b, "slug sets must not collide across a naive concatenation boundary");
    }

    #[test]
    fn genre_feed_type_is_deterministic() {
        let slugs = vec!["deep-house".to_string(), "techno".to_string()];
        assert_eq!(genre_feed_type(&slugs), genre_feed_type(&slugs));
    }

    #[test]
    fn genre_feed_type_empty_slice_does_not_panic() {
        let ft = genre_feed_type(&[]);
        assert!(ft.starts_with("genre:"));
    }

    #[test]
    fn genre_feed_type_all_empty_or_whitespace_slugs_normalizes_to_empty_set() {
        // Blank entries are filtered out before hashing — an all-blank input
        // must hash the same as an actually-empty slice.
        let a = genre_feed_type(&["".to_string(), "   ".to_string()]);
        let b = genre_feed_type(&[]);
        assert_eq!(a, b);
    }

    // ── genre_pool (GENRE-03) ────────────────────────────────────────────────

    #[test]
    fn cache_key_genre_pool_has_correct_prefix() {
        let hash = genre_feed_type(&["deep-house".to_string()]);
        let key = CacheKey::genre_pool(&hash);
        assert!(
            key.starts_with("rec:genre_pool:v1:"),
            "expected key to start with 'rec:genre_pool:v1:', got: {key}"
        );
    }

    #[test]
    fn cache_key_genre_pool_is_distinct_from_recs_key_for_the_same_hash() {
        // The shared candidate-pool cache and the per-user scored-results
        // cache must never collide even though both key off the same hash —
        // one holds raw unscored candidates, the other a fully personalized
        // per-user output.
        let hash = genre_feed_type(&["deep-house".to_string()]);
        let pool_key = CacheKey::genre_pool(&hash);
        let recs_key = CacheKey::recs("0xabc", &hash);
        assert_ne!(pool_key, recs_key);
    }

    #[test]
    fn cache_key_genre_pool_different_hashes_produce_different_keys() {
        let hash_a = genre_feed_type(&["deep-house".to_string()]);
        let hash_b = genre_feed_type(&["uk-garage".to_string()]);
        assert_ne!(CacheKey::genre_pool(&hash_a), CacheKey::genre_pool(&hash_b));
    }

    // ── topic_affinity ───────────────────────────────────────────────────────

    #[test]
    fn cache_key_topic_affinity_has_correct_prefix() {
        let addr = "0xABCDEF1234567890ABCDEF1234567890ABCDEF12";
        let tag = "GenerativeArt";
        let key = CacheKey::topic_affinity(addr, tag);
        assert!(
            key.starts_with("topic_affinity:"),
            "expected key to start with 'topic_affinity:', got: {key}"
        );
        // Both components must be lowercased for key stability.
        assert_eq!(
            key,
            "topic_affinity:0xabcdef1234567890abcdef1234567890abcdef12:generativeart"
        );
    }

    #[test]
    fn cache_key_topic_affinity_lowercases_addr_and_tag() {
        let key_upper = CacheKey::topic_affinity("0xABCD", "ART");
        let key_lower = CacheKey::topic_affinity("0xabcd", "art");
        assert_eq!(key_upper, key_lower, "mixed-case and lowercase inputs must produce the same key");
    }

    // ── TTL ordering sanity ──────────────────────────────────────────────────

    #[test]
    fn ttl_nebula_query_is_shorter_than_recommendation() {
        // Nebula graph traversals re-run on a tighter cadence than cached recs.
        assert!(
            NEBULA_QUERY_TTL < RECOMMENDATION_TTL,
            "NEBULA_QUERY_TTL ({NEBULA_QUERY_TTL}s) should be < RECOMMENDATION_TTL ({RECOMMENDATION_TTL}s)"
        );
    }

    #[test]
    fn ttl_recommendation_is_shorter_than_nft_features() {
        // Recommendation lists are volatile relative to feature vectors.
        assert!(
            RECOMMENDATION_TTL < NFT_FEATURES_TTL,
            "RECOMMENDATION_TTL ({RECOMMENDATION_TTL}s) should be < NFT_FEATURES_TTL ({NFT_FEATURES_TTL}s)"
        );
    }

    #[test]
    fn ttl_all_values_are_positive() {
        assert!(NEBULA_QUERY_TTL > 0);
        assert!(NFT_FEATURES_TTL > 0);
        assert!(USER_PREFS_TTL > 0);
        assert!(FOLLOWING_TTL > 0);
        assert!(SEEN_NFTS_TTL > 0);
        assert!(RECOMMENDATION_TTL > 0);
    }

    // ── hash_query determinism ────────────────────────────────────────────────

    #[test]
    fn hash_query_is_deterministic() {
        let q = "MATCH (n:User)-[:FOLLOWS]->(m) WHERE n.addr == '0xabc' RETURN m LIMIT 100";
        assert_eq!(RecCache::hash_query(q), RecCache::hash_query(q));
    }

    #[test]
    fn hash_query_different_inputs_produce_different_hashes() {
        let h1 = RecCache::hash_query("SELECT * FROM foo WHERE id = 1");
        let h2 = RecCache::hash_query("SELECT * FROM foo WHERE id = 2");
        assert_ne!(h1, h2);
    }

    #[test]
    fn hash_query_output_is_16_hex_chars() {
        let h = RecCache::hash_query("any query text here");
        assert_eq!(h.len(), 16, "expected 16-char hex string, got: {h}");
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()), "non-hex char in: {h}");
    }

    #[test]
    fn hash_query_empty_string_does_not_panic() {
        let h = RecCache::hash_query("");
        // FNV-1a of the empty string is the offset basis itself.
        assert_eq!(h.len(), 16);
    }

    // ── serde roundtrip ───────────────────────────────────────────────────────

    #[test]
    fn scored_nft_empty_tags_roundtrip() {
        use crate::recommendation::scoring::{RecommendationReason, ScoredNft};
        let original = ScoredNft {
            nft_id: "x".into(),
            token_id: 1,
            contract_address: "0x0".into(),
            score: 0.5,
            reason: RecommendationReason::Discovery,
            contract_type: "art".into(),
            creator_address: "0x0".into(),
            tags: vec![].into(),
        };
        let json = serde_json::to_string(&original).expect("should serialize");
        let decoded: ScoredNft = serde_json::from_str(&json)
            .expect("empty tags must round-trip — omitted field needs #[serde(default)]");
        assert!(decoded.tags.is_empty());
    }

    #[test]
    fn scored_nft_empty_tags_roundtrip_via_reccache_wire_format() {
        // Exercises the actual RecCache::encode/decode path (MessagePack, named-field
        // mode) rather than raw serde_json — this is what Redis/Postgres actually
        // store. Guards the same #[serde(default)] requirement as the JSON-level
        // test above, but against the real wire format used in production.
        use crate::recommendation::scoring::{RecommendationReason, ScoredNft};
        let original = ScoredNft {
            nft_id: "x".into(),
            token_id: 1,
            contract_address: "0x0".into(),
            score: 0.5,
            reason: RecommendationReason::Discovery,
            contract_type: "art".into(),
            creator_address: "0x0".into(),
            tags: vec![].into(),
        };
        let encoded = RecCache::encode(&original).expect("should encode");
        let decoded: ScoredNft = RecCache::decode(&encoded)
            .expect("empty tags must round-trip through the real wire format");
        assert!(decoded.tags.is_empty());
        assert_eq!(decoded.nft_id, original.nft_id);
    }

    #[test]
    fn scored_nft_batch_roundtrip_via_reccache_wire_format() {
        // Same wire-format path as above, but a mixed batch (some empty tags, some
        // not) — approximates what get_recommendations/set_recommendations actually
        // stores (Vec<ScoredNft>).
        use crate::recommendation::scoring::{RecommendationReason, ScoredNft};
        let batch: Vec<ScoredNft> = (0..5).map(|i| ScoredNft {
            nft_id: format!("nft-{i}").into(),
            token_id: i,
            contract_address: "0xabc".into(),
            score: i as f32 * 0.1,
            reason: RecommendationReason::Discovery,
            contract_type: "art".into(),
            creator_address: "0xdef".into(),
            tags: if i % 2 == 0 { vec![].into() } else { vec!["tag".into()].into() },
        }).collect();

        let encoded = RecCache::encode(&batch).expect("should encode");
        let decoded: Vec<ScoredNft> = RecCache::decode(&encoded).expect("should decode");
        assert_eq!(decoded.len(), 5);
        for (orig, dec) in batch.iter().zip(decoded.iter()) {
            assert_eq!(orig.nft_id, dec.nft_id);
            assert_eq!(orig.tags, dec.tags);
        }
    }

    #[test]
    fn scored_nft_serde_roundtrip() {
        use crate::recommendation::scoring::{RecommendationReason, ScoredNft};

        let original = ScoredNft {
            nft_id: "nft-abc-123".into(),
            token_id: 42,
            contract_address: "0x1111111111111111111111111111111111111111".into(),
            score: 0.875_f32,
            reason: RecommendationReason::TagMatch {
                matching_tags: vec!["art".into(), "generative".into()].into(),
            },
            contract_type: "ERC721".into(),
            creator_address: "0x2222222222222222222222222222222222222222".into(),
            tags: vec!["art".into(), "generative".into()].into(),
        };

        let json = serde_json::to_string(&original).expect("ScoredNft should serialize");
        let decoded: ScoredNft =
            serde_json::from_str(&json).expect("ScoredNft should deserialize");

        assert_eq!(decoded.nft_id, original.nft_id);
        assert_eq!(decoded.token_id, original.token_id);
        assert_eq!(decoded.contract_address, original.contract_address);
        // f32 round-trips through JSON as a float literal; compare via absolute diff.
        assert!(
            (decoded.score - original.score).abs() < 1e-6,
            "score mismatch after roundtrip: {} vs {}",
            decoded.score,
            original.score
        );
        assert_eq!(decoded.contract_type, original.contract_type);
        assert_eq!(decoded.creator_address, original.creator_address);
        assert_eq!(decoded.tags, original.tags);
    }

    #[test]
    fn recommendation_reason_trending_serde_roundtrip() {
        use crate::recommendation::scoring::RecommendationReason;

        let reason = RecommendationReason::Trending { trending_score: 0.95 };
        let json = serde_json::to_string(&reason).expect("serialize");
        let decoded: RecommendationReason = serde_json::from_str(&json).expect("deserialize");

        // serde_json::to_string on both sides gives canonical form for comparison.
        assert_eq!(
            serde_json::to_string(&decoded).unwrap(),
            serde_json::to_string(&reason).unwrap()
        );
    }

    #[test]
    fn fof_vec_tuple_serde_roundtrip() {
        // FoF caches store Vec<(String, f32)> — scores are cast from f64 at write time
        // (set_fof_recommendations) and returned as f32 at read time.
        // f32 is sufficient: the scoring engine (apply_cache_boosts) uses f32 throughout.
        let recs: Vec<(String, f32)> = vec![
            ("0xaaaa".to_string(), 0.9_f32),
            ("0xbbbb".to_string(), 0.5_f32),
            ("0xcccc".to_string(), 0.1_f32),
        ];

        let json = serde_json::to_string(&recs).expect("serialize");
        let decoded: Vec<(String, f32)> = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(decoded.len(), recs.len());
        for ((addr_orig, score_orig), (addr_dec, score_dec)) in recs.iter().zip(decoded.iter()) {
            assert_eq!(addr_orig, addr_dec);
            assert!((score_orig - score_dec).abs() < f32::EPSILON);
        }
    }

    // ── FoF sub-bucket key distinctness ─────────────────────────────────────

    /// All FofBucket variants produce distinct keys for the same address.
    /// A new bucket added to FofBucket::all() is automatically tested here.
    #[test]
    fn fof_sub_bucket_keys_are_distinct_for_same_address() {
        let addr = "0xaaaa000000000000000000000000000000000000";

        let keys: Vec<String> = FofBucket::all()
            .iter()
            .map(|b| CacheKey::fof_bucket(*b, addr))
            .collect();

        for (i, a) in keys.iter().enumerate() {
            for (j, b) in keys.iter().enumerate() {
                if i != j {
                    assert_ne!(a, b, "FofBucket variant {i} collides with variant {j}: {a}");
                }
            }
        }

        // Spot-check exact prefixes so a rename is caught immediately.
        assert_eq!(CacheKey::fof_bucket(FofBucket::FollowLike, addr), format!("rec:fof:v2:{addr}"));
        assert_eq!(CacheKey::fof_bucket(FofBucket::ViewEvent,  addr), format!("rec:fof_view:v2:{addr}"));
        assert_eq!(CacheKey::fof_bucket(FofBucket::Comment,    addr), format!("rec:fof_comment:v2:{addr}"));
        assert_eq!(CacheKey::fof_bucket(FofBucket::Purchase,   addr), format!("rec:fof_purchase:v2:{addr}"));
        assert_eq!(CacheKey::fof_bucket(FofBucket::Share,      addr), format!("rec:fof_share:v2:{addr}"));
        assert_eq!(CacheKey::fof_bucket(FofBucket::Bookmark,   addr), format!("rec:fof_bookmark:v2:{addr}"));
    }

    #[test]
    fn user_suggestions_key_uses_dedicated_prefix() {
        let viewer  = "0xaaaa000000000000000000000000000000000000";
        let creator = "0xbbbb000000000000000000000000000000000000";
        let key = CacheKey::user_suggestions(viewer, creator);

        // Must use the dedicated suggestions namespace, not the fof namespace.
        assert!(
            key.starts_with("rec:suggestions:"),
            "user_suggestions key must start with 'rec:suggestions:', got: {key}"
        );
        // Must not alias any fof bucket key for the same viewer address.
        assert_ne!(key, CacheKey::fof_bucket(FofBucket::FollowLike, viewer), "must not collide with fof key");
        assert_ne!(key, CacheKey::fof_bucket(FofBucket::ViewEvent,  viewer), "must not collide with fof_view key");
        assert_ne!(key, CacheKey::fof_bucket(FofBucket::Comment,    viewer), "must not collide with fof_comment key");
    }

    #[test]
    fn user_suggestions_key_is_scoped_by_both_viewer_and_creator() {
        let v1 = "0xaaaa000000000000000000000000000000000000";
        let v2 = "0xbbbb000000000000000000000000000000000000";
        let c1 = "0xcccc000000000000000000000000000000000000";
        let c2 = "0xdddd000000000000000000000000000000000000";

        // Same viewer, different creator → different keys.
        assert_ne!(CacheKey::user_suggestions(v1, c1), CacheKey::user_suggestions(v1, c2));
        // Different viewer, same creator → different keys.
        assert_ne!(CacheKey::user_suggestions(v1, c1), CacheKey::user_suggestions(v2, c1));
        // Swapped viewer/creator → different key (order matters).
        assert_ne!(CacheKey::user_suggestions(v1, c1), CacheKey::user_suggestions(c1, v1));
    }

    // ── compile-time guarantee note ──────────────────────────────────────────

    // There are no hardcoded prefix strings scattered in the rest of the codebase.
    // All Redis keys for the recommendation namespace flow through CacheKey::*.
    // This is a compile-time property: the PREFIX_* constants are private (no `pub`)
    // and CacheKey is the only module that imports them. Any new key constructor
    // must be added here, preventing silent mismatches. No runtime test can verify
    // a compile-time invariant — this comment documents it instead.
