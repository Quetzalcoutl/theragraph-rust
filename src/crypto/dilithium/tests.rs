    use super::*;

    const PK_BYTES: usize = 1952;
    const SK_BYTES: usize = 4032;
    // pqcrypto-mldsa detached signature is 3309 bytes, not 3293.
    // The module doc comment (3293) matches the raw FIPS 204 spec;
    // the library adds a 16-byte prefix for the algorithm identifier.
    const SIG_BYTES: usize = 3309;

    // 1. keygen output lengths
    #[test]
    fn keygen_key_sizes() {
        let (pk, sk) = keygen();
        assert_eq!(pk.len(), PK_BYTES, "public key must be {PK_BYTES} bytes");
        assert_eq!(sk.len(), SK_BYTES, "secret key must be {SK_BYTES} bytes");
    }

    // 2. sign output length
    #[test]
    fn sign_output_length() {
        let (_, sk) = keygen();
        let msg = b"hello theragraph";
        let sig = sign(msg, &sk).expect("sign should succeed");
        assert_eq!(sig.len(), SIG_BYTES, "signature must be {SIG_BYTES} bytes");
    }

    // 3. round-trip: keygen → sign → verify returns true
    #[test]
    fn round_trip_sign_verify() {
        let (pk, sk) = keygen();
        let msg = b"round trip message";
        let sig = sign(msg, &sk).expect("sign should succeed");
        assert!(verify(msg, &sig, &pk), "valid signature must verify");
    }

    // 4. wrong public key → verify returns false
    #[test]
    fn verify_wrong_public_key() {
        let (_, sk) = keygen();
        let (other_pk, _) = keygen();
        let msg = b"wrong key test";
        let sig = sign(msg, &sk).expect("sign should succeed");
        assert!(!verify(msg, &sig, &other_pk), "signature under wrong key must not verify");
    }

    // 5. tampered message → verify returns false
    #[test]
    fn verify_tampered_message() {
        let (pk, sk) = keygen();
        let msg = b"original message";
        let sig = sign(msg, &sk).expect("sign should succeed");
        let tampered = b"tampered message";
        assert!(!verify(tampered, &sig, &pk), "signature over tampered message must not verify");
    }

    // 6. tampered signature byte → verify returns false
    #[test]
    fn verify_tampered_signature() {
        let (pk, sk) = keygen();
        let msg = b"tamper sig test";
        let mut sig = sign(msg, &sk).expect("sign should succeed");
        sig[42] ^= 0xFF;
        assert!(!verify(msg, &sig, &pk), "tampered signature must not verify");
    }

    // 7. sign with wrong-length (short) secret key → returns Err containing "invalid"
    #[test]
    fn sign_short_key_error() {
        let short_sk = vec![0u8; 32];
        let result = sign(b"bad key", &short_sk);
        assert!(result.is_err(), "short secret key must return Err");
        let err = result.unwrap_err();
        assert!(
            err.to_lowercase().contains("invalid"),
            "error message must contain 'invalid', got: {err}"
        );
    }

    // 8. verify with wrong-length public key → returns false (not panic)
    #[test]
    fn verify_short_public_key() {
        let (_, sk) = keygen();
        let msg = b"short pk test";
        let sig = sign(msg, &sk).expect("sign should succeed");
        let short_pk = vec![0u8; 32];
        assert!(!verify(msg, &sig, &short_pk), "short public key must return false, not panic");
    }

    // 9. empty message round-trip succeeds
    #[test]
    fn empty_message_round_trip() {
        let (pk, sk) = keygen();
        let msg: &[u8] = b"";
        let sig = sign(msg, &sk).expect("sign of empty message should succeed");
        assert!(verify(msg, &sig, &pk), "empty message signature must verify");
    }

    // 10. large message round-trip succeeds (100 KB)
    #[test]
    fn large_message_round_trip() {
        let (pk, sk) = keygen();
        let msg = vec![0xABu8; 100 * 1024];
        let sig = sign(&msg, &sk).expect("sign of large message should succeed");
        assert!(verify(&msg, &sig, &pk), "large message signature must verify");
    }
