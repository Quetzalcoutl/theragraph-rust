//! ABI decode helpers shared by `data.rs`.
//!
//! All functions are `pub(super)` — they are implementation details of the
//! events module and must not be part of the public API.

use alloy::dyn_abi::{DynSolType, DynSolValue};
use alloy::primitives::{Bytes, U256};

/// ABI-decode a tuple of types from raw log data.
///
/// Returns the flat list of values on success, `None` on decode failure
/// (caller emits a `Raw` variant with the hex payload).
pub(crate) fn abi_tuple(types: Vec<DynSolType>, data: &[u8]) -> Option<Vec<DynSolValue>> {
    DynSolType::Tuple(types).abi_decode_params(data).ok().and_then(|v| {
        if let DynSolValue::Tuple(vals) = v { Some(vals) } else { None }
    })
}

/// Decode a `DynSolValue::Uint` at position `idx` as its decimal string.
pub(crate) fn decode_uint(vals: &[DynSolValue], idx: usize) -> Option<String> {
    vals.get(idx).and_then(|v| {
        if let DynSolValue::Uint(u, _) = v { Some(u.to_string()) } else { None }
    })
}

/// Decode a `DynSolValue::String` at position `idx`, cloning the inner value.
pub(crate) fn decode_str(vals: &[DynSolValue], idx: usize) -> Option<String> {
    vals.get(idx).and_then(|v| {
        if let DynSolValue::String(s) = v { Some(s.clone()) } else { None }
    })
}

/// Decode a `DynSolValue::Bool` at position `idx`, copying the inner value.
pub(crate) fn decode_bool(vals: &[DynSolValue], idx: usize) -> Option<bool> {
    vals.get(idx).and_then(|v| {
        if let DynSolValue::Bool(b) = v { Some(*b) } else { None }
    })
}

/// Decode a `DynSolValue::Address` at position `idx` as a `0x`-prefixed hex string.
pub(crate) fn decode_addr_token(vals: &[DynSolValue], idx: usize) -> Option<String> {
    vals.get(idx).and_then(|v| {
        if let DynSolValue::Address(a) = v {
            Some(format!("{a:#x}"))
        } else {
            None
        }
    })
}

/// Read two consecutive raw U256 fields from `data` at byte offsets 0 and 32.
///
/// Returns `(field_at_0, field_at_32)` as decimal strings; each falls back to
/// `String::new()` when `data` is too short to contain that word.
///
/// Used by events whose non-indexed data layout is `[amount (32 bytes), timestamp (32 bytes)]`.
#[inline]
pub(crate) fn read_amount_timestamp(data: &Bytes) -> (String, String) {
    let amount = if data.len() >= 32 {
        U256::from_be_slice(&data[0..32]).to_string()
    } else {
        String::new()
    };
    let timestamp = if data.len() >= 64 {
        U256::from_be_slice(&data[32..64]).to_string()
    } else {
        String::new()
    };
    (amount, timestamp)
}

/// ABI-decode `[Uint(256), Uint(256)]` from `data` and return `(field_0, field_1)`.
///
/// Returns `None` when `data` is empty (caller should supply empty-data defaults).
/// Returns `Some(Err(hex))` when the ABI decoder fails (caller wraps as `Raw`).
/// Returns `Some(Ok((a, b)))` on success.
#[inline]
pub(crate) fn decode_two_uint256(
    data: &Bytes,
) -> Option<std::result::Result<(String, String), String>> {
    if data.is_empty() { return None; }
    let ty = DynSolType::Tuple(vec![DynSolType::Uint(256), DynSolType::Uint(256)]);
    Some(
        ty.abi_decode_params(data)
            .map(|val| {
                let vals = if let DynSolValue::Tuple(v) = val { v } else { vec![] };
                (decode_uint(&vals, 0).unwrap_or_default(), decode_uint(&vals, 1).unwrap_or_default())
            })
            .map_err(|_| format!("0x{}", hex::encode(data)))
    )
}

/// ABI-decode `[String, Uint(256)]` from `data` and return `(string_field, uint_field)`.
///
/// Same tri-state return convention as `decode_two_uint256`.
#[inline]
pub(crate) fn decode_str_uint256(
    data: &Bytes,
) -> Option<std::result::Result<(String, String), String>> {
    if data.is_empty() { return None; }
    let ty = DynSolType::Tuple(vec![DynSolType::String, DynSolType::Uint(256)]);
    Some(
        ty.abi_decode_params(data)
            .map(|val| {
                let vals = if let DynSolValue::Tuple(v) = val { v } else { vec![] };
                (decode_str(&vals, 0).unwrap_or_default(), decode_uint(&vals, 1).unwrap_or_default())
            })
            .map_err(|_| format!("0x{}", hex::encode(data)))
    )
}
