// ─── Gas Packing & Fee Estimation ─────────────────────────────────────────────
use alloy::primitives::FixedBytes;

#[inline]
fn pack_two_u128s(hi: u128, lo: u128) -> FixedBytes<32> {
    let mut out = [0u8; 32];
    out[..16].copy_from_slice(&hi.to_be_bytes());
    out[16..].copy_from_slice(&lo.to_be_bytes());
    FixedBytes(out)
}

#[inline]
fn unpack_two_u128s(packed: &FixedBytes<32>) -> (u128, u128) {
    let hi = u128::from_be_bytes(packed[..16].try_into().expect("slice is 16 bytes"));
    let lo = u128::from_be_bytes(packed[16..].try_into().expect("slice is 16 bytes"));
    (hi, lo)
}

pub fn pack_account_gas_limits(verification_gas_limit: u128, call_gas_limit: u128) -> FixedBytes<32> {
    pack_two_u128s(verification_gas_limit, call_gas_limit)
}

pub fn pack_gas_fees(max_priority_fee_per_gas: u128, max_fee_per_gas: u128) -> FixedBytes<32> {
    pack_two_u128s(max_priority_fee_per_gas, max_fee_per_gas)
}

#[allow(dead_code)]
pub fn unpack_account_gas_limits(packed: &FixedBytes<32>) -> (u128, u128) {
    unpack_two_u128s(packed)
}

#[allow(dead_code)]
pub fn unpack_gas_fees(packed: &FixedBytes<32>) -> (u128, u128) {
    unpack_two_u128s(packed)
}

pub fn scale_call_gas(base_call_gas: u64, n_calls: usize) -> u64 {
    let extra = u64::try_from(n_calls.saturating_sub(1))
        .unwrap_or(u64::MAX)
        .saturating_mul(100_000);
    base_call_gas.saturating_add(extra)
}

pub fn deployment_verification_gas(base: u64) -> u64 {
    base.saturating_add(400_000)
}


#[cfg(test)]
mod tests;
