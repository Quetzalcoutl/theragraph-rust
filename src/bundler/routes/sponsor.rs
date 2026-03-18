// ─── POST /bundler/api/sponsor ─────────────────────────────────────────────────

use axum::{extract::State, http::StatusCode, Json};
use serde_json::{json, Value};
use tracing::{error, info};

use crate::bundler::{
    state::BundlerState,
    types::{SerializedUserOp, SponsorRequest},
};

pub async fn handler(
    State(state): State<BundlerState>,
    Json(body): Json<SponsorRequest>,
) -> (StatusCode, Json<Value>) {
    let sender = match body.sender {
        Some(s) => s,
        None => match body.owner_address {
            Some(owner) => match state.bundler.get_smart_account_address(owner).await {
                Ok(addr) => addr,
                Err(e)   => {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(json!({ "error": format!("Address lookup failed: {e}") })),
                    )
                }
            },
            None => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "error": "Provide sender or ownerAddress" })),
                )
            }
        },
    };

    if body.calls.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "calls array is required" })),
        );
    }

    info!("Sponsoring UserOp for {sender:#x}");

    match state
        .bundler
        .build_sponsored_user_op(sender, &body.calls, body.owner_address, &state.op_nonce_mgr)
        .await
    {
        Ok((user_op, hash)) => {
            let serialized = SerializedUserOp::from(&user_op);
            (
                StatusCode::OK,
                Json(json!({
                    "userOp":     serialized,
                    "userOpHash": format!("{:#x}", hash),
                })),
            )
        }
        Err(e) => {
            error!("[sponsor] {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": e.to_string() })),
            )
        }
    }
}
