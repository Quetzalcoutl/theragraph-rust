//! Ethereum ECDSA (secp256k1) signature recovery — EIP-191 `personal_sign`.
//!
//! Verifies wallet-signed messages produced by viem's
//! `walletClient.signMessage({ account, message })`, which:
//!   1. prefixes `message` per EIP-191: `"\x19Ethereum Signed Message:\n" + byte_len(message) + message`
//!      (no separator between the length and the message — the ASCII decimal digits run
//!      straight into the message bytes),
//!   2. keccak256-hashes the prefixed bytes,
//!   3. ECDSA-signs the hash over secp256k1.
//!
//! alloy's `Signature::recover_address_from_msg` performs exactly this prefix+hash
//! internally (see `alloy_primitives::eip191_hash_message`), so this module does not
//! need to reconstruct the prefixed byte string by hand — it only needs to parse the
//! raw 65-byte signature and hand alloy the *unprefixed* message, matching what viem
//! was given.
//!
//! Unlike `crypto::dilithium::verify` (which takes a public key and returns a bool),
//! ECDSA signatures don't carry the signer's key — verification is "recover the
//! address, then compare it to the address you expected." That comparison is left to
//! the caller (Elixir's `EthSignature.verify_signer/3`) so this module's job is only
//! recovery.

use alloy::primitives::Signature;

/// Recover the Ethereum address that produced `signature_hex` over `message`.
///
/// `message` is the *raw* (unprefixed) string that was passed to
/// `walletClient.signMessage({ message })` on the frontend — this function applies
/// the EIP-191 prefix and keccak256 hash internally, exactly matching what viem signs.
///
/// `signature_hex` is a `0x`-prefixed (optional) 130-hex-char string: the raw 65 bytes
/// `r (32) || s (32) || v (1)`, matching what `walletClient.signMessage` returns.
///
/// Returns the recovered address as a lowercase `0x…`-prefixed hex string on success,
/// or a `String` describing what went wrong (malformed hex, wrong length, invalid
/// signature) — this function never panics on attacker-controlled input.
pub fn recover_eth_signer(message: &str, signature_hex: &str) -> Result<String, String> {
    let hex_body = signature_hex.strip_prefix("0x").unwrap_or(signature_hex);
    if hex_body.len() != 130 {
        return Err(format!(
            "signature must decode to 65 bytes (130 hex chars), got {} hex chars",
            hex_body.len()
        ));
    }

    let raw = hex::decode(hex_body).map_err(|e| format!("invalid signature hex: {e}"))?;

    let sig = Signature::from_raw(&raw).map_err(|e| format!("malformed signature: {e}"))?;

    let address = sig
        .recover_address_from_msg(message.as_bytes())
        .map_err(|e| format!("signature recovery failed: {e}"))?;

    // `{:?}` (Debug) on alloy's Address prints lowercase hex without EIP-55 checksum
    // casing — `{}` (Display) would checksum-case it, which the Elixir side does not
    // expect (it lower-cases the expected address before comparing).
    Ok(format!("{address:?}"))
}

#[cfg(test)]
mod tests;
