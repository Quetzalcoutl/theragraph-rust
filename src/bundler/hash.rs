// ─── UserOp & Paymaster Hash Computation ──────────────────────────────────────
//
// Both hash functions must reproduce the on-chain logic byte-for-byte.
//
// Architecture note (Boris Staal / Parity Technologies):
//   No allocator pressure — all intermediate buffers are stack-allocated
//   fixed-size arrays.  keccak256 is applied once per digest needed.

use alloy::primitives::{keccak256, Address, B256, FixedBytes, U256};

use super::types::PackedUserOperation;

// ─── ABI encoding helpers (static, no heap) ───────────────────────────────────

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
fn word_u32(n: u32) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[28..].copy_from_slice(&n.to_be_bytes());
    w
}

#[inline]
fn word_b32(b: &FixedBytes<32>) -> [u8; 32] {
    *b.as_ref()
}

// ─── UserOp hash ──────────────────────────────────────────────────────────────

/// Compute the ERC-4337 v0.7 `userOpHash` that the account owner must sign.
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

// ─── Paymaster hash ───────────────────────────────────────────────────────────

/// Compute the hash that the paymaster's `verifyingSigner` must sign.
pub fn compute_paymaster_hash(
    user_op: &PackedUserOperation,
    chain_id: u64,
    paymaster: &Address,
    valid_until: u32,
    valid_after: u32,
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
    buf[9 * 32..10 * 32].copy_from_slice(&word_u32(valid_until));
    buf[10 * 32..11 * 32].copy_from_slice(&word_u32(valid_after));

    keccak256(buf)
}
