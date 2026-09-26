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
use crate::crypto::eth_signature;

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
fn ml_dsa65_sign<'a>(env: Env<'a>, message: Binary<'a>, secret_key: Binary<'a>) -> NifResult<Binary<'a>> {
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
#[allow(unused_variables)] // `env` must be named exactly `env` — rustler_codegen 0.38
                           // re-splices the parameter list by name when calling this
                           // function from the generated NIF wrapper, so `_env` (the
                           // usual "unused" convention) does not resolve.
fn ml_dsa65_verify(env: Env, message: Binary, signature: Binary, public_key: Binary) -> NifResult<bool> {
    Ok(dilithium::verify(&message, &signature, &public_key))
}

// ── Ethereum ECDSA (secp256k1) signature recovery ────────────────────────────
//
// Recovers the wallet address that produced an EIP-191 `personal_sign` signature
// (viem's `walletClient.signMessage`). Used by TheraGraphWeb.SessionAuth to verify
// message-session registration signatures instead of the old regex-only check.
// secp256k1 recovery runs in tens of microseconds — DirtyCpu is used here purely
// for consistency with the other crypto NIFs above, not because it's required.

/// Recover the Ethereum address that signed `message` (raw, unprefixed) with the
/// given EIP-191 signature.
///
/// `signature_hex` is a `0x`-prefixed 130-hex-char string: the raw 65 bytes
/// `r (32) || s (32) || v (1)`, exactly what `walletClient.signMessage` returns.
///
/// On success, returns the recovered address as a lowercase `0x…` string directly
/// (not wrapped). On failure (malformed hex, wrong length, bad signature), returns
/// `rustler::Error::Term`, which Rustler encodes as `{:error, reason_string}`.
#[rustler::nif(name = "eth_recover_signer", schedule = "DirtyCpu")]
fn eth_recover_signer(message: String, signature_hex: String) -> NifResult<String> {
    eth_signature::recover_eth_signer(&message, &signature_hex)
        .map_err(|e| rustler::Error::Term(Box::new(e)))
}

// ── NIF registration ─────────────────────────────────────────────────────────
//
// rustler 0.38 no longer takes an explicit function list here — each
// `#[rustler::nif]`-annotated function self-registers via `inventory::submit!`
// at link time (see rustler_codegen::nif). `init!` takes only the target Elixir
// module name (plus an optional `load = <fn>` for a custom on-load callback,
// unused here). Passing a bracketed function list (the pre-0.3x API this call
// was written against) fails to parse under 0.38 with "expected assignment
// expression (i.e. `load = load`)" — fixed here as part of adding
// `eth_recover_signer`, since both live in the same `rustler::init!` call.

rustler::init!("Elixir.TheraGraph.QuantumNif");
