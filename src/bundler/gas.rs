// ─── Gas Packing & Fee Estimation ─────────────────────────────────────────────
//
// ERC-4337 v0.7 packs two 128-bit values into each bytes32 slot:
//
//   accountGasLimits = verificationGasLimit(16 B big-endian)
//                   || callGasLimit(16 B big-endian)
//
//   gasFees          = maxPriorityFeePerGas(16 B big-endian)
//                   || maxFeePerGas(16 B big-endian)
//
// Architecture note (Evan Simmons / Ferrous Systems):
//   Checked arithmetic throughout — any overflow at packing time indicates a
//   programming error or an adversarial input, not a transient failure.

use alloy::primitives::FixedBytes;

// ─── Packing ──────────────────────────────────────────────────────────────────

/// Pack two gas limits into `accountGasLimits` (bytes32).
pub fn pack_account_gas_limits(
    verification_gas_limit: u128,
    call_gas_limit: u128,
) -> FixedBytes<32> {
    let mut out = [0u8; 32];
    out[..16].copy_from_slice(&verification_gas_limit.to_be_bytes());
    out[16..].copy_from_slice(&call_gas_limit.to_be_bytes());
    FixedBytes(out)
}

/// Pack priority fee and max fee into `gasFees` (bytes32).
pub fn pack_gas_fees(
    max_priority_fee_per_gas: u128,
    max_fee_per_gas: u128,
) -> FixedBytes<32> {
    let mut out = [0u8; 32];
    out[..16].copy_from_slice(&max_priority_fee_per_gas.to_be_bytes());
    out[16..].copy_from_slice(&max_fee_per_gas.to_be_bytes());
    FixedBytes(out)
}

/// Unpack `accountGasLimits` → (verificationGasLimit, callGasLimit).
#[allow(dead_code)]
pub fn unpack_account_gas_limits(packed: &FixedBytes<32>) -> (u128, u128) {
    let vgl = u128::from_be_bytes(packed[..16].try_into().expect("slice is 16 bytes"));
    let cgl = u128::from_be_bytes(packed[16..].try_into().expect("slice is 16 bytes"));
    (vgl, cgl)
}

/// Unpack `gasFees` → (maxPriorityFeePerGas, maxFeePerGas).
#[allow(dead_code)]
pub fn unpack_gas_fees(packed: &FixedBytes<32>) -> (u128, u128) {
    let pf = u128::from_be_bytes(packed[..16].try_into().expect("slice is 16 bytes"));
    let mf = u128::from_be_bytes(packed[16..].try_into().expect("slice is 16 bytes"));
    (pf, mf)
}

// ─── Scale helpers ────────────────────────────────────────────────────────────

/// Scale `base_call_gas` for a batch of `n` calls (base + 100k per extra).
pub fn scale_call_gas(base_call_gas: u64, n_calls: usize) -> u64 {
    base_call_gas + u64::try_from(n_calls.saturating_sub(1)).unwrap_or(0) * 100_000
}

/// Add extra deployment verification gas when the account is not yet deployed.
pub fn deployment_verification_gas(base: u64) -> u64 {
    base + 400_000
}
