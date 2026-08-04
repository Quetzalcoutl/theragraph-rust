//! Rustler NIF bindings — exposes ML-DSA-65 (FIPS 204) crypto to the BEAM.
//!
//! Single function group:
//!   - ML-DSA-65 post-quantum crypto (keygen/sign/verify) — DirtyCpu, sub-millisecond
//!
//! Scoring NIFs (score_nft/score_nft_batch) were removed — the Rust HTTP engine
//! uses a richer scoring model (exponential tag boosting, diversity penalties, Rayon
//! parallelism) that cannot be replicated in a NIF without diverging from production
//! results. The HTTP path + FallbackEngine is the correct cascade.
//!
//! Build with `--features nif` (Rustler / Mix handles this automatically).
//! Elixir entry point: `TheraGraph.QuantumNif` (use Rustler, otp_app: :theragraph).

use rustler::{Binary, Env, NifResult, OwnedBinary};
use crate::crypto::dilithium;

// ── ML-DSA-65 (FIPS 204) post-quantum crypto ─────────────────────────────────
//
// All three NIFs run on DirtyCpu: keygen ~0.1ms, sign ~0.5ms, verify ~0.7ms.
// ML-DSA-65 (NIST FIPS 204) sizes: pubkey 1952 B, seckey 4032 B, signature 3293 B.
// Note: function names use "ml_dsa65" to match the FIPS 204 standard. The HKDF
// info string "theragraph-dilithium3-v1" is a protocol constant and must NOT change.

/// Generate a fresh ML-DSA-65 keypair.
/// Returns `{public_key_bytes, secret_key_bytes}` allocated directly in BEAM heap.
#[rustler::nif(name = "ml_dsa65_keygen", schedule = "DirtyCpu")]
fn ml_dsa65_keygen(env: Env) -> NifResult<(Binary, Binary)> {
    let (pk, sk) = dilithium::keygen();

    let mut pk_bin = OwnedBinary::new(pk.len())
        .ok_or(rustler::Error::Atom("alloc_error"))?;
    pk_bin.as_mut_slice().copy_from_slice(&pk);

    let mut sk_bin = OwnedBinary::new(sk.len())
        .ok_or(rustler::Error::Atom("alloc_error"))?;
    sk_bin.as_mut_slice().copy_from_slice(&sk);

    Ok((pk_bin.release(env), sk_bin.release(env)))
}

/// Produce a detached ML-DSA-65 signature.
/// `secret_key` must be the raw 4032-byte key returned by `ml_dsa65_keygen`.
/// Returns the 3293-byte detached signature allocated directly in BEAM heap.
/// Inputs are zero-copy refs into the BEAM heap (Binary<'_> vs Vec<u8>).
#[rustler::nif(name = "ml_dsa65_sign", schedule = "DirtyCpu")]
fn ml_dsa65_sign(env: Env, message: Binary, secret_key: Binary) -> NifResult<Binary> {
    let sig = dilithium::sign(&message, &secret_key)
        .map_err(|e| rustler::Error::Term(Box::new(e)))?;

    let mut bin = OwnedBinary::new(sig.len())
        .ok_or(rustler::Error::Atom("alloc_error"))?;
    bin.as_mut_slice().copy_from_slice(&sig);

    Ok(bin.release(env))
}

/// Verify a detached ML-DSA-65 signature.
/// Returns `true` if valid, `false` on any key/signature/message mismatch.
/// Never returns an error — invalid inputs yield `false`.
/// Inputs are zero-copy refs into the BEAM heap (Binary<'_> vs Vec<u8>).
#[rustler::nif(name = "ml_dsa65_verify", schedule = "DirtyCpu")]
fn ml_dsa65_verify(_env: Env, message: Binary, signature: Binary, public_key: Binary) -> NifResult<bool> {
    Ok(dilithium::verify(&message, &signature, &public_key))
}

// ── NIF registration ─────────────────────────────────────────────────────────

rustler::init!(
    "Elixir.TheraGraph.QuantumNif",
    [
        ml_dsa65_keygen,
        ml_dsa65_sign,
        ml_dsa65_verify,
    ]
);
