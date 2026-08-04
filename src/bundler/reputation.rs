// ─── Sender Reputation Tracker ───────────────────────────────────────────────
//
// Defends against the ERC-4337 "Simulation Collision" attack:
//   An attacker crafts a UserOperation that passes bundler simulation
//   (so the Paymaster signs it) but intentionally reverts on-chain.
//   The Paymaster is charged the revert gas; no useful work is done.
//   At scale (botnet × 10,000 ops) this drains the Paymaster deposit
//   within hours.
//
// Mitigation (ERC-7562 §3.3 — "entity reputation"):
//   Track per-sender (sender address) failure counts within a rolling
//   window.  If the failure rate exceeds MAX_FAILURE_RATE the sender is
//   temporarily throttled.  After the cool-down window passes, we give
//   the sender one retry (benefit of the doubt for transient failures).
//
// Integration points:
//   1. `routes/sponsor.rs`  — reject the /sponsor call for throttled senders
//      before building the UserOp or calling the signer (zero treasury cost).
//   2. `mempool.rs`         — record a failure after batch submission confirms
//      a revert, so the reputation tracks actual on-chain behaviour.
//
// Design notes:
//   • Uses DashMap (sharded concurrent HashMap) — no global lock under load.
//   • Sliding window implemented as a VecDeque of timestamps; expired entries
//     are lazily pruned on each access (amortised O(1)).
//   • `Clone` is cheap: all state is behind Arc.

use std::{
    collections::VecDeque,
    sync::Arc,
    time::{Duration, Instant},
};

use alloy::primitives::Address;
use dashmap::DashMap;
use tracing::warn;

// ── Tuning constants ─────────────────────────────────────────────────────────

/// Rolling window over which failures are counted.
const WINDOW: Duration = Duration::from_secs(3_600); // 1 hour

/// Max failures allowed within the window before throttling.
const MAX_FAILURES: usize = 5;

/// Once throttled, the sender is blocked for this duration.
const BAN_DURATION: Duration = Duration::from_secs(3_600); // 1 hour

// ── Types ─────────────────────────────────────────────────────────────────────

struct SenderEntry {
    /// Timestamps of recent on-chain execution failures.
    failures: VecDeque<Instant>,
    /// If Some, the sender is throttled until this instant.
    throttled_until: Option<Instant>,
}

impl SenderEntry {
    fn new() -> Self {
        Self { failures: VecDeque::new(), throttled_until: None }
    }

    /// Remove failure timestamps older than WINDOW.
    fn prune(&mut self) {
        let cutoff = Instant::now() - WINDOW;
        while self.failures.front().map_or(false, |&t| t < cutoff) {
            self.failures.pop_front();
        }
    }
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Thread-safe, `Clone`-cheap sender reputation tracker.
#[derive(Clone)]
pub struct SenderReputation {
    inner: Arc<DashMap<Address, SenderEntry>>,
}

impl SenderReputation {
    pub fn new() -> Self {
        Self { inner: Arc::new(DashMap::new()) }
    }

    /// Returns `true` if the sender is currently throttled.
    ///
    /// Call this in `routes/sponsor.rs` before signing the UserOp.
    pub fn is_throttled(&self, sender: Address) -> bool {
        let Some(mut entry) = self.inner.get_mut(&sender) else {
            return false;
        };
        if let Some(until) = entry.throttled_until {
            if Instant::now() < until {
                return true;
            }
            // Cool-down expired — lift the throttle.
            entry.throttled_until = None;
        }
        false
    }

    /// Record an on-chain execution failure for the sender.
    ///
    /// Call this in the mempool batch processor when a UserOp reverts
    /// on-chain (EntryPoint emits `UserOperationRevertReason`).
    pub fn record_failure(&self, sender: Address) {
        let mut entry = self.inner.entry(sender).or_insert_with(SenderEntry::new);
        entry.prune();
        entry.failures.push_back(Instant::now());

        if entry.failures.len() >= MAX_FAILURES {
            let until = Instant::now() + BAN_DURATION;
            entry.throttled_until = Some(until);
            warn!(
                sender = %sender,
                failures = entry.failures.len(),
                "Sender throttled for {}s: on-chain failure rate exceeded {} failures/hour",
                BAN_DURATION.as_secs(),
                MAX_FAILURES,
            );
        }
    }

    /// Expose current failure count for a sender (used in metrics / admin).
    pub fn failure_count(&self, sender: Address) -> usize {
        let Some(mut entry) = self.inner.get_mut(&sender) else {
            return 0;
        };
        entry.prune();
        entry.failures.len()
    }

    /// Test-only helper: backdate `throttled_until` so the ban appears expired.
    #[cfg(test)]
    fn expire_ban(&self, sender: Address) {
        if let Some(mut entry) = self.inner.get_mut(&sender) {
            entry.throttled_until = Some(Instant::now() - Duration::from_secs(2));
        }
    }
}

#[cfg(test)]
mod tests {
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
}
