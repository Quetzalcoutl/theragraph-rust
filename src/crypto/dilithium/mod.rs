//! ML-DSA-65 signing — NIST FIPS 204.
//!
//! Pure Rust via PQClean bindings (pqcrypto-mldsa). No external liboqs needed.
//! Must use ML-DSA-65 (FIPS 204), NOT old NIST Round 3 Dilithium3 — the client
//! signs with @noble/post-quantum ml_dsa65 which follows FIPS 204 encoding.
//!
//! Key sizes (ML-DSA-65 / FIPS 204):
//!   Public key:  1952 bytes
//!   Secret key:  4032 bytes
//!   Signature:   3293 bytes (detached)

use pqcrypto_mldsa::mldsa65;
use pqcrypto_traits::sign::{
    DetachedSignature, PublicKey as PublicKeyTrait, SecretKey as SecretKeyTrait,
};

/// Generate an ML-DSA-65 keypair.
///
/// Returns `(public_key_bytes, secret_key_bytes)`.
/// Uses OS randomness — not deterministic. Callers store and protect the secret key.
#[allow(dead_code)]
pub fn keygen() -> (Vec<u8>, Vec<u8>) {
    let (pk, sk) = mldsa65::keypair();
    (pk.as_bytes().to_vec(), sk.as_bytes().to_vec())
}

/// Sign `message` with an ML-DSA-65 secret key.
///
/// `secret_key_bytes` must be exactly 4032 bytes (raw key, not base64).
/// Returns the detached signature (3293 bytes).
#[allow(dead_code)]
pub fn sign(message: &[u8], secret_key_bytes: &[u8]) -> Result<Vec<u8>, String> {
    let sk = mldsa65::SecretKey::from_bytes(secret_key_bytes)
        .map_err(|e| format!("invalid ML-DSA-65 secret key: {e}"))?;
    let sig = mldsa65::detached_sign(message, &sk);
    Ok(sig.as_bytes().to_vec())
}

/// Verify a detached ML-DSA-65 signature.
///
/// Returns `true` if the signature is valid for `message` under `public_key_bytes`.
/// Returns `false` on any invalid key, signature, or mismatch — never panics.
pub fn verify(message: &[u8], signature_bytes: &[u8], public_key_bytes: &[u8]) -> bool {
    let pk = match mldsa65::PublicKey::from_bytes(public_key_bytes) {
        Ok(k) => k,
        Err(_) => return false,
    };
    let sig = match mldsa65::DetachedSignature::from_bytes(signature_bytes) {
        Ok(s) => s,
        Err(_) => return false,
    };
    mldsa65::verify_detached_signature(&sig, message, &pk).is_ok()
}


#[cfg(test)]
mod tests;
