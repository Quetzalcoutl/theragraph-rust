// ─── POST /bundler/api/submit ──────────────────────────────────────────────────
//
// Flow:
//   1. Structural validation: signature must be present.
//   2. Push onto the mempool channel and await the batch result.
//      Per-op eth_call simulation is intentionally NOT done here because it
//      would fail with AA25 for any op whose nonce is ahead of the current
//      on-chain value (which is the normal case for ops 2-N from a rapid
//      sequence of actions).  Instead, the batch processor simulates the
//      assembled batch atomically before broadcasting, and quarantines any
//      truly invalid op (wrong signature, bad paymaster, etc.) without
//      affecting the rest of the batch.
//   3. Return the tx_hash once the batch has been broadcast.
//
// API contract is unchanged: response contains `{ userOpHash, txHash }`.

use axum::{extract::State, http::StatusCode, Json};
use serde_json::{json, Value};
use tracing::{error, info};

use crate::bundler::{
    hash::compute_user_op_hash,
    state::BundlerState,
    types::{PackedUserOperation, Receipt, SubmitRequest},
};

pub async fn handler(
    State(state): State<BundlerState>,
    Json(body): Json<SubmitRequest>,
) -> (StatusCode, Json<Value>) {
    let raw = body.user_op;

    if raw.signature.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "UserOp must have a signature" })),
        );
    }

    // Per-field size caps — prevent oversized UserOps from consuming bundler resources.
    const MAX_INIT_CODE: usize = 131_072;       // 128 KiB
    const MAX_CALL_DATA: usize = 131_072;       // 128 KiB
    const MAX_SIGNATURE: usize = 4_096;         // 4 KiB
    const MAX_PAYMASTER_AND_DATA: usize = 4_096;

    if raw.init_code.len() > MAX_INIT_CODE {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "initCode exceeds 128 KiB" })));
    }
    if raw.call_data.len() > MAX_CALL_DATA {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "callData exceeds 128 KiB" })));
    }
    if raw.signature.len() > MAX_SIGNATURE {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "signature exceeds 4 KiB" })));
    }
    if raw.paymaster_and_data.len() > MAX_PAYMASTER_AND_DATA {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "paymasterAndData exceeds 4 KiB" })));
    }

    let user_op: PackedUserOperation = raw.into();
    let user_op_hash = compute_user_op_hash(
        &user_op,
        &state.config.entry_point,
        state.config.chain_id,
    );

    info!("Submitting {:#x}", user_op_hash);

    // ── Enqueue → await batch result (simulation happens inside batch processor) ──
    match state.mempool.push(user_op).await {
        Ok(tx_hash) => {
            info!("→ tx {:#x}", tx_hash);
            let receipt = Receipt { tx_hash, block_number: None, success: None };
            state.store.set(user_op_hash, &receipt).await;

            (
                StatusCode::OK,
                Json(json!({
                    "userOpHash": format!("{:#x}", user_op_hash),
                    "txHash":     format!("{:#x}", tx_hash),
                })),
            )
        }
        Err(e) => {
            error!("[submit] {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": e.to_string() })),
            )
        }
    }
}
