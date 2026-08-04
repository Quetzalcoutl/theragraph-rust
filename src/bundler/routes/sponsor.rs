// ─── POST /bundler/api/sponsor ─────────────────────────────────────────────────

use axum::{extract::State, http::StatusCode, Json};
use serde_json::{json, Value};
use tracing::{error, info, warn};

use crate::bundler::{
    state::BundlerState,
    types::{SerializedUserOp, SponsorRequest},
};

/// Maximum calls per sponsored UserOp — prevents gas exhaustion.
const MAX_CALLS: usize = 50;
/// Maximum bytes per call's data field — prevents paymaster treasury drain.
const MAX_CALL_DATA: usize = 65_536;

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
                    error!("[sponsor] address lookup failed: {e}");
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(json!({ "error": "Invalid owner address or account not found" })),
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

    if body.calls.len() > MAX_CALLS {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": format!("Too many calls (max {MAX_CALLS})") })),
        );
    }

    for call in &body.calls {
        if call.data.len() > MAX_CALL_DATA {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": format!("call.data exceeds maximum of {MAX_CALL_DATA} bytes") })),
            );
        }
    }

    // ── ERC-7562 Reputation check ─────────────────────────────────────────
    // Block senders whose on-chain failure rate exceeds the configured threshold.
    // This is the primary defence against "Simulation Collision" treasury drain:
    // the check happens before signing, so a throttled sender costs us nothing.
    if state.reputation.is_throttled(sender) {
        warn!(
            sender = %sender,
            failures = state.reputation.failure_count(sender),
            "Rejected /sponsor for throttled sender"
        );
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({
                "error": "Sender temporarily throttled due to repeated on-chain execution failures. Try again later."
            })),
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
                Json(json!({ "error": "Failed to build sponsored UserOp" })),
            )
        }
    }
}
