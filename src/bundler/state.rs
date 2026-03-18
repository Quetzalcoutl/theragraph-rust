// ─── Bundler Application State ─────────────────────────────────────────────────

use std::sync::Arc;

use super::{
    account_nonce::UserOpNonceManager,
    config::Config,
    mempool::Mempool,
    service::BundlerService,
    store::ReceiptStore,
};

#[derive(Clone)]
pub struct BundlerState {
    pub config:       Arc<Config>,
    pub bundler:      Arc<BundlerService>,
    pub store:        Arc<ReceiptStore>,
    /// Hot path for incoming UserOps: push here instead of calling
    /// `bundler.submit_user_op` directly.  The batch processor amortises
    /// L1 costs and eliminates bundler EOA nonce races.
    pub mempool:      Mempool,
    /// Per-sender ERC-4337 account nonce tracker.  Ensures concurrent
    /// /sponsor requests for the same sender receive sequential nonces
    /// instead of all sampling the same on-chain value.
    pub op_nonce_mgr: UserOpNonceManager,
}
