// ─── ERC-4337 v0.7 Domain Types ───────────────────────────────────────────────

use alloy::primitives::{Address, Bytes, FixedBytes, B256, U256};
use serde::{Deserialize, Serialize};

// ─── Runtime types (used internally) ─────────────────────────────────────────

/// ERC-4337 v0.7 packed user operation (in-memory representation).
#[derive(Debug, Clone)]
pub struct PackedUserOperation {
    pub sender: Address,
    pub nonce: U256,
    pub init_code: Bytes,
    pub call_data: Bytes,
    pub account_gas_limits: FixedBytes<32>,
    pub pre_verification_gas: U256,
    pub gas_fees: FixedBytes<32>,
    pub paymaster_and_data: Bytes,
    pub signature: Bytes,
}

/// A single call to execute through the smart account.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Call {
    pub target: Address,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<HexU256>,
    pub data: Bytes,
}

// ─── Wire types (JSON API) ────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SerializedUserOp {
    pub sender: Address,
    pub nonce: HexU256,
    pub init_code: Bytes,
    pub call_data: Bytes,
    pub account_gas_limits: FixedBytes<32>,
    pub pre_verification_gas: HexU256,
    pub gas_fees: FixedBytes<32>,
    pub paymaster_and_data: Bytes,
    pub signature: Bytes,
}

// ─── Request / Response types ─────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SponsorRequest {
    pub sender: Option<Address>,
    pub owner_address: Option<Address>,
    pub calls: Vec<Call>,
}

#[allow(dead_code)]
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SponsorResponse {
    pub user_op: SerializedUserOp,
    pub user_op_hash: B256,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmitRequest {
    pub user_op: SerializedUserOp,
}

#[allow(dead_code)]
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmitResponse {
    pub user_op_hash: B256,
    pub tx_hash: B256,
}

#[allow(dead_code)]
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountResponse {
    pub owner: Address,
    pub smart_account: Address,
    pub deployed: bool,
}

// ─── Receipt store ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Receipt {
    pub tx_hash: B256,
    pub block_number: Option<u64>,
    pub success: Option<bool>,
}

// ─── JSON-RPC ─────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct JsonRpcRequest {
    #[allow(dead_code)]
    pub jsonrpc: String,
    pub id: Option<serde_json::Value>,
    pub method: String,
    #[serde(default)]
    pub params: Vec<serde_json::Value>,
}

#[derive(Debug, Serialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

impl JsonRpcResponse {
    pub fn ok(id: Option<serde_json::Value>, result: serde_json::Value) -> Self {
        Self { jsonrpc: "2.0", id, result: Some(result), error: None }
    }

    pub fn err(id: Option<serde_json::Value>, code: i64, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(JsonRpcError { code, message: message.into(), data: None }),
        }
    }
}

// ─── Round-trip conversions ───────────────────────────────────────────────────

impl From<&PackedUserOperation> for SerializedUserOp {
    fn from(op: &PackedUserOperation) -> Self {
        Self {
            sender: op.sender,
            nonce: HexU256(op.nonce),
            init_code: op.init_code.clone(),
            call_data: op.call_data.clone(),
            account_gas_limits: op.account_gas_limits,
            pre_verification_gas: HexU256(op.pre_verification_gas),
            gas_fees: op.gas_fees,
            paymaster_and_data: op.paymaster_and_data.clone(),
            signature: op.signature.clone(),
        }
    }
}

impl From<SerializedUserOp> for PackedUserOperation {
    fn from(s: SerializedUserOp) -> Self {
        Self {
            sender: s.sender,
            nonce: s.nonce.0,
            init_code: s.init_code,
            call_data: s.call_data,
            account_gas_limits: s.account_gas_limits,
            pre_verification_gas: s.pre_verification_gas.0,
            gas_fees: s.gas_fees,
            paymaster_and_data: s.paymaster_and_data,
            signature: s.signature,
        }
    }
}

// ─── HexU256 newtype ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HexU256(pub U256);

impl Serialize for HexU256 {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        ser.serialize_str(&format!("{:#x}", self.0))
    }
}

impl<'de> Deserialize<'de> for HexU256 {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        let s = String::deserialize(de)?;
        let stripped = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")).unwrap_or(&s);
        let n = U256::from_str_radix(stripped, 16).map_err(serde::de::Error::custom)?;
        Ok(HexU256(n))
    }
}
