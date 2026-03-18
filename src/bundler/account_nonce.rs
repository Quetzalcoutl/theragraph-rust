// ─── Per-Sender ERC-4337 UserOp Nonce Manager ────────────────────────────────
//
// Problem this solves:
//   When a user likes 10 NFTs in 20 seconds the frontend fires 10 concurrent
//   /sponsor requests.  Each one calls `ep.getNonce(sender, 0)` and gets the
//   SAME on-chain value (say, 5) because none of the previous ops have been
//   mined yet.  The user signs 10 ops all with nonce=5.  First one lands →
//   EntryPoint advances to 6.  The other 9 still carry nonce=5 →
//   "execution reverted" (62k gas, bundle-level rejection).
//
// Solution:
//   A `tokio::sync::Mutex<HashMap<Address, Reservation>>` that serialises
//   nonce allocation for each smart-account sender.
//
//   On the first /sponsor call for a sender:
//     1. Acquire the global lock.
//     2. Fetch the authoritative on-chain nonce (one RPC call, held under lock
//        to stop concurrent callers racing).
//     3. Record `{next: on_chain, reserved_at: now}` and return `on_chain`.
//
//   On every subsequent call before the previous op is confirmed:
//     1. Acquire the lock.
//     2. Return `pending.next`; advance `pending.next += 1`.
//     → No chain round-trip, no race.
//
//   Stale-entry eviction:
//     If a user builds a UserOp but never submits it (app closes, network
//     error) the reservation stays pending.  We evict entries older than
//     RESERVATION_TTL_SECS to prevent permanently over-estimating the nonce.
//     The next fresh /sponsor call after eviction re-fetches from chain.
//
// ERC-4337 nonce encoding (EntryPoint v0.7):
//   nonce = (key << 64) | seq
//   We always use key=0 (sequential lane).  Each `seq` increment is one
//   valid pending UserOp.  The EntryPoint verifies sequential seq values
//   but allows any number of in-flight ops per sender as long as their
//   seq values are 0, 1, 2… with no gaps.
//
// Architecture note (Flashbots alto / ERC-4337 reference bundler):
//   This is exactly the "nonceManager" component present in every production
//   bundler.  Alto calls it `NonceManangerService`; Skandha calls it
//   `UserOpNonceManager`.  The invariant is always: hand out nonces
//   optimistically and re-sync from chain on confirmed tx or eviction.

use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use alloy::{
    primitives::{Address, U256},
    providers::Provider,
};
use eyre::{Result, WrapErr};
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use super::{contracts::IEntryPoint, service::HttpProvider};

// ─── Tuning ───────────────────────────────────────────────────────────────────

/// How long (seconds) a pending reservation is kept before we treat it as
/// abandoned and re-sync from chain on the next /sponsor call.
const RESERVATION_TTL_SECS: u64 = 120;

// ─── Internal state ───────────────────────────────────────────────────────────

struct Reservation {
    /// The nonce value that was most recently handed out for this sender.
    /// The *next* call gets `last_handed + 1` (except the very first call
    /// which gets the value fetched from chain).
    last_handed: u64,
    reserved_at: Instant,
}

// ─── Public API ───────────────────────────────────────────────────────────────

/// Thread-safe per-sender ERC-4337 account nonce allocator.
///
/// Cheap to clone — all state is behind `Arc<Mutex<_>>`.
#[derive(Clone)]
pub struct UserOpNonceManager {
    inner:       Arc<Mutex<HashMap<Address, Reservation>>>,
    entry_point: Address,
}

impl UserOpNonceManager {
    pub fn new(entry_point: Address) -> Self {
        Self {
            inner:       Arc::new(Mutex::new(HashMap::new())),
            entry_point,
        }
    }

    /// Reserve the next valid ERC-4337 nonce for `sender`.
    ///
    /// Serialises under a global Mutex so no two concurrent /sponsor requests
    /// for the same (or different) senders can produce duplicate nonces.
    ///
    /// The lock is held across the optional async chain fetch so even the very
    /// first concurrent call pair is safe.
    pub async fn reserve(
        &self,
        sender:   Address,
        provider: &HttpProvider,
    ) -> Result<U256> {
        let mut guard = self.inner.lock().await;

        // Evict stale entries (quick in-place pass, no alloc).
        let ttl = Duration::from_secs(RESERVATION_TTL_SECS);
        guard.retain(|addr, res| {
            let fresh = res.reserved_at.elapsed() < ttl;
            if !fresh {
                warn!("UserOpNonceManager: evicting stale reservation for {addr:#x}");
            }
            fresh
        });

        let nonce_to_use = if let Some(res) = guard.get_mut(&sender) {
            // We already have a pending reservation → optimistically advance.
            let next = res.last_handed + 1;
            res.last_handed  = next;
            res.reserved_at  = Instant::now(); // refresh TTL
            debug!(
                "UserOpNonceManager: {sender:#x} → nonce {next} (pending+1)"
            );
            next
        } else {
            // No reservation yet → fetch authoritative nonce from chain.
            // NOTE: we hold the Mutex across this await.  That's intentional:
            // it prevents a concurrent caller from also hitting the chain and
            // getting the same value before we record our reservation.
            let chain_nonce = self.fetch_chain_nonce(sender, provider).await?;
            info!(
                "UserOpNonceManager: {sender:#x} → nonce {chain_nonce} (from chain)"
            );
            guard.insert(
                sender,
                Reservation { last_handed: chain_nonce, reserved_at: Instant::now() },
            );
            chain_nonce
        };

        // ERC-4337 nonce encoding: nonce = (key << 64) | seq
        // We always use key=0, so the full uint256 value equals the seq.
        Ok(U256::from(nonce_to_use))
    }

    /// Notify the manager that an op was confirmed on-chain at `confirmed_nonce`.
    ///
    /// This removes the reservation if the confirmed seq is ≥ last_handed,
    /// forcing the next /sponsor to re-sync from chain (which is now at
    /// confirmed_nonce + 1).  This is a best-effort call — it's fine to skip
    /// it; the TTL eviction will clean up eventually.
    pub async fn on_confirmed(&self, sender: Address, confirmed_nonce: u64) {
        let mut guard = self.inner.lock().await;
        if let Some(res) = guard.get(&sender) {
            if confirmed_nonce >= res.last_handed {
                guard.remove(&sender);
                debug!(
                    "UserOpNonceManager: cleared reservation for {sender:#x} \
                     (confirmed nonce {confirmed_nonce})"
                );
            }
        }
    }

    // ── Private helpers ───────────────────────────────────────────────────────

    async fn fetch_chain_nonce(
        &self,
        sender:   Address,
        provider: &HttpProvider,
    ) -> Result<u64> {
        use alloy::primitives::Uint;
        type U192 = Uint<192, 3>;

        let ep     = IEntryPoint::new(self.entry_point, provider.clone());
        let result = ep
            .getNonce(sender, U192::ZERO)
            .call()
            .await
            .wrap_err("getNonce RPC failed")?;

        // Low 64 bits of the uint256 = seq counter for key=0.
        let seq: u64 = result.nonce
            .try_into()
            .unwrap_or_else(|_| (result.nonce & U256::from(u64::MAX)).try_into().unwrap_or(0));
        Ok(seq)
    }
}
