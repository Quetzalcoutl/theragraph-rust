    use super::*;

    fn test_addr() -> Address {
        "0xAbCdEf1234567890AbCdEf1234567890AbCdEf12"
            .parse()
            .unwrap()
    }

    fn other_addr() -> Address {
        "0x1111111111111111111111111111111111111111"
            .parse()
            .unwrap()
    }

    // ── is_throttled ──────────────────────────────────────────────────────────

    #[test]
    fn is_throttled_returns_false_for_unknown_sender() {
        let rep = SenderReputation::new();
        assert!(!rep.is_throttled(test_addr()));
    }

    #[test]
    fn is_throttled_false_after_four_failures() {
        let rep = SenderReputation::new();
        let sender = test_addr();
        for _ in 0..4 {
            rep.record_failure(sender);
        }
        assert!(!rep.is_throttled(sender));
    }

    #[test]
    fn is_throttled_true_after_max_failures() {
        let rep = SenderReputation::new();
        let sender = test_addr();
        for _ in 0..MAX_FAILURES {
            rep.record_failure(sender);
        }
        assert!(rep.is_throttled(sender));
    }

    // ── failure_count ─────────────────────────────────────────────────────────

    #[test]
    fn failure_count_returns_zero_for_unknown_sender() {
        let rep = SenderReputation::new();
        assert_eq!(rep.failure_count(other_addr()), 0);
    }

    #[test]
    fn failure_count_returns_correct_count_after_n_failures() {
        let rep = SenderReputation::new();
        let sender = test_addr();
        for i in 1..=3 {
            rep.record_failure(sender);
            assert_eq!(rep.failure_count(sender), i);
        }
    }

    // ── ban expiry ────────────────────────────────────────────────────────────

    #[test]
    fn is_throttled_false_after_ban_expires() {
        let rep = SenderReputation::new();
        let sender = test_addr();
        // Trigger throttle
        for _ in 0..MAX_FAILURES {
            rep.record_failure(sender);
        }
        assert!(rep.is_throttled(sender), "should be throttled immediately");
        // Backdate throttled_until to simulate the ban window passing
        rep.expire_ban(sender);
        assert!(!rep.is_throttled(sender), "should be unthrottled after ban expires");
    }

    // ── sender isolation ──────────────────────────────────────────────────────

    #[test]
    fn throttling_one_sender_does_not_affect_another() {
        let rep = SenderReputation::new();
        let bad = test_addr();
        let good = other_addr();

        for _ in 0..MAX_FAILURES {
            rep.record_failure(bad);
        }

        assert!(rep.is_throttled(bad), "bad sender should be throttled");
        assert!(!rep.is_throttled(good), "good sender must not be affected");
    }

    #[test]
    fn failure_count_is_independent_per_sender() {
        let rep = SenderReputation::new();
        let a = test_addr();
        let b = other_addr();

        rep.record_failure(a);
        rep.record_failure(a);
        rep.record_failure(b);

        assert_eq!(rep.failure_count(a), 2);
        assert_eq!(rep.failure_count(b), 1);
    }

    // ── failure_count while throttled ─────────────────────────────────────────

    #[test]
    fn failure_count_still_returns_value_while_throttled() {
        let rep = SenderReputation::new();
        let sender = test_addr();

        for _ in 0..MAX_FAILURES {
            rep.record_failure(sender);
        }

        assert!(rep.is_throttled(sender));
        // failure_count should reflect all recorded failures regardless of throttle state
        assert_eq!(
            rep.failure_count(sender),
            MAX_FAILURES,
            "failure_count must equal MAX_FAILURES while the sender is throttled"
        );
    }

    // ── clone shares state ────────────────────────────────────────────────────

    #[test]
    fn clone_shares_underlying_state() {
        let rep = SenderReputation::new();
        let rep2 = rep.clone();
        let sender = test_addr();

        rep.record_failure(sender);
        // The clone wraps the same Arc<DashMap>, so rep2 sees the change.
        assert_eq!(
            rep2.failure_count(sender),
            1,
            "cloned SenderReputation must share the same underlying state"
        );
    }
