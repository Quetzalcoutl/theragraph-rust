use super::*;
    use super::*;

    #[test]
    fn test_format_address() {
        let addr: Address = "0x1234567890123456789012345678901234567890"
            .parse()
            .unwrap();
        let formatted = format_address(&addr);
        assert!(formatted.contains("..."));
    }

    #[test]
    fn test_decode_uint256() {
        let mut data = vec![0u8; 32];
        data[31] = 42;
        let value = decode_uint256(&data, 0).unwrap();
        assert_eq!(value, U256::from(42));
    }
