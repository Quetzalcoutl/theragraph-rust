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

/// Write the 7 ERC-4337 UserOperation fields shared by the inner hash and the
/// paymaster hash into `buf` starting at offset 0 (7 × 32 bytes).
#[inline]
fn write_user_op_core(buf: &mut [u8], op: &PackedUserOperation) {
    buf[0 * 32..1 * 32].copy_from_slice(&word_address(&op.sender));
    buf[1 * 32..2 * 32].copy_from_slice(&word_u256(&op.nonce));
    buf[2 * 32..3 * 32].copy_from_slice(keccak256(&op.init_code).as_slice());
    buf[3 * 32..4 * 32].copy_from_slice(keccak256(&op.call_data).as_slice());
    buf[4 * 32..5 * 32].copy_from_slice(&word_b32(&op.account_gas_limits));
    buf[5 * 32..6 * 32].copy_from_slice(&word_u256(&op.pre_verification_gas));
    buf[6 * 32..7 * 32].copy_from_slice(&word_b32(&op.gas_fees));
}

pub fn compute_user_op_hash(
    user_op: &PackedUserOperation,
    entry_point: &Address,
    chain_id: u64,
) -> B256 {
    let mut inner = [0u8; 8 * 32];
    write_user_op_core(&mut inner, user_op);
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
    write_user_op_core(&mut buf, user_op);
    buf[7 * 32..8 * 32].copy_from_slice(&word_u64(chain_id));
    buf[8 * 32..9 * 32].copy_from_slice(&word_address(paymaster));
    buf[9 * 32..10 * 32].copy_from_slice(&word_u64(valid_until));
    buf[10 * 32..11 * 32].copy_from_slice(&word_u64(valid_after));
    keccak256(buf)
}


#[cfg(test)]
mod tests;
