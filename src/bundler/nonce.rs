// ─── Nonce Manager ────────────────────────────────────────────────────────────
//
// Prevents the most common bundler failure mode: nonce races.
//
// Problem: when N UserOps arrive concurrently, every `do_submit` call fires
// `get_transaction_count` at the same time, gets the same pending nonce, signs
// N different L1 transactions with that same nonce, and broadcasts — only one
// lands; the other N-1 get "nonce too low" / "replacement underpriced" and
// `send_raw_transaction` returns an error.
//
// Solution: a `tokio::sync::Mutex<NonceState>` that serialises the atomic
// region [allocate nonce → sign → broadcast]. The lock is held across the
// entire async sign+broadcast path so no two in-flight submissions can ever
// share a nonce.  On any nonce-related error the state is re-synced from chain
// before the lock is released, keeping the counter accurate.
//
// Architecture note (Flashbots alto / Skandha reference):
//   Production bundlers use the same "pending nonce lock" pattern.  The
//   difference is they also track a per-sender UserOp nonce to deduplicate
//   replacement ops — that layer lives in the mempool (mempool.rs).

use std::sync::Arc;

use alloy::{
    primitives::Address,
    providers::Provider,
};
use eyre::{Result, WrapErr};
use tokio::sync::{Mutex, MutexGuard};
use tracing::{info, warn};

use super::service::HttpProvider;

// ─── Internal state ───────────────────────────────────────────────────────────

pub(super) struct NonceState {
    /// `None` until the first call to `lock()` syncs from chain.
    pub pending: Option<u64>,
}

// ─── Public API ───────────────────────────────────────────────────────────────

/// Thread-safe, serialising nonce allocator for the bundler signer.
///
/// Cheap to clone — internals are behind `Arc`.
#[derive(Clone)]
pub struct NonceManager {
    provider: Arc<HttpProvider>,
    signer:   Address,
    /// The mutex guards the entire sign → broadcast window.
    pub(super) state: Arc<Mutex<NonceState>>,
}

impl NonceManager {
    pub fn new(provider: Arc<HttpProvider>, signer: Address) -> Self {
        Self {
            provider,
            signer,
            state: Arc::new(Mutex::new(NonceState { pending: None })),
        }
    }

    /// Acquire the nonce lock and return the next nonce to use.
    ///
    /// **The caller MUST hold the returned `NonceGuard` until the L1
    /// transaction has been broadcast (or definitively failed).**
    /// Dropping the guard releases the lock and lets the next op proceed.
    ///
    /// The guard exposes `commit()` (success path — advances the counter)
    /// and `resync()` (error path — re-fetches from chain).
    pub async fn lock(&self) -> Result<NonceGuard<'_>> {
        let mut guard = self.state.lock().await;

        if guard.pending.is_none() {
            let n = self.fetch().await?;
            info!("NonceManager: initialised signer nonce at {n}");
            guard.pending = Some(n);
        }

        let nonce = guard.pending.unwrap();
        Ok(NonceGuard {
            nonce,
            guard,
            provider:   self.provider.clone(),
            signer:     self.signer,
        })
    }

    /// Force a re-sync from chain (useful after a failed broadcast where the
    /// nonce guard has already been dropped).
    pub async fn resync(&self) -> Result<()> {
        let n = self.fetch().await?;
        warn!("NonceManager: re-synced signer nonce to {n}");
        self.state.lock().await.pending = Some(n);
        Ok(())
    }

    async fn fetch(&self) -> Result<u64> {
        self.provider
            .get_transaction_count(self.signer)
            .await
            .wrap_err("get_transaction_count failed")
    }
}

// ─── Guard ────────────────────────────────────────────────────────────────────

/// Holds the nonce mutex lock across the sign → broadcast window.
///
/// Call `commit()` after a successful broadcast to optimistically advance the
/// local counter.  Call `resync()` after a nonce-related failure to re-fetch
/// the authoritative value from the RPC node.  The lock is released when this
/// value is dropped regardless.
pub struct NonceGuard<'a> {
    pub nonce: u64,
    guard:     MutexGuard<'a, NonceState>,
    provider:  Arc<HttpProvider>,
    signer:    Address,
}

impl<'a> NonceGuard<'a> {
    /// Advance the local counter after a successful `send_raw_transaction`.
    pub fn commit(&mut self) {
        self.guard.pending = Some(self.nonce + 1);
    }

    /// Re-fetch the authoritative nonce after a nonce-related error.
    /// The updated value takes effect for the **next** reservation.
    pub async fn resync(&mut self) {
        match self.provider.get_transaction_count(self.signer).await {
            Ok(n) => {
                warn!("NonceManager: re-synced (in guard) to {n}");
                self.guard.pending = Some(n);
            }
            Err(e) => warn!("NonceManager: resync failed: {e}"),
        }
    }
}
