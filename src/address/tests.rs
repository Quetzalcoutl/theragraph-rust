    use super::*;

    #[test]
    fn normalizes_mixed_case() {
        let addr: EthAddress = "0xABCD1234abcd1234ABCD1234abcd1234ABCD1234".parse().unwrap();
        assert_eq!(addr.as_str(), "0xabcd1234abcd1234abcd1234abcd1234abcd1234");
    }

    #[test]
    fn rejects_short_address() {
        assert!("0x123".parse::<EthAddress>().is_err());
    }

    #[test]
    fn rejects_no_prefix() {
        assert!("abcd1234abcd1234abcd1234abcd1234abcd1234ab".parse::<EthAddress>().is_err());
    }

    #[test]
    fn cache_keys_are_lowercase() {
        let key = CacheKey::user_prefs("0xABCD1234abcd1234ABCD1234abcd1234ABCD1234");
        assert!(key.contains("0xabcd"));
        assert!(!key.contains("ABCD"));
    }
