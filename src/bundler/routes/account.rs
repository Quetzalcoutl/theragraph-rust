// ─── GET  /bundler/api/account/:owner ─────────────────────────────────────────
// ─── POST /bundler/api/account/:owner/fund-upgrade ────────────────────────────

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use alloy::{primitives::Address, providers::Provider};
use serde_json::{json, Value};
use tracing::info;

use crate::bundler::state::BundlerState;

/// GET /bundler/api/account/:owner
pub async fn get_account(
    State(state): State<BundlerState>,
    Path(owner_str): Path<String>,
) -> (StatusCode, Json<Value>) {
    let owner: Address = match owner_str.parse() {
        Ok(a) => a,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "Invalid owner address" })),
            )
        }
    };

    match state.bundler.get_smart_account_address(owner).await {
        Ok(smart_account) => {
            let deployed = state
                .bundler
                .is_deployed(smart_account)
                .await
                .unwrap_or(false);

            (
                StatusCode::OK,
                Json(json!({
                    "owner":        format!("{:#x}", owner),
                    "smartAccount": format!("{:#x}", smart_account),
                    "deployed":     deployed,
                })),
            )
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        ),
    }
}

/// POST /bundler/api/account/:owner/fund-upgrade
pub async fn fund_upgrade(
    State(state): State<BundlerState>,
    Path(owner_str): Path<String>,
) -> (StatusCode, Json<Value>) {
    let owner: Address = match owner_str.parse() {
        Ok(a) => a,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "Invalid owner address" })),
            )
        }
    };

    let new_impl = match state.config.account_impl_v2 {
        Some(addr) => addr,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "funded": false,
                    "message": "ACCOUNT_IMPL_V2 not configured"
                })),
            )
        }
    };

    let smart_account = match state.bundler.get_smart_account_address(owner).await {
        Ok(a) => a,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": e.to_string() })),
            )
        }
    };

    let deployed = state.bundler.is_deployed(smart_account).await.unwrap_or(false);
    if !deployed {
        return (
            StatusCode::OK,
            Json(json!({
                "funded": false,
                "smartAccount": format!("{:#x}", smart_account),
                "message": "Smart account not yet deployed — upgrade not needed"
            })),
        );
    }

    const UPGRADE_AMOUNT_WEI: u128 = 2_000_000_000_000_000;

    let balance = match state.bundler.provider().get_balance(owner).await {
        Ok(b) => b,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": e.to_string() })),
            )
        }
    };

    if balance >= alloy::primitives::U256::from(UPGRADE_AMOUNT_WEI) {
        return (
            StatusCode::OK,
            Json(json!({
                "funded": false,
                "smartAccount": format!("{:#x}", smart_account),
                "newImpl": format!("{:#x}", new_impl),
                "message": format!("Owner already has {} wei — sufficient for upgrade", balance),
                "ownerBalance": balance.to_string(),
            })),
        );
    }

    info!("Funding {owner:#x} with 0.002 ETH for account upgrade…");

    match state.bundler.send_eth(owner, alloy::primitives::U256::from(UPGRADE_AMOUNT_WEI)).await {
        Ok(tx_hash) => {
            info!("Funded {owner:#x}. Tx: {tx_hash:#x}");
            (
                StatusCode::OK,
                Json(json!({
                    "funded": true,
                    "smartAccount": format!("{:#x}", smart_account),
                    "newImpl": format!("{:#x}", new_impl),
                    "fundTx": format!("{:#x}", tx_hash),
                    "message": "Owner EOA funded with 0.002 ETH. Call upgradeToAndCall(newImpl, \"0x\") from the owner EOA."
                })),
            )
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        ),
    }
}
