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
mod tests;
