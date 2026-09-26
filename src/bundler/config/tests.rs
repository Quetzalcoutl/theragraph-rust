    use super::*;

    // --- normalize_hex_key ---

    #[test]
    fn normalize_hex_key_strips_lowercase_0x_prefix() {
        let raw = "0xdeadbeef";
        assert_eq!(normalize_hex_key(raw), "0xdeadbeef");
    }

    #[test]
    fn normalize_hex_key_strips_uppercase_0x_prefix() {
        let raw = "0Xdeadbeef";
        // Already has 0X prefix — returned unchanged
        assert_eq!(normalize_hex_key(raw), "0Xdeadbeef");
    }

    #[test]
    fn normalize_hex_key_prepends_0x_when_absent() {
        let raw = "deadbeef1234567890abcdef";
        let result = normalize_hex_key(raw);
        assert_eq!(result, "0xdeadbeef1234567890abcdef");
        assert!(result.starts_with("0x"));
    }

    #[test]
    fn normalize_hex_key_handles_empty_string() {
        let result = normalize_hex_key("");
        assert_eq!(result, "0x");
    }

    #[test]
    fn normalize_hex_key_idempotent_with_0x_prefix() {
        let raw = "0xabcdef";
        // Calling twice should give the same result as calling once
        assert_eq!(normalize_hex_key(&normalize_hex_key(raw)), normalize_hex_key(raw));
    }

    // --- parse_address ---

    #[test]
    fn parse_address_accepts_valid_checksummed_address() {
        let addr = parse_address("0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045");
        assert!(addr.is_ok());
    }

    #[test]
    fn parse_address_accepts_lowercase_address_with_prefix() {
        let addr = parse_address("0xd8da6bf26964af9d7eed9e03e53415d37aa96045");
        assert!(addr.is_ok());
    }

    #[test]
    fn parse_address_accepts_address_without_0x_prefix() {
        let addr = parse_address("d8dA6BF26964aF9D7eEd9e03E53415D37aA96045");
        assert!(addr.is_ok());
    }

    #[test]
    fn parse_address_rejects_address_too_short() {
        let addr = parse_address("0x1234");
        assert!(addr.is_err());
    }

    #[test]
    fn parse_address_rejects_non_hex_string() {
        let addr = parse_address("not-an-address");
        assert!(addr.is_err());
    }

    #[test]
    fn parse_address_rejects_empty_string() {
        let addr = parse_address("");
        assert!(addr.is_err());
    }

    #[test]
    fn parse_address_well_known_entrypoint() {
        // The ERC-4337 entry point used in Config::from_env
        let addr = parse_address("0x0000000071727De22E5E9d8BAf0edAc6f37da032");
        assert!(addr.is_ok());
        let a = addr.unwrap();
        // Confirm the Address round-trips to the same hex representation (lowercase)
        let s = format!("{a:?}");
        assert!(s.to_lowercase().contains("0000000071727de22e5e9d8baf0edac6f37da032"));
    }
