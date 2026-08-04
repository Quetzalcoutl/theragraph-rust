// ─── UserOp & Paymaster Hash Computation ──────────────────────────────────────
use alloy::primitives::{keccak256, Address, B256, FixedBytes, U256};

use super::types::PackedUserOperation;

#[inline]
fn word_address(addr: &Address) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[12..].copy_from_slice(addr.as_slice());
    w
}

#[inline]
fn word_u256(n: &U256) -> [u8; 32] {
    n.to_be_bytes()
}

#[inline]
fn word_u64(n: u64) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[24..].copy_from_slice(&n.to_be_bytes());
    w
}

#[inline]
fn word_b32(b: &FixedBytes<32>) -> [u8; 32] {
    *b.as_ref()
}

pub fn compute_user_op_hash(
    user_op: &PackedUserOperation,
    entry_point: &Address,
    chain_id: u64,
) -> B256 {
    let mut inner = [0u8; 8 * 32];
    inner[0 * 32..1 * 32].copy_from_slice(&word_address(&user_op.sender));
    inner[1 * 32..2 * 32].copy_from_slice(&word_u256(&user_op.nonce));
    inner[2 * 32..3 * 32].copy_from_slice(keccak256(&user_op.init_code).as_slice());
    inner[3 * 32..4 * 32].copy_from_slice(keccak256(&user_op.call_data).as_slice());
    inner[4 * 32..5 * 32].copy_from_slice(&word_b32(&user_op.account_gas_limits));
    inner[5 * 32..6 * 32].copy_from_slice(&word_u256(&user_op.pre_verification_gas));
    inner[6 * 32..7 * 32].copy_from_slice(&word_b32(&user_op.gas_fees));
    inner[7 * 32..8 * 32].copy_from_slice(keccak256(&user_op.paymaster_and_data).as_slice());
    let inner_hash = keccak256(inner);

    let mut outer = [0u8; 3 * 32];
    outer[0 * 32..1 * 32].copy_from_slice(inner_hash.as_slice());
    outer[1 * 32..2 * 32].copy_from_slice(&word_address(entry_point));
    outer[2 * 32..3 * 32].copy_from_slice(&word_u64(chain_id));
    keccak256(outer)
}

pub fn compute_paymaster_hash(
    user_op: &PackedUserOperation,
    chain_id: u64,
    paymaster: &Address,
    valid_until: u64,
    valid_after: u64,
) -> B256 {
    let mut buf = [0u8; 11 * 32];
    buf[0 * 32..1 * 32].copy_from_slice(&word_address(&user_op.sender));
    buf[1 * 32..2 * 32].copy_from_slice(&word_u256(&user_op.nonce));
    buf[2 * 32..3 * 32].copy_from_slice(keccak256(&user_op.init_code).as_slice());
    buf[3 * 32..4 * 32].copy_from_slice(keccak256(&user_op.call_data).as_slice());
    buf[4 * 32..5 * 32].copy_from_slice(&word_b32(&user_op.account_gas_limits));
    buf[5 * 32..6 * 32].copy_from_slice(&word_u256(&user_op.pre_verification_gas));
    buf[6 * 32..7 * 32].copy_from_slice(&word_b32(&user_op.gas_fees));
    buf[7 * 32..8 * 32].copy_from_slice(&word_u64(chain_id));
    buf[8 * 32..9 * 32].copy_from_slice(&word_address(paymaster));
    buf[9 * 32..10 * 32].copy_from_slice(&word_u64(valid_until));
    buf[10 * 32..11 * 32].copy_from_slice(&word_u64(valid_after));
    keccak256(buf)
}

#[cfg(test)]
mod tests {
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
}
