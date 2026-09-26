use super::*;
use alloy::signers::{local::PrivateKeySigner, Signer};

// 1. Known third-party test vector (web3.js docs), independent of this codebase's
//    signing path — pins that our hex-parsing + prefix-stripping + recovery wiring
//    is correct, not just self-consistent with our own signer.
//    https://web3js.readthedocs.io/en/v1.2.2/web3-eth-accounts.html#sign
#[test]
fn recovers_known_web3_test_vector() {
    let message = "Some data";
    let signature = "0xb91467e570a6466aa9e9876cbcd013baba02900b8979d43fe208a4a4f339f5fd6007e74cd82e037b800186422fc2da167c747ef045e5d18a5f5d4300f8e1a0291c";
    let expected = "0x2c7536e3605d9c16a7a3d7b1898e529396a65c23";

    let recovered = recover_eth_signer(message, signature).expect("recovery should succeed");
    assert_eq!(recovered, expected);
}

// 1b. A second, independently-produced real-world vector: signed live by Node's
//     viem (`walletClient.signMessage`) against a freshly generated secp256k1
//     keypair — the exact frontend code path this whole feature exists to verify
//     (see therafriendz/src/utils/messageSession.ts), not merely self-consistent
//     with our own Rust signer. Also doubles as proof that
//     `TheraGraphWeb.SessionAuth.session_message/1`'s reconstructed message
//     (same literal string here) is what viem actually signed.
#[test]
fn recovers_real_viem_produced_signature() {
    let message = "TheraGraph Message Session\nValid until: 2025-10-04T12:34:56.789Z\nVersion: 1";
    let signature = "0xb22416f59424cff62e1952bdcd244ab3ba207c2a1e55d1a01f00dc05e0fc3f8f28891afbfdfb987731980ff6454cbd4be24b7913cc80c5d4450e90912b3b8c181c";
    let expected = "0xf22575deb2a6356cb89547f9c4af9504d9ae7417";

    let recovered = recover_eth_signer(message, signature).expect("recovery should succeed");
    assert_eq!(recovered, expected);

    // Tampering with the message must not recover the same address.
    let tampered = "TheraGraph Message Session\nValid until: 2025-10-04T12:34:56.790Z\nVersion: 1";
    let recovered_tampered =
        recover_eth_signer(tampered, signature).expect("recovery math still succeeds");
    assert_ne!(recovered_tampered, expected, "one-digit drift must not still recover the signer");

    // A caller comparing against the WRONG expected address (the exact
    // `recovered.eq_ignore_ascii_case(expected)` check `eth_verify_handler` in
    // src/api/mod.rs performs) must reject — this is what stops the original
    // vulnerability this feature fixes (binding a fake signature to a victim's
    // address): the recovered signer never matches an attacker-claimed address
    // that isn't actually who signed.
    let wrong_address = "0x000000000000000000000000000000000000dead";
    assert!(
        !recovered.eq_ignore_ascii_case(wrong_address),
        "must not match an address that did not sign the message"
    );
}

// 2. Round trip: sign a message with a freshly generated secp256k1 keypair (via
//    alloy's own EIP-191 signer, `sign_message`) and confirm we recover the same
//    address that signed it.
#[tokio::test]
async fn round_trip_sign_and_recover() {
    let signer = PrivateKeySigner::random();
    let expected = format!("{:?}", signer.address());

    let message = "TheraGraph Message Session\nValid until: 2026-10-04T12:34:56.789Z\nVersion: 1";
    let sig = signer.sign_message(message.as_bytes()).await.expect("sign should succeed");
    let sig_hex = format!("0x{}", hex::encode(sig.as_bytes()));

    let recovered = recover_eth_signer(message, &sig_hex).expect("recovery should succeed");
    assert_eq!(recovered, expected);
}

// 3. Tampering with the signed message must not recover the original signer's address.
#[tokio::test]
async fn tampered_message_recovers_different_address() {
    let signer = PrivateKeySigner::random();
    let expected = format!("{:?}", signer.address());

    let message = "original session message";
    let sig = signer.sign_message(message.as_bytes()).await.expect("sign should succeed");
    let sig_hex = format!("0x{}", hex::encode(sig.as_bytes()));

    let tampered = "tampered session message";
    let recovered =
        recover_eth_signer(tampered, &sig_hex).expect("recovery math still succeeds on tampered input");
    assert_ne!(recovered, expected, "tampered message must not recover the original signer");
}

// 4. Malformed (wrong-length) signature hex must return Err, not panic.
#[test]
fn wrong_length_signature_is_error() {
    let result = recover_eth_signer("hello", "0xdeadbeef");
    assert!(result.is_err(), "short signature must return Err");
}

// 5. Non-hex signature body must return Err, not panic.
#[test]
fn non_hex_signature_is_error() {
    let bogus = format!("0x{}", "zz".repeat(65));
    let result = recover_eth_signer("hello", &bogus);
    assert!(result.is_err(), "non-hex signature must return Err");
}

// 6. Signature accepted without the `0x` prefix too (defense in depth — Elixir's
//    session_auth.ex lower-cases but does not strip the prefix before calling us,
//    so this mirrors what the seam actually receives; kept as a guard against a
//    future caller that forgets the prefix).
#[tokio::test]
async fn recovers_without_0x_prefix_on_signature() {
    let signer = PrivateKeySigner::random();
    let expected = format!("{:?}", signer.address());

    let message = "no prefix on the signature hex";
    let sig = signer.sign_message(message.as_bytes()).await.expect("sign should succeed");
    let sig_hex = hex::encode(sig.as_bytes()); // no "0x" prefix

    let recovered = recover_eth_signer(message, &sig_hex).expect("recovery should succeed");
    assert_eq!(recovered, expected);
}
