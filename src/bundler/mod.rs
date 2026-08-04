// ─── ERC-4337 Bundler Module ──────────────────────────────────────────────────
//
// Unified into TheraGraph Engine for single-binary performance.
// Handles:
//   • UserOp sponsorship (paymaster signing)
//   • UserOp simulation + submission (handleOps on EntryPoint)
//   • Smart-account address lookup
//   • JSON-RPC ERC-4337 endpoint
//   • Receipt storage (Redis / in-memory DashMap)
//
// Architecture note (Ruben Koch / Quantstamp best practices):
//   The bundler is encapsulated as a self-contained module with its own
//   configuration, state, and route handlers.  This isolation ensures
//   the indexer and bundler cannot interfere with each other while sharing
//   the same Tokio runtime and Axum server for zero-overhead integration.

pub mod account_nonce;
pub mod config;
pub mod contracts;
pub mod error;
pub mod gas;
pub mod hash;
pub mod mempool;
pub mod nonce;
pub mod paymaster;
pub mod reputation;
pub mod rpc;
pub mod routes;
pub mod service;
pub mod state;
pub mod store;
pub mod types;

use axum::{
    routing::{get, post},
    Router,
};
use tower_http::limit::RequestBodyLimitLayer;
use std::sync::Arc;
use tracing::{info, warn};

pub use service::BundlerService;
pub use state::BundlerState;

/// Initialize the bundler and return a ready-to-merge `Router<()>`.
///
/// Returns `None` if bundler env vars (PRIVATE_KEY, etc.) are not configured.
/// The caller can safely skip bundler routes in that case.
pub async fn init() -> Option<Router> {
    let cfg = match config::Config::from_env() {
        Ok(c) => c,
        Err(e) => {
            warn!("⚠️  Bundler not configured: {e}. Bundler endpoints will be disabled.");
            return None;
        }
    };
    let cfg = Arc::new(cfg);

    let bundler = match BundlerService::new(cfg.clone()) {
        Ok(b) => Arc::new(b),
        Err(e) => {
            warn!("⚠️  Bundler init failed: {e}. Bundler endpoints will be disabled.");
            return None;
        }
    };

    let receipt_store = match store::ReceiptStore::from_config(cfg.redis_url.as_deref()).await {
        Ok(s) => Arc::new(s),
        Err(e) => {
            warn!("⚠️  Bundler receipt store init failed: {e}. Bundler endpoints will be disabled.");
            return None;
        }
    };

    // Verify paymaster config in background (non-blocking)
    {
        let b = bundler.clone();
        tokio::spawn(async move {
            b.verify_paymaster_config().await;
        });
    }

    // ── Batch processor setup ─────────────────────────────────────────────
    // The mempool serialises all incoming UserOps into a bounded mpsc channel.
    // The background processor drains it in batches of up to MAX_BATCH_SIZE,
    // submitting one handleOps tx per batch and routing results back via
    // per-op oneshot channels.
    let (mempool, mempool_rx) = mempool::Mempool::new();
    let reputation = reputation::SenderReputation::new();
    {
        let b = bundler.clone();
        let rep = reputation.clone();
        mempool::spawn_batch_processor(mempool_rx, b, rep);
    }
    info!("Batch processor spawned");

    // Per-sender ERC-4337 account nonce manager.
    let op_nonce_mgr = account_nonce::UserOpNonceManager::new(cfg.entry_point);

    let bundler_state = BundlerState {
        config: cfg.clone(),
        bundler,
        store: receipt_store,
        mempool,
        op_nonce_mgr,
        reputation,
    };

    info!("═══════════════════════════════════════════════════════════════");
    info!("  🌿 ERC-4337 Bundler Module Active");
    info!("    Chain:      {}", cfg.chain_id);
    info!("    EntryPoint: {:#x}", cfg.entry_point);
    info!("    Paymaster:  {:#x}", cfg.paymaster);
    info!("    Factory:    {:#x}", cfg.factory);
    info!("═══════════════════════════════════════════════════════════════");

    let router = Router::new()
        .route("/", get(bundler_root))
        .route("/health", get(routes::health::handler))
        .route("/rpc", post(routes::rpc::handler))
        .route("/api/sponsor", post(routes::sponsor::handler))
        .route("/api/submit", post(routes::submit::handler))
        .route("/api/account/:owner", get(routes::account::get_account))
        .route(
            "/api/account/:owner/fund-upgrade",
            post(routes::account::fund_upgrade),
        )
        .with_state(bundler_state)
        // 1 MiB body limit — matches thera-bundler-rust; prevents OOM via
        // unbounded JSON-RPC payloads before the MAX_BATCH check fires.
        .layer(RequestBodyLimitLayer::new(1 * 1024 * 1024));

    Some(router)
}

/// GET /bundler/ — service info
async fn bundler_root() -> axum::Json<serde_json::Value> {
    axum::Json(serde_json::json!({
        "name":    "thera-bundler",
        "version": env!("CARGO_PKG_VERSION"),
        "endpoints": [
            "GET  /bundler/health",
            "POST /bundler/rpc",
            "POST /bundler/api/sponsor",
            "POST /bundler/api/submit",
            "GET  /bundler/api/account/:owner",
            "POST /bundler/api/account/:owner/fund-upgrade",
        ]
    }))
}
