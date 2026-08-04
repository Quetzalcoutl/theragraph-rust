// ─── ERC-4337 JSON-RPC Handler ────────────────────────────────────────────────

use alloy::primitives::{Address, B256};
use serde_json::{json, Value};
use tracing::{info, error};

use super::{
    state::BundlerState,
    types::{
        Call, JsonRpcRequest, JsonRpcResponse, PackedUserOperation, SerializedUserOp,
    },
};

pub async fn handle_rpc(state: &BundlerState, req: JsonRpcRequest) -> JsonRpcResponse {
    let id = req.id.clone();

    macro_rules! bail {
        ($code:expr, $msg:expr) => {
            return JsonRpcResponse::err(id, $code, $msg)
        };
    }

    match req.method.as_str() {
        "eth_supportedEntryPoints" => {
            JsonRpcResponse::ok(id, json!([format!("{:#x}", state.config.entry_point)]))
        }

        "eth_chainId" => {
            JsonRpcResponse::ok(id, json!(format!("{:#x}", state.config.chain_id)))
        }

        "eth_sendUserOperation" => {
            let (raw_op, ep): (SerializedUserOp, String) =
                match parse_two_params(&req.params) {
                    Ok(v) => v,
                    Err(e) => bail!(-32602, format!("Invalid params: {e}")),
                };

            if ep.to_lowercase() != format!("{:#x}", state.config.entry_point).to_lowercase() {
                bail!(-32602, "Unsupported EntryPoint");
            }

            if raw_op.signature.is_empty() {
                bail!(-32602, "UserOp must be signed");
            }

            let user_op: PackedUserOperation = raw_op.into();
            let user_op_hash = super::hash::compute_user_op_hash(
                &user_op,
                &state.config.entry_point,
                state.config.chain_id,
            );

            info!("eth_sendUserOperation {:#x}", user_op_hash);

            match state.bundler.submit_user_op(user_op).await {
                Ok(tx_hash) => {
                    info!("→ tx {:#x}", tx_hash);
                    let receipt = super::types::Receipt {
                        tx_hash,
                        block_number: None,
                        success: None,
                    };
                    state.store.set(user_op_hash, &receipt).await;
                    JsonRpcResponse::ok(id, json!(format!("{:#x}", user_op_hash)))
                }
                Err(e) => {
                    error!("eth_sendUserOperation failed: {e}");
                    JsonRpcResponse::err(id, -32603, e.to_string())
                }
            }
        }

        "eth_estimateUserOperationGas" => {
            JsonRpcResponse::ok(
                id,
                json!({
                    "preVerificationGas":
                        format!("{:#x}", state.config.gas.pre_verification_gas),
                    "verificationGasLimit":
                        format!("{:#x}", state.config.gas.verification_gas_limit),
                    "callGasLimit":
                        format!("{:#x}", state.config.gas.call_gas_limit),
                    "paymasterVerificationGasLimit":
                        format!("{:#x}", state.config.gas.paymaster_verification_gas_limit),
                    "paymasterPostOpGasLimit":
                        format!("{:#x}", state.config.gas.paymaster_post_op_gas_limit),
                }),
            )
        }

        "eth_getUserOperationByHash" => {
            let hash: B256 = match parse_one_b256(&req.params) {
                Ok(h) => h,
                Err(e) => bail!(-32602, format!("Invalid params: {e}")),
            };

            match state.store.get(hash).await {
                Some(r) => JsonRpcResponse::ok(
                    id,
                    json!({
                        "userOpHash": format!("{:#x}", hash),
                        "transactionHash": format!("{:#x}", r.tx_hash),
                    }),
                ),
                None => JsonRpcResponse::ok(id, Value::Null),
            }
        }

        "eth_getUserOperationReceipt" => {
            let hash: B256 = match parse_one_b256(&req.params) {
                Ok(h) => h,
                Err(e) => bail!(-32602, format!("Invalid params: {e}")),
            };

            match state.store.get(hash).await {
                Some(r) => JsonRpcResponse::ok(
                    id,
                    json!({
                        "userOpHash":        format!("{:#x}", hash),
                        "transactionHash":   format!("{:#x}", r.tx_hash),
                        "success":           r.success.unwrap_or(true),
                        "blockNumber":       r.block_number,
                    }),
                ),
                None => JsonRpcResponse::ok(id, Value::Null),
            }
        }

        "thera_sponsorUserOperation" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Body {
                sender: Option<Address>,
                owner_address: Option<Address>,
                calls: Vec<Call>,
            }

            let first = match req.params.into_iter().next() {
                Some(v) => v,
                None    => bail!(-32602, "Missing params"),
            };
            let body: Body = match serde_json::from_value(first) {
                Ok(b)  => b,
                Err(e) => bail!(-32602, format!("Invalid params: {e}")),
            };

            let sender = match body.sender {
                Some(s) => s,
                None => match body.owner_address {
                    Some(owner) => match state.bundler.get_smart_account_address(owner).await {
                        Ok(addr) => addr,
                        Err(e)   => bail!(-32603, format!("Address lookup failed: {e}")),
                    },
                    None => bail!(-32602, "Missing sender or ownerAddress"),
                },
            };

            if state.reputation.is_throttled(sender) {
                return JsonRpcResponse::err(id, -32600, "Sender temporarily throttled");
            }

            if body.calls.is_empty() {
                bail!(-32602, "calls must not be empty");
            }

            match state
                .bundler
                .build_sponsored_user_op(sender, &body.calls, body.owner_address, &state.op_nonce_mgr)
                .await
            {
                Ok((user_op, hash)) => {
                    let serialized = SerializedUserOp::from(&user_op);
                    JsonRpcResponse::ok(
                        id,
                        json!({
                            "userOp": serialized,
                            "userOpHash": format!("{:#x}", hash),
                        }),
                    )
                }
                Err(e) => {
                    error!("thera_sponsorUserOperation failed: {e}");
                    JsonRpcResponse::err(id, -32603, e.to_string())
                }
            }
        }

        unknown => JsonRpcResponse::err(id, -32601, format!("Method not found: {unknown}")),
    }
}

fn parse_two_params<A, B>(params: &[Value]) -> eyre::Result<(A, B)>
where
    A: serde::de::DeserializeOwned,
    B: serde::de::DeserializeOwned,
{
    let a = params.first().ok_or_else(|| eyre::eyre!("missing param[0]"))?;
    let b = params.get(1).ok_or_else(|| eyre::eyre!("missing param[1]"))?;
    Ok((
        serde_json::from_value(a.clone())?,
        serde_json::from_value(b.clone())?,
    ))
}

fn parse_one_b256(params: &[Value]) -> eyre::Result<B256> {
    let raw: String = serde_json::from_value(
        params.first().ok_or_else(|| eyre::eyre!("missing param[0]"))?.clone(),
    )?;
    let stripped = raw
        .strip_prefix("0x")
        .or_else(|| raw.strip_prefix("0X"))
        .unwrap_or(&raw);
    let bytes = hex::decode(stripped)?;
    if bytes.len() != 32 {
        eyre::bail!("Expected 32-byte hash, got {} bytes", bytes.len());
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes);
    Ok(B256::from(arr))
}
