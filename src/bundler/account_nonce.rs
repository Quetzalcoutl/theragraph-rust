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
//   A `DashMap<Address, Arc<Mutex<Option<Reservation>>>>` with per-sender
//   Mutex locks so concurrent /sponsor requests for DIFFERENT senders never
//   serialize.  Only requests for the SAME sender serialize (preventing
//   duplicate-nonce races within one sender's concurrent calls).
//
//   Previously used Arc<Mutex<HashMap<Address, Reservation>>> — a GLOBAL lock
//   held across the async RPC fetch.  Under load with N senders, all N fetches
//   serialised through one queue even though they are completely independent.
//
//   On the first /sponsor call for a sender:
//     1. Acquire the per-sender lock.
//     2. Fetch the authoritative on-chain nonce (one RPC call, held under
//        per-sender lock to stop concurrent calls for THIS sender racing).
//     3. Record `{last_handed: on_chain, reserved_at: now}` and return.
//
//   On every subsequent call before the previous op is confirmed:
//     1. Acquire the per-sender lock.
//     2. Return `last_handed + 1`; advance counter.
//     → No chain round-trip, no race.
//
//   Stale-entry eviction:
//     If a user builds a UserOp but never submits it (app closes, network
//     error) the reservation stays pending.  We evict entries older than
//     RESERVATION_TTL_SECS via DashMap::retain on every reserve() call.
//     The next fresh /sponsor call after eviction re-fetches from chain.
//
// ERC-4337 nonce encoding (EntryPoint v0.7):
//   nonce = (key << 64) | seq
//   We always use key=0 (sequential lane).  Each `seq` increment is one
//   valid pending UserOp.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use alloy::primitives::{Address, U256};
use dashmap::DashMap;
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
    last_handed: u64,
    reserved_at: Instant,
}

// ─── Public API ───────────────────────────────────────────────────────────────

/// Thread-safe per-sender ERC-4337 account nonce allocator.
///
/// Cheap to clone — all state is behind `Arc<DashMap<_>>`.
#[derive(Clone)]
pub struct UserOpNonceManager {
    inner:       Arc<DashMap<Address, Arc<Mutex<Option<Reservation>>>>>,
    entry_point: Address,
}

impl UserOpNonceManager {
    pub fn new(entry_point: Address) -> Self {
        Self {
            inner:       Arc::new(DashMap::new()),
            entry_point,
        }
    }

    /// Reserve the next valid ERC-4337 nonce for `sender`.
    ///
    /// Serialises under a per-sender Mutex so concurrent /sponsor requests for
    /// the same sender cannot produce duplicate nonces, while requests for
    /// different senders proceed concurrently.
    ///
    /// The per-sender lock is held across the optional async chain fetch so
    /// even the first concurrent call pair for a sender is safe.
    pub async fn reserve(
        &self,
        sender:   Address,
        provider: &HttpProvider,
    ) -> Result<U256> {
        // Evict stale entries — try_lock skips any entry currently in use.
        let ttl = Duration::from_secs(RESERVATION_TTL_SECS);
        self.inner.retain(|addr, lock| {
            if let Ok(guard) = lock.try_lock() {
                if let Some(ref res) = *guard {
                    let fresh = res.reserved_at.elapsed() < ttl;
                    if !fresh {
                        warn!("UserOpNonceManager: evicting stale reservation for {addr:#x}");
                    }
                    return fresh;
                }
                return false; // None = no reservation, remove the slot
            }
            true // locked = currently in use, keep
        });

        // Get or create per-sender lock slot.
        let sender_lock = self.inner
            .entry(sender)
            .or_insert_with(|| Arc::new(Mutex::new(None)))
            .clone();

        // Only serialises concurrent calls for THIS sender.
        let mut guard = sender_lock.lock().await;

        let nonce_to_use = if let Some(ref mut res) = *guard {
            // We already have a pending reservation → optimistically advance.
            let next = res.last_handed.checked_add(1)
                .ok_or_else(|| eyre::eyre!("nonce u64 overflow for {sender:#x}"))?;
            res.last_handed = next;
            res.reserved_at = Instant::now(); // refresh TTL
            debug!("UserOpNonceManager: {sender:#x} → nonce {next} (pending+1)");
            next
        } else {
            // No reservation yet → fetch authoritative nonce from chain.
            // Per-sender lock prevents a second concurrent call for this sender
            // from fetching the same chain value before we record the reservation.
            let chain_nonce = self.fetch_chain_nonce(sender, provider).await?;
            info!("UserOpNonceManager: {sender:#x} → nonce {chain_nonce} (from chain)");
            *guard = Some(Reservation { last_handed: chain_nonce, reserved_at: Instant::now() });
            chain_nonce
        };

        // ERC-4337 nonce encoding: nonce = (key << 64) | seq
        // We always use key=0, so the full uint256 value equals the seq.
        Ok(U256::from(nonce_to_use))
    }

    /// Notify the manager that an op was confirmed on-chain at `confirmed_nonce`.
    ///
    /// Clears the reservation if the confirmed seq is ≥ last_handed, forcing
    /// the next /sponsor to re-sync from chain.  Best-effort — TTL eviction
    /// handles the case where this is never called.
    #[allow(dead_code)]
    pub async fn on_confirmed(&self, sender: Address, confirmed_nonce: u64) {
        if let Some(sender_lock) = self.inner.get(&sender) {
            let mut guard = sender_lock.lock().await;
            if let Some(ref res) = *guard {
                if confirmed_nonce >= res.last_handed {
                    *guard = None;
                    debug!(
                        "UserOpNonceManager: cleared reservation for {sender:#x} \
                         (confirmed nonce {confirmed_nonce})"
                    );
                }
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
        let seq: u64 = (result.nonce & U256::from(u64::MAX))
            .try_into()
            .wrap_err("nonce low-64-bits extraction failed — masked value > u64::MAX")?;
        Ok(seq)
    }
}
