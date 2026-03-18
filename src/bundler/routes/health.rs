// ─── GET /bundler/health ──────────────────────────────────────────────────────

use axum::{extract::State, http::StatusCode, Json};
use serde_json::{json, Value};

use crate::bundler::state::BundlerState;

pub async fn handler(State(state): State<BundlerState>) -> (StatusCode, Json<Value>) {
    match state.bundler.health_stats().await {
        Ok((block, deposit, active)) => (
            StatusCode::OK,
            Json(json!({
                "status":             "ok",
                "version":            env!("CARGO_PKG_VERSION"),
                "chain":              state.config.chain_id,
                "blockNumber":        block,
                "entryPoint":         format!("{:#x}", state.config.entry_point),
                "paymaster":          format!("{:#x}", state.config.paymaster),
                "factory":            format!("{:#x}", state.config.factory),
                "paymasterDepositWei": deposit.to_string(),
                "sponsorshipActive":   active,
            })),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "status": "error", "error": e.to_string() })),
        ),
    }
}
