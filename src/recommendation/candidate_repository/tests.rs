use super::*;

    // ── Helpers ───────────────────────────────────────────────────────────────

    /// Mirrors the validation gate inside `list_candidates` without touching
    /// the database.  If this returns `Err`, `list_candidates` would also
    /// return `Err` immediately, before any SQL is executed.
    fn validate_contract_type(ct: Option<&str>) -> Result<()> {
        if let Some(s) = ct {
            if ContentType::from_str(s).is_none() {
                return Err(anyhow::anyhow!("Invalid contract_type: {}", s));
            }
        }
        Ok(())
    }

    // ── Valid inputs ──────────────────────────────────────────────────────────

    #[test]
    fn valid_known_types_pass() {
        for ct in &["snap", "art", "music", "flix"] {
            assert!(
                validate_contract_type(Some(ct)).is_ok(),
                "expected Ok for known type {ct:?}"
            );
        }
    }

    #[test]
    fn valid_known_types_are_case_insensitive() {
        // `list_candidates` passes the raw string to SQL as `$1` but the
        // ContentType guard uses `.to_lowercase()`, so mixed-case strings
        // accepted by the guard will reach the DB with their original casing.
        // This test ensures the guard itself does not reject them.
        for ct in &["SNAP", "Art", "MUSIC", "FLiX"] {
            assert!(
                validate_contract_type(Some(ct)).is_ok(),
                "expected Ok for mixed-case type {ct:?}"
            );
        }
    }

    #[test]
    fn none_filter_is_always_valid() {
        assert!(
            validate_contract_type(None).is_ok(),
            "None filter (fetch all) must not be rejected"
        );
    }

    // ── Invalid inputs ────────────────────────────────────────────────────────

    #[test]
    fn unknown_type_returns_err() {
        let err = validate_contract_type(Some("video"))
            .expect_err("'video' is not a known ContentType and must be rejected");
        let msg = err.to_string();
        assert!(
            msg.contains("Invalid contract_type"),
            "error message should mention 'Invalid contract_type', got: {msg:?}"
        );
        assert!(
            msg.contains("video"),
            "error message should echo the bad value, got: {msg:?}"
        );
    }

    #[test]
    fn empty_string_is_rejected() {
        let err = validate_contract_type(Some(""))
            .expect_err("empty string must be rejected as an invalid ContentType");
        let msg = err.to_string();
        assert!(
            msg.contains("Invalid contract_type"),
            "error message should mention 'Invalid contract_type', got: {msg:?}"
        );
    }

    #[test]
    fn whitespace_string_is_rejected() {
        let err = validate_contract_type(Some("  "))
            .expect_err("whitespace-only string must be rejected as an invalid ContentType");
        let msg = err.to_string();
        assert!(
            msg.contains("Invalid contract_type"),
            "error message should mention 'Invalid contract_type', got: {msg:?}"
        );
    }

    #[test]
    fn close_misspelling_is_rejected() {
        // Guard against fuzzy-matching: "snapp", "artt", etc. must not slip through.
        for bad in &["snapp", "artt", "musics", "flix2", "NFT", "image"] {
            assert!(
                validate_contract_type(Some(bad)).is_err(),
                "close misspelling {bad:?} must be rejected by the validation gate"
            );
        }
    }

    // ── genre_pool_fetch_size (GENRE-03) ──────────────────────────────────────

    #[test]
    fn genre_pool_fetch_size_rounds_small_needs_up_to_pool_size() {
        // A small on-demand request (e.g. a single "radio" slug page) should
        // still trigger a generously-sized fetch so the cached pool is worth
        // sharing with the next caller, not sized to just this one request.
        assert_eq!(genre_pool_fetch_size(20), GENRE_POOL_SIZE);
        assert_eq!(genre_pool_fetch_size(50), GENRE_POOL_SIZE);
    }

    #[test]
    fn genre_pool_fetch_size_never_undersizes_a_large_request() {
        // A request larger than GENRE_POOL_SIZE (e.g. MAX_LIMIT=200 with the
        // 4x fetch multiplier) must get exactly what it needs, never less —
        // undersizing here would silently return fewer genre-matching
        // candidates than a caller asked for.
        let big = GENRE_POOL_SIZE + 500;
        assert_eq!(genre_pool_fetch_size(big), big);
    }

    #[test]
    fn genre_pool_fetch_size_at_exact_boundary_is_pool_size() {
        assert_eq!(genre_pool_fetch_size(GENRE_POOL_SIZE), GENRE_POOL_SIZE);
    }

    #[test]
    fn genre_pool_fetch_size_zero_is_pool_size() {
        assert_eq!(genre_pool_fetch_size(0), GENRE_POOL_SIZE);
    }
