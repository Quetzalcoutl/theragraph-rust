// ─── POST /bundler/rpc ────────────────────────────────────────────────────────

use axum::{extract::State, http::StatusCode, Json};
use serde_json::Value;
use tracing::{debug, error};

use crate::bundler::{
    rpc::handle_rpc,
    state::BundlerState,
    types::{JsonRpcRequest, JsonRpcResponse},
};

/// Maximum number of JSON-RPC requests allowed in a single batch.
const MAX_BATCH: usize = 5;

pub async fn handler(
    State(state): State<BundlerState>,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    if let Some(arr) = body.as_array() {
        if arr.len() > MAX_BATCH {
            let resp = JsonRpcResponse::err(None, -32600, format!("Batch too large (max {MAX_BATCH})"));
            return (StatusCode::BAD_REQUEST, Json(serde_json::to_value(resp).unwrap_or_else(|_| serde_json::json!({"jsonrpc":"2.0","id":null,"error":{"code":-32603,"message":"Internal error"}}))));
        }
        let futs: Vec<_> = arr
            .iter()
            .map(|v| {
                let state  = state.clone();
                let v      = v.clone();
                tokio::spawn(async move {
                    match serde_json::from_value::<JsonRpcRequest>(v) {
                        Ok(req)  => handle_rpc(&state, req).await,
                        Err(e)   => JsonRpcResponse::err(
                            None,
                            -32700,
                            format!("Parse error: {e}"),
                        ),
                    }
                })
            })
            .collect();

        let mut responses = Vec::with_capacity(futs.len());
        for fut in futs {
            let resp = fut.await.unwrap_or_else(|e| {
                error!("[rpc] batch task failed: {e}");
                JsonRpcResponse::err(None, -32603, "Internal server error")
            });
            responses.push(serde_json::to_value(resp).unwrap_or_else(|e| { tracing::error!("[rpc] serialize failed: {e}"); serde_json::json!({"jsonrpc":"2.0","id":null,"error":{"code":-32603,"message":"Internal error"}}) }));
        }

        return (StatusCode::OK, Json(Value::Array(responses)));
    }

    let req: JsonRpcRequest = match serde_json::from_value(body) {
        Ok(r)  => r,
        Err(e) => {
            let resp = JsonRpcResponse::err(None, -32700, format!("Parse error: {e}"));
            return (StatusCode::BAD_REQUEST, Json(serde_json::to_value(resp).unwrap_or_else(|e| { tracing::error!("[rpc] serialize failed: {e}"); serde_json::json!({"jsonrpc":"2.0","id":null,"error":{"code":-32603,"message":"Internal error"}}) })));
        }
    };

    debug!("RPC method={}", req.method);
    let resp = handle_rpc(&state, req).await;
    let status = StatusCode::OK; // JSON-RPC 2.0: always 200; errors are in the response body
    (status, Json(serde_json::to_value(resp).unwrap_or_else(|e| { tracing::error!("[rpc] serialize failed: {e}"); serde_json::json!({"jsonrpc":"2.0","id":null,"error":{"code":-32603,"message":"Internal error"}}) })))
}
