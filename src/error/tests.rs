use super::*;

    #[test]
    fn test_error_retryable() {
        assert!(Error::PoolExhausted.is_retryable());
        assert!(Error::RateLimited {
            retry_after_ms: 1000
        }
        .is_retryable());
        assert!(!Error::NotFound {
            entity_type: "nft",
            id: "123".to_string()
        }
        .is_retryable());
    }

    #[test]
    fn test_error_status_codes() {
        assert_eq!(
            Error::NotFound {
                entity_type: "nft",
                id: "123".to_string()
            }
            .status_code(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            Error::BadRequest {
                message: "invalid".into()
            }
            .status_code(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            Error::Internal { source: None }.status_code(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    // ---- Constructor tests --------------------------------------------------

    #[test]
    fn config_error_has_correct_code() {
        let err = Error::config("bad");
        assert_eq!(err.error_code(), "CONFIG_ERROR");
    }

    #[test]
    fn not_found_error_includes_entity_and_id() {
        let err = Error::not_found("user", "0x123");
        assert_eq!(err.status_code(), StatusCode::NOT_FOUND);
        let msg = err.to_string();
        assert!(msg.contains("user"), "message should contain entity type");
        assert!(msg.contains("0x123"), "message should contain id");
    }

    #[test]
    fn bad_request_error_status_400() {
        let err = Error::bad_request("invalid");
        assert_eq!(err.status_code(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn database_error_status_500() {
        let err = Error::database("conn failed");
        assert_eq!(err.status_code(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    // ---- Retryability tests -------------------------------------------------

    #[test]
    fn database_error_is_retryable() {
        assert!(Error::database("conn failed").is_retryable());
    }

    #[test]
    fn kafka_error_is_retryable() {
        assert!(Error::kafka("broker unreachable").is_retryable());
    }

    #[test]
    fn config_error_not_retryable() {
        assert!(!Error::config("missing key").is_retryable());
    }

    #[test]
    fn bad_request_not_retryable() {
        assert!(!Error::bad_request("invalid input").is_retryable());
    }

    #[test]
    fn not_found_not_retryable() {
        assert!(!Error::not_found("user", "0xabc").is_retryable());
    }

    // ---- Log-level tests ----------------------------------------------------

    #[test]
    fn config_and_db_are_error_level() {
        // Config is NOT in is_error_level — only Database, Blockchain, Kafka,
        // Internal, and Migration are. Verify the real contract rather than
        // assume config is error-level.
        assert!(Error::database("oops").is_error_level());
        assert!(Error::kafka("oops").is_error_level());
        assert!(Error::blockchain("oops").is_error_level());
        assert!(Error::Internal { source: None }.is_error_level());
        // Config is intentionally NOT error-level per the match arms above.
        assert!(!Error::config("bad key").is_error_level());
    }

    #[test]
    fn bad_request_is_warn_level() {
        // Client errors (4xx) are not logged at error level.
        assert!(!Error::bad_request("invalid").is_error_level());
    }

    // ---- Status-code survey -------------------------------------------------

    #[test]
    fn status_code_mapping_survey() {
        // Spot-check four distinct variants to guard against accidental
        // catch-all regressions in the match arms.
        assert_eq!(
            Error::config("x").status_code(),
            StatusCode::INTERNAL_SERVER_ERROR,
        );
        assert_eq!(
            Error::bad_request("x").status_code(),
            StatusCode::BAD_REQUEST,
        );
        assert_eq!(
            Error::not_found("post", "42").status_code(),
            StatusCode::NOT_FOUND,
        );
        assert_eq!(
            Error::Unauthorized {
                message: "token expired".into()
            }
            .status_code(),
            StatusCode::UNAUTHORIZED,
        );
    }
