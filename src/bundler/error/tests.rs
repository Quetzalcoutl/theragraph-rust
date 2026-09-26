    use super::*;

    // ── BundlerError Display / Debug ─────────────────────────────────────────

    #[test]
    fn failed_op_display_contains_failed_op_and_reason() {
        let err = BundlerError::FailedOp {
            op_index: 0,
            reason: "AA21 didn't pay prefund".to_string(),
        };
        let s = err.to_string();
        assert!(s.contains("FailedOp"), "expected 'FailedOp' in: {s}");
        assert!(s.contains("AA21 didn't pay prefund"), "expected reason in: {s}");
    }

    #[test]
    fn other_display_equals_message() {
        let msg = "rpc connection failed";
        let err = BundlerError::Other(msg.to_string());
        assert_eq!(err.to_string(), msg);
    }

    #[test]
    fn other_implements_std_error() {
        let err = BundlerError::Other("boom".to_string());
        // Calling source() on a bare Error impl returns None — this must not panic.
        let _source = std::error::Error::source(&err);
    }

    #[test]
    fn bundler_error_failed_op_display() {
        let e = BundlerError::FailedOp {
            op_index: 0,
            reason: "AA10 sender already constructed".to_string(),
        };
        let s = e.to_string();
        assert!(s.contains("FailedOp"), "Display should mention FailedOp: {s}");
        assert!(s.contains("0"),        "Display should include op_index: {s}");
        assert!(s.contains("AA10"),     "Display should include reason: {s}");
    }

    #[test]
    fn bundler_error_other_display() {
        let e = BundlerError::Other("RPC timeout".to_string());
        assert_eq!(e.to_string(), "RPC timeout");
    }

    #[test]
    fn bundler_error_failed_op_debug() {
        let e = BundlerError::FailedOp { op_index: 3, reason: "AA25".to_string() };
        let s = format!("{e:?}");
        assert!(s.contains("FailedOp"), "Debug should include variant name: {s}");
        assert!(s.contains("3"),         "Debug should include op_index: {s}");
        assert!(s.contains("AA25"),      "Debug should include reason: {s}");
    }

    #[test]
    fn bundler_error_other_debug() {
        let e = BundlerError::Other("some error".to_string());
        let s = format!("{e:?}");
        assert!(s.contains("Other"),      "Debug should include variant name: {s}");
        assert!(s.contains("some error"), "Debug should include message: {s}");
    }

    #[test]
    fn bundler_error_failed_op_is_error_trait() {
        // Ensure BundlerError implements std::error::Error (required by eyre etc.)
        let e: Box<dyn std::error::Error> = Box::new(BundlerError::FailedOp {
            op_index: 0,
            reason: "AA10".to_string(),
        });
        assert!(e.to_string().contains("AA10"));
    }

    // ── parse_failed_op ──────────────────────────────────────────────────────

    #[test]
    fn parse_failed_op_decoded_format() {
        // alloy "decoded" format: "FailedOp { opIndex: 2, reason: \"AA25 invalid account nonce\" }"
        let msg = r#"FailedOp { opIndex: 2, reason: "AA25 invalid account nonce" }"#;
        let result = parse_failed_op(msg);
        assert!(result.is_ok(), "expected Ok, got {result:?}");
        let (idx, reason) = result.unwrap();
        assert_eq!(idx, 2);
        assert_eq!(reason, "AA25 invalid account nonce");
    }

    #[test]
    fn parse_failed_op_malformed_no_index() {
        // Contains "FailedOp" but no numeric index after the known anchors.
        let msg = "execution reverted: FailedOp(opIndex: , reason: \"AA10\")";
        let result = parse_failed_op(msg);
        assert!(
            result.is_err(),
            "expected Err for malformed input, got {result:?}"
        );
        match result.unwrap_err() {
            BundlerError::Other(s) => assert!(
                s.contains("parse_failed_op"),
                "error message should name the function: {s}"
            ),
            other => panic!("expected BundlerError::Other, got {other:?}"),
        }
    }

    #[test]
    fn parse_failed_op_positional_index_zero() {
        // Raw positional format: "FailedOp(0, \"reason\")"
        let msg = r#"FailedOp(0, "AA10 sender already constructed")"#;
        let (idx, reason) = parse_failed_op(msg).expect("should parse positional FailedOp(0,…)");
        assert_eq!(idx, 0);
        assert_eq!(reason, "AA10 sender already constructed");
    }

    #[test]
    fn parse_failed_op_large_index() {
        // Large op index to confirm usize parsing works for multi-digit values.
        let msg = r#"FailedOp { opIndex: 999, reason: "AA33 reverted" }"#;
        let (idx, reason) = parse_failed_op(msg).expect("should parse large index");
        assert_eq!(idx, 999);
        assert_eq!(reason, "AA33 reverted");
    }

    #[test]
    fn parse_failed_op_revert_prefix_format() {
        // "execution reverted: FailedOp(N, \"AA…\")" — older alloy style
        let msg = r#"execution reverted: FailedOp(3, "AA25 invalid account nonce")"#;
        let (idx, reason) = parse_failed_op(msg).expect("should handle revert-prefix format");
        assert_eq!(idx, 3);
        assert_eq!(reason, "AA25 invalid account nonce");
    }

    #[test]
    fn parse_failed_op_empty_string_returns_not_failed_op() {
        // Empty string has no "FailedOp" → Other("not a FailedOp")
        let err = parse_failed_op("").unwrap_err();
        match err {
            BundlerError::Other(s) => assert_eq!(s, "not a FailedOp"),
            other => panic!("expected Other(\"not a FailedOp\"), got {other:?}"),
        }
    }

    #[test]
    fn parse_failed_op_no_failed_op_keyword() {
        // Generic RPC error — must not parse as FailedOp.
        let msg = "RPC error: connection refused";
        let err = parse_failed_op(msg).unwrap_err();
        match err {
            BundlerError::Other(s) => assert_eq!(s, "not a FailedOp"),
            other => panic!("expected Other(\"not a FailedOp\"), got {other:?}"),
        }
    }

    #[test]
    fn parse_failed_op_malformed_no_parens_or_braces() {
        // "FailedOp" present but nothing parseable follows — no anchors found.
        let msg = "FailedOp something completely unparseable";
        let err = parse_failed_op(msg).unwrap_err();
        match err {
            BundlerError::Other(s) => assert!(
                s.contains("parse_failed_op"),
                "error should name the function: {s}"
            ),
            other => panic!("expected BundlerError::Other, got {other:?}"),
        }
    }

    #[test]
    fn parse_failed_op_reason_with_aa_fallback() {
        // Positional format without `reason:` keyword — falls back to `"AA` scan.
        let msg = r#"FailedOp(1, "AA21 didn't pay prefund")"#;
        let (idx, reason) = parse_failed_op(msg).expect("should fall back to AA-string scan");
        assert_eq!(idx, 1);
        assert_eq!(reason, "AA21 didn't pay prefund");
    }

    #[test]
    fn parse_failed_op_unknown_reason_fallback() {
        // Index found, but no `reason:` and no `"AA` → falls back to "unknown AA error".
        let msg = "FailedOp(5, no-quotes-here)";
        let (idx, reason) = parse_failed_op(msg).expect("should parse index even without reason");
        assert_eq!(idx, 5);
        assert_eq!(reason, "unknown AA error");
    }

    // ── BatchSimOutcome ──────────────────────────────────────────────────────

    #[test]
    fn batch_sim_outcome_ok_debug() {
        let o = BatchSimOutcome::Ok;
        let s = format!("{o:?}");
        assert!(s.contains("Ok"), "Debug of Ok variant: {s}");
    }

    #[test]
    fn batch_sim_outcome_bad_op_fields() {
        let o = BatchSimOutcome::BadOp {
            index:  2,
            reason: "AA25 invalid account nonce".to_string(),
        };
        match o {
            BatchSimOutcome::BadOp { index, reason } => {
                assert_eq!(index, 2);
                assert_eq!(reason, "AA25 invalid account nonce");
            }
            other => panic!("expected BadOp, got {other:?}"),
        }
    }

    #[test]
    fn batch_sim_outcome_rpc_error_carries_message() {
        let o = BatchSimOutcome::RpcError("connection refused".to_string());
        match o {
            BatchSimOutcome::RpcError(msg) => assert_eq!(msg, "connection refused"),
            other => panic!("expected RpcError, got {other:?}"),
        }
    }
