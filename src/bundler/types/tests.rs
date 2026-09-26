    use super::*;
    use alloy::primitives::{Address, Bytes, FixedBytes, U256};

    // ── JsonRpcResponse::ok ────────────────────────────────────────────────────

    #[test]
    fn json_rpc_ok_has_result_no_error() {
        let resp = JsonRpcResponse::ok(Some(serde_json::json!(1)), serde_json::json!("pong"));
        assert_eq!(resp.jsonrpc, "2.0");
        assert_eq!(resp.id, Some(serde_json::json!(1)));
        assert_eq!(resp.result, Some(serde_json::json!("pong")));
        assert!(resp.error.is_none());
    }

    #[test]
    fn json_rpc_ok_null_id() {
        let resp = JsonRpcResponse::ok(None, serde_json::json!(42));
        assert!(resp.id.is_none());
        assert!(resp.result.is_some());
        assert!(resp.error.is_none());
    }

    // ── JsonRpcResponse::err ───────────────────────────────────────────────────

    #[test]
    fn json_rpc_err_has_error_no_result() {
        let resp = JsonRpcResponse::err(Some(serde_json::json!(2)), -32600, "Invalid Request");
        assert_eq!(resp.jsonrpc, "2.0");
        assert_eq!(resp.id, Some(serde_json::json!(2)));
        assert!(resp.result.is_none());
        let err = resp.error.expect("error field must be set");
        assert_eq!(err.code, -32600);
        assert_eq!(err.message, "Invalid Request");
        assert!(err.data.is_none());
    }

    #[test]
    fn json_rpc_err_accepts_string_message() {
        let resp = JsonRpcResponse::err(None, -32000, String::from("server error"));
        let err = resp.error.unwrap();
        assert_eq!(err.message, "server error");
    }

    // ── HexU256 serde ─────────────────────────────────────────────────────────

    #[test]
    fn hex_u256_serialize_zero() {
        let s = serde_json::to_string(&HexU256(U256::ZERO)).unwrap();
        // alloy formats U256::ZERO as "0x0"
        assert_eq!(s, "\"0x0\"");
    }

    #[test]
    fn hex_u256_serialize_nonzero() {
        let s = serde_json::to_string(&HexU256(U256::from(255u64))).unwrap();
        assert_eq!(s, "\"0xff\"");
    }

    #[test]
    fn hex_u256_deserialize_lowercase() {
        let v: HexU256 = serde_json::from_str("\"0x1\"").unwrap();
        assert_eq!(v, HexU256(U256::from(1u64)));
    }

    #[test]
    fn hex_u256_deserialize_uppercase_prefix() {
        let v: HexU256 = serde_json::from_str("\"0XFF\"").unwrap();
        assert_eq!(v, HexU256(U256::from(255u64)));
    }

    #[test]
    fn hex_u256_deserialize_no_prefix() {
        let v: HexU256 = serde_json::from_str("\"ff\"").unwrap();
        assert_eq!(v, HexU256(U256::from(255u64)));
    }

    #[test]
    fn hex_u256_roundtrip_via_json() {
        let original = HexU256(U256::from(0xdeadbeef_u64));
        let serialized = serde_json::to_string(&original).unwrap();
        let deserialized: HexU256 = serde_json::from_str(&serialized).unwrap();
        assert_eq!(deserialized, original);
    }

    #[test]
    fn hex_u256_roundtrip_large_value() {
        // A value that occupies more than one limb
        let big = U256::from(u128::MAX);
        let original = HexU256(big);
        let serialized = serde_json::to_string(&original).unwrap();
        let deserialized: HexU256 = serde_json::from_str(&serialized).unwrap();
        assert_eq!(deserialized, original);
    }

    // ── PackedUserOperation ↔ SerializedUserOp round-trip ─────────────────────

    fn zero_op() -> PackedUserOperation {
        PackedUserOperation {
            sender: Address::ZERO,
            nonce: U256::ZERO,
            init_code: Bytes::new(),
            call_data: Bytes::new(),
            account_gas_limits: FixedBytes::ZERO,
            pre_verification_gas: U256::ZERO,
            gas_fees: FixedBytes::ZERO,
            paymaster_and_data: Bytes::new(),
            signature: Bytes::new(),
        }
    }

    #[test]
    fn packed_user_op_round_trip_zero() {
        let op = zero_op();
        let serialized = SerializedUserOp::from(&op);
        let back = PackedUserOperation::from(serialized);

        assert_eq!(back.sender, op.sender);
        assert_eq!(back.nonce, op.nonce);
        assert_eq!(back.init_code, op.init_code);
        assert_eq!(back.call_data, op.call_data);
        assert_eq!(back.account_gas_limits, op.account_gas_limits);
        assert_eq!(back.pre_verification_gas, op.pre_verification_gas);
        assert_eq!(back.gas_fees, op.gas_fees);
        assert_eq!(back.paymaster_and_data, op.paymaster_and_data);
        assert_eq!(back.signature, op.signature);
    }

    #[test]
    fn packed_user_op_round_trip_nonzero() {
        let mut op = zero_op();
        op.sender = Address::repeat_byte(0xab);
        op.nonce = U256::from(7u64);
        op.call_data = Bytes::from(vec![0x01, 0x02, 0x03]);
        op.pre_verification_gas = U256::from(21_000u64);
        op.signature = Bytes::from(vec![0xff; 65]);

        let serialized = SerializedUserOp::from(&op);
        let back = PackedUserOperation::from(serialized);

        assert_eq!(back.sender, op.sender);
        assert_eq!(back.nonce, op.nonce);
        assert_eq!(back.call_data, op.call_data);
        assert_eq!(back.pre_verification_gas, op.pre_verification_gas);
        assert_eq!(back.signature, op.signature);
    }

    #[test]
    fn serialized_user_op_nonce_is_hex_encoded() {
        let mut op = zero_op();
        op.nonce = U256::from(16u64); // 0x10
        let serialized = SerializedUserOp::from(&op);
        // HexU256 wraps the nonce — confirm it round-trips through JSON correctly
        let json_str = serde_json::to_string(&serialized).unwrap();
        assert!(json_str.contains("0x10"), "nonce should appear as 0x10 in JSON, got: {json_str}");
    }
