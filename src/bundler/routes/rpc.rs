// ─── POST /bundler/rpc ────────────────────────────────────────────────────────

use axum::{extract::State, http::StatusCode, Json};
use serde_json::Value;
use tracing::debug;

use crate::bundler::{
    rpc::handle_rpc,
    state::BundlerState,
    types::{JsonRpcRequest, JsonRpcResponse},
};

pub async fn handler(
    State(state): State<BundlerState>,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    if let Some(arr) = body.as_array() {
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
                JsonRpcResponse::err(None, -32603, format!("Internal error: {e}"))
            });
            responses.push(serde_json::to_value(resp).unwrap_or(Value::Null));
        }

        return (StatusCode::OK, Json(Value::Array(responses)));
    }

    let req: JsonRpcRequest = match serde_json::from_value(body) {
        Ok(r)  => r,
        Err(e) => {
            let resp = JsonRpcResponse::err(None, -32700, format!("Parse error: {e}"));
            return (StatusCode::BAD_REQUEST, Json(serde_json::to_value(resp).unwrap()));
        }
    };

    debug!("RPC method={}", req.method);
    let resp = handle_rpc(&state, req).await;
    let status = if resp.error.is_none() { StatusCode::OK } else { StatusCode::OK };
    (status, Json(serde_json::to_value(resp).unwrap_or(Value::Null)))
}
