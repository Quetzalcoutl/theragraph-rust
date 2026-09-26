    use super::*;
    use alloy::primitives::{Address, Bytes, FixedBytes, U256};
    use super::super::types::PackedUserOperation;

    fn zero_op() -> PackedUserOperation {
        PackedUserOperation {
            sender: Address::ZERO,
            nonce: U256::ZERO,
            init_code: Bytes::default(),
            call_data: Bytes::default(),
            account_gas_limits: FixedBytes::<32>::default(),
            pre_verification_gas: U256::ZERO,
            gas_fees: FixedBytes::<32>::default(),
            paymaster_and_data: Bytes::default(),
            signature: Bytes::default(),
        }
    }

    #[test]
    fn zero_op_hash_is_deterministic() {
        let ep = Address::ZERO;
        let h1 = compute_user_op_hash(&zero_op(), &ep, 1);
        let h2 = compute_user_op_hash(&zero_op(), &ep, 1);
        assert_eq!(h1, h2);
    }

    #[test]
    fn different_chain_id_produces_different_hash() {
        let ep = Address::ZERO;
        let h1 = compute_user_op_hash(&zero_op(), &ep, 1);
        let h2 = compute_user_op_hash(&zero_op(), &ep, 2);
        assert_ne!(h1, h2);
    }

    #[test]
    fn different_nonce_produces_different_hash() {
        let ep = Address::ZERO;
        let mut op1 = zero_op();
        let mut op2 = zero_op();
        op1.nonce = U256::from(0u64);
        op2.nonce = U256::from(1u64);
        let h1 = compute_user_op_hash(&op1, &ep, 1);
        let h2 = compute_user_op_hash(&op2, &ep, 1);
        assert_ne!(h1, h2);
    }

    #[test]
    fn different_entry_point_produces_different_hash() {
        let ep1 = Address::ZERO;
        let ep2 = Address::repeat_byte(0xab);
        let h1 = compute_user_op_hash(&zero_op(), &ep1, 1);
        let h2 = compute_user_op_hash(&zero_op(), &ep2, 1);
        assert_ne!(h1, h2);
    }

    #[test]
    fn paymaster_hash_deterministic() {
        let paymaster = Address::ZERO;
        let h1 = compute_paymaster_hash(&zero_op(), 1, &paymaster, 0, 0);
        let h2 = compute_paymaster_hash(&zero_op(), 1, &paymaster, 0, 0);
        assert_eq!(h1, h2);
    }

    #[test]
    fn paymaster_hash_different_chain_id() {
        let paymaster = Address::ZERO;
        let h1 = compute_paymaster_hash(&zero_op(), 1, &paymaster, 0, 0);
        let h2 = compute_paymaster_hash(&zero_op(), 2, &paymaster, 0, 0);
        assert_ne!(h1, h2);
    }

    #[test]
    fn paymaster_hash_valid_until_affects_result() {
        let paymaster = Address::ZERO;
        let h1 = compute_paymaster_hash(&zero_op(), 1, &paymaster, 0, 0);
        let h2 = compute_paymaster_hash(&zero_op(), 1, &paymaster, 1, 0);
        assert_ne!(h1, h2);
    }
